use anyhow::{anyhow, Context, Result};
use common::{EventSource, IngressEvent, RawTransactionEnvelope};
use config::FlashblocksConfig;
use ethers::providers::{Ipc, Middleware, Provider};
use ethers::types::{Address, Bytes, H256, U256};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{str::FromStr, sync::Arc};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, info, warn};

pub struct ChainClient {
    provider: Arc<Provider<Ipc>>,
}

impl ChainClient {
    pub async fn connect(ipc_path: &str) -> Result<Self> {
        let provider = Provider::<Ipc>::connect_ipc(ipc_path)
            .await
            .with_context(|| format!("failed to connect to local IPC at {ipc_path}"))?;
        Ok(Self {
            provider: Arc::new(provider),
        })
    }

    pub fn provider(&self) -> Arc<Provider<Ipc>> {
        Arc::clone(&self.provider)
    }

    pub async fn health_check(&self) -> Result<u64> {
        let block_number = self
            .provider
            .get_block_number()
            .await
            .context("failed to fetch the latest Base block number")?;
        Ok(block_number.as_u64())
    }
}

#[derive(Clone)]
pub struct FlashblocksClient {
    config: FlashblocksConfig,
}

impl FlashblocksClient {
    pub fn new(config: FlashblocksConfig) -> Self {
        Self { config }
    }

    pub fn spawn(self, sender: mpsc::Sender<IngressEvent>) -> JoinHandle<Result<()>> {
        tokio::spawn(async move { self.run(sender).await })
    }

    async fn run(self, sender: mpsc::Sender<IngressEvent>) -> Result<()> {
        let (stream, _) = connect_async(&self.config.ws_url)
            .await
            .with_context(|| format!("failed to connect to {}", self.config.ws_url))?;
        info!("connected to Flashblocks endpoint {}", self.config.ws_url);

        let (mut write, mut read) = stream.split();

        if self.config.subscribe_transactions {
            write
                .send(Message::Text(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1u64,
                        "method": "eth_subscribe",
                        "params": ["newFlashblockTransactions"]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .context("failed to subscribe to newFlashblockTransactions")?;
        }

        if self.config.subscribe_pending_logs {
            write
                .send(Message::Text(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 2u64,
                        "method": "eth_subscribe",
                        "params": ["pendingLogs", {}]
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .context("failed to subscribe to pendingLogs")?;
        }

        while let Some(message) = read.next().await {
            let message = message.context("failed to read Flashblocks frame")?;

            match message {
                Message::Text(text) => {
                    let value: Value = serde_json::from_str(&text)
                        .with_context(|| format!("invalid Flashblocks JSON payload: {text}"))?;
                    if let Some(event) = normalize_flashblocks_message(value) {
                        if sender.send(event).await.is_err() {
                            return Err(anyhow!("failed to forward Flashblocks ingress event"));
                        }
                    }
                }
                Message::Ping(payload) => {
                    write.send(Message::Pong(payload)).await?;
                }
                Message::Pong(_) => {
                    if sender
                        .send(IngressEvent::Heartbeat {
                            source: EventSource::Flashblocks,
                        })
                        .await
                        .is_err()
                    {
                        return Err(anyhow!("failed to forward Flashblocks heartbeat"));
                    }
                }
                Message::Close(frame) => {
                    warn!("Flashblocks websocket closed: {frame:?}");
                    break;
                }
                Message::Binary(_) | Message::Frame(_) => {
                    debug!("ignored non-text Flashblocks frame");
                }
            }
        }

        Ok(())
    }
}

fn normalize_flashblocks_message(value: Value) -> Option<IngressEvent> {
    let result = value.pointer("/params/result").cloned().unwrap_or_else(|| value.clone());

    if let Some(envelope) = parse_transaction_envelope(&result, value.clone()) {
        return Some(IngressEvent::Transaction(envelope));
    }

    if result.get("topics").is_some() || result.get("address").is_some() {
        return Some(IngressEvent::Log {
            source: EventSource::Flashblocks,
            payload: value,
        });
    }

    None
}

fn parse_transaction_envelope(result: &Value, raw_payload: Value) -> Option<RawTransactionEnvelope> {
    let candidate = result
        .get("tx")
        .cloned()
        .unwrap_or_else(|| result.clone());

    let tx_hash = parse_hash(find_first_string(
        &candidate,
        &["hash", "txHash", "transactionHash"],
    )?)?;
    let from = parse_address(find_first_string(&candidate, &["from"])?)?;
    let to = find_first_string(&candidate, &["to"]).and_then(parse_address);
    let input = parse_bytes(
        find_first_string(&candidate, &["input", "data"]).unwrap_or("0x"),
    )?;
    let value = find_first_string(&candidate, &["value"])
        .and_then(parse_u256)
        .unwrap_or_else(U256::zero);
    let block_number = find_first_string(&candidate, &["blockNumber", "block_number"])
        .and_then(parse_u64_quantity);

    Some(RawTransactionEnvelope {
        source: EventSource::Flashblocks,
        tx_hash,
        from,
        to,
        input,
        value,
        block_number,
        raw_payload,
    })
}

fn find_first_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    for key in keys {
        if let Some(candidate) = value.get(*key).and_then(Value::as_str) {
            return Some(candidate);
        }
    }
    None
}

fn parse_hash(value: &str) -> Option<H256> {
    H256::from_str(value).ok()
}

fn parse_address(value: &str) -> Option<Address> {
    Address::from_str(value).ok()
}

fn parse_bytes(value: &str) -> Option<Bytes> {
    Bytes::from_str(value).ok()
}

fn parse_u256(value: &str) -> Option<U256> {
    if let Some(stripped) = value.strip_prefix("0x") {
        U256::from_str_radix(stripped, 16).ok()
    } else {
        U256::from_dec_str(value).ok()
    }
}

fn parse_u64_quantity(value: &str) -> Option<u64> {
    if let Some(stripped) = value.strip_prefix("0x") {
        u64::from_str_radix(stripped, 16).ok()
    } else {
        value.parse::<u64>().ok()
    }
}

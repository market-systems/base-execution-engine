use anyhow::{anyhow, Context, Result};
use common::{EventSource, IngressEvent, RawTransactionEnvelope};
use config::{BaseNetworkConfig, ChainTransportKind, FlashblocksConfig};
use ethers::{
    middleware::SignerMiddleware,
    providers::{Http, Ipc, Middleware, Provider, Ws},
    signers::{LocalWallet, Signer},
    types::{
        transaction::eip2718::TypedTransaction, Address, BlockId, Bytes, H256, Transaction,
        TransactionReceipt, U256,
    },
};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{str::FromStr, sync::Arc};
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use tracing::{debug, info, warn};

#[derive(Debug, Clone)]
enum ReadProvider {
    Ipc(Arc<Provider<Ipc>>),
    Http(Arc<Provider<Http>>),
    Ws(Arc<Provider<Ws>>),
}

#[derive(Debug, Clone)]
enum WriteProvider {
    Ipc(Arc<SignerMiddleware<Provider<Ipc>, LocalWallet>>),
    Http(Arc<SignerMiddleware<Provider<Http>, LocalWallet>>),
    Ws(Arc<SignerMiddleware<Provider<Ws>, LocalWallet>>),
}

#[derive(Debug, Clone)]
pub struct ChainClient {
    transport: ChainTransportKind,
    reader: ReadProvider,
    writer: Option<WriteProvider>,
}

impl ChainClient {
    pub async fn connect(config: &BaseNetworkConfig) -> Result<Self> {
        match Self::resolve_transport(config)? {
            ChainTransportKind::Ipc => {
                let ipc_path = config
                    .ipc_path
                    .as_deref()
                    .ok_or_else(|| anyhow!("BASE_IPC_PATH must be set for ipc transport"))?;
                let provider = Provider::<Ipc>::connect_ipc(ipc_path)
                    .await
                    .with_context(|| format!("failed to connect to local IPC at {ipc_path}"))?;
                Ok(Self {
                    transport: ChainTransportKind::Ipc,
                    reader: ReadProvider::Ipc(Arc::new(provider)),
                    writer: None,
                })
            }
            ChainTransportKind::Http => {
                let rpc_url = config
                    .rpc_url
                    .as_deref()
                    .ok_or_else(|| anyhow!("BASE_RPC_URL must be set for http transport"))?;
                let provider = Provider::<Http>::try_from(rpc_url)
                    .with_context(|| format!("failed to connect to HTTP RPC at {rpc_url}"))?;
                Ok(Self {
                    transport: ChainTransportKind::Http,
                    reader: ReadProvider::Http(Arc::new(provider)),
                    writer: None,
                })
            }
            ChainTransportKind::Ws => {
                let ws_url = config
                    .ws_url
                    .as_deref()
                    .ok_or_else(|| anyhow!("BASE_WS_URL must be set for ws transport"))?;
                let provider = Provider::<Ws>::connect(ws_url)
                    .await
                    .with_context(|| format!("failed to connect to websocket RPC at {ws_url}"))?;
                Ok(Self {
                    transport: ChainTransportKind::Ws,
                    reader: ReadProvider::Ws(Arc::new(provider)),
                    writer: None,
                })
            }
            ChainTransportKind::Auto => Err(anyhow!("auto transport should be resolved before connect")),
        }
    }

    pub fn with_signer(&self, executor_private_key: &str, chain_id: u64) -> Result<Self> {
        let wallet = executor_private_key
            .parse::<LocalWallet>()
            .context("failed to parse EXECUTOR_PRIVATE_KEY")?
            .with_chain_id(chain_id);

        let writer = match &self.reader {
            ReadProvider::Ipc(provider) => {
                WriteProvider::Ipc(Arc::new(SignerMiddleware::new((**provider).clone(), wallet)))
            }
            ReadProvider::Http(provider) => {
                WriteProvider::Http(Arc::new(SignerMiddleware::new((**provider).clone(), wallet)))
            }
            ReadProvider::Ws(provider) => {
                WriteProvider::Ws(Arc::new(SignerMiddleware::new((**provider).clone(), wallet)))
            }
        };

        Ok(Self {
            transport: self.transport,
            reader: self.reader.clone(),
            writer: Some(writer),
        })
    }

    pub fn transport(&self) -> ChainTransportKind {
        self.transport
    }

    pub fn signer_address(&self) -> Option<Address> {
        match &self.writer {
            Some(WriteProvider::Ipc(client)) => Some(client.address()),
            Some(WriteProvider::Http(client)) => Some(client.address()),
            Some(WriteProvider::Ws(client)) => Some(client.address()),
            None => None,
        }
    }

    pub async fn health_check(&self) -> Result<u64> {
        Ok(self.get_block_number().await?.as_u64())
    }

    pub async fn get_block_number(&self) -> Result<ethers::types::U64> {
        match &self.reader {
            ReadProvider::Ipc(provider) => provider
                .get_block_number()
                .await
                .context("failed to fetch the latest Base block number"),
            ReadProvider::Http(provider) => provider
                .get_block_number()
                .await
                .context("failed to fetch the latest Base block number"),
            ReadProvider::Ws(provider) => provider
                .get_block_number()
                .await
                .context("failed to fetch the latest Base block number"),
        }
    }

    pub async fn call(&self, tx: &TypedTransaction) -> Result<Bytes> {
        match &self.reader {
            ReadProvider::Ipc(provider) => provider.call(tx, None).await.context("eth_call failed"),
            ReadProvider::Http(provider) => provider.call(tx, None).await.context("eth_call failed"),
            ReadProvider::Ws(provider) => provider.call(tx, None).await.context("eth_call failed"),
        }
    }

    pub async fn get_transaction(&self, tx_hash: H256) -> Result<Option<Transaction>> {
        match &self.reader {
            ReadProvider::Ipc(provider) => provider
                .get_transaction(tx_hash)
                .await
                .with_context(|| format!("failed to fetch transaction {tx_hash:?}")),
            ReadProvider::Http(provider) => provider
                .get_transaction(tx_hash)
                .await
                .with_context(|| format!("failed to fetch transaction {tx_hash:?}")),
            ReadProvider::Ws(provider) => provider
                .get_transaction(tx_hash)
                .await
                .with_context(|| format!("failed to fetch transaction {tx_hash:?}")),
        }
    }

    pub async fn get_transaction_receipt(
        &self,
        tx_hash: H256,
    ) -> Result<Option<TransactionReceipt>> {
        match &self.reader {
            ReadProvider::Ipc(provider) => provider
                .get_transaction_receipt(tx_hash)
                .await
                .with_context(|| format!("failed to fetch receipt for {tx_hash:?}")),
            ReadProvider::Http(provider) => provider
                .get_transaction_receipt(tx_hash)
                .await
                .with_context(|| format!("failed to fetch receipt for {tx_hash:?}")),
            ReadProvider::Ws(provider) => provider
                .get_transaction_receipt(tx_hash)
                .await
                .with_context(|| format!("failed to fetch receipt for {tx_hash:?}")),
        }
    }

    pub async fn get_transaction_count(
        &self,
        address: Address,
        block: Option<BlockId>,
    ) -> Result<U256> {
        match &self.reader {
            ReadProvider::Ipc(provider) => provider
                .get_transaction_count(address, block)
                .await
                .with_context(|| format!("failed to fetch nonce for {address:?}")),
            ReadProvider::Http(provider) => provider
                .get_transaction_count(address, block)
                .await
                .with_context(|| format!("failed to fetch nonce for {address:?}")),
            ReadProvider::Ws(provider) => provider
                .get_transaction_count(address, block)
                .await
                .with_context(|| format!("failed to fetch nonce for {address:?}")),
        }
    }

    pub async fn get_gas_price(&self) -> Result<U256> {
        match &self.reader {
            ReadProvider::Ipc(provider) => provider
                .get_gas_price()
                .await
                .context("failed to fetch current gas price"),
            ReadProvider::Http(provider) => provider
                .get_gas_price()
                .await
                .context("failed to fetch current gas price"),
            ReadProvider::Ws(provider) => provider
                .get_gas_price()
                .await
                .context("failed to fetch current gas price"),
        }
    }

    pub async fn call_many(&self, txs: &[TypedTransaction]) -> Result<Vec<Bytes>> {
        let calls = txs
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to serialize transaction sequence for post-trigger simulation")?;

        let result = match &self.reader {
            ReadProvider::Ipc(provider) => request_call_many_ipc(provider.as_ref(), &calls).await,
            ReadProvider::Http(provider) => request_call_many_http(provider.as_ref(), &calls).await,
            ReadProvider::Ws(provider) => request_call_many_ws(provider.as_ref(), &calls).await,
        }?;

        parse_call_many_response(result)
    }

    pub async fn send_transaction(&self, tx: TypedTransaction) -> Result<H256> {
        match &self.writer {
            Some(WriteProvider::Ipc(client)) => Ok(client
                .send_transaction(tx, None)
                .await
                .context("failed to submit signed transaction")?
                .tx_hash()),
            Some(WriteProvider::Http(client)) => Ok(client
                .send_transaction(tx, None)
                .await
                .context("failed to submit signed transaction")?
                .tx_hash()),
            Some(WriteProvider::Ws(client)) => Ok(client
                .send_transaction(tx, None)
                .await
                .context("failed to submit signed transaction")?
                .tx_hash()),
            None => Err(anyhow!("chain client is not configured with a signer")),
        }
    }

    fn resolve_transport(config: &BaseNetworkConfig) -> Result<ChainTransportKind> {
        match config.transport {
            ChainTransportKind::Auto => {
                if config
                    .ipc_path
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .is_some()
                {
                    Ok(ChainTransportKind::Ipc)
                } else if config
                    .ws_url
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .is_some()
                {
                    Ok(ChainTransportKind::Ws)
                } else if config
                    .rpc_url
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .is_some()
                {
                    Ok(ChainTransportKind::Http)
                } else {
                    Err(anyhow!(
                        "BASE_TRANSPORT=auto requires at least one configured network endpoint"
                    ))
                }
            }
            transport => Ok(transport),
        }
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

async fn request_call_many_ipc(provider: &Provider<Ipc>, calls: &[Value]) -> Result<Value> {
    request_call_many_impl(provider, calls).await
}

async fn request_call_many_http(provider: &Provider<Http>, calls: &[Value]) -> Result<Value> {
    request_call_many_impl(provider, calls).await
}

async fn request_call_many_ws(provider: &Provider<Ws>, calls: &[Value]) -> Result<Value> {
    request_call_many_impl(provider, calls).await
}

async fn request_call_many_impl<P>(provider: &Provider<P>, calls: &[Value]) -> Result<Value>
where
    P: ethers::providers::JsonRpcClient,
{
    let attempts = [
        ("eth_callMany", json!([calls, "pending"])),
        (
            "eth_simulateV1",
            json!([{
                "blockStateCalls": [{
                    "calls": calls,
                }],
                "validation": true,
                "traceTransfers": false
            }]),
        ),
    ];

    let mut last_error = None;
    for (method, params) in attempts {
        match provider.request::<Value, Value>(method, params).await {
            Ok(value) => return Ok(value),
            Err(error) => last_error = Some(anyhow!("{method} failed: {error}")),
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow!("no supported post-trigger simulation RPC method")))
}

fn parse_call_many_response(value: Value) -> Result<Vec<Bytes>> {
    let calls = value
        .get("calls")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .ok_or_else(|| anyhow!("unexpected post-trigger simulation response shape"))?;

    let mut outputs = Vec::new();
    for entry in calls {
        if let Some(inner_calls) = entry.get("calls").and_then(Value::as_array) {
            for call in inner_calls {
                if let Some(bytes) = parse_call_output(call) {
                    outputs.push(bytes);
                }
            }
            continue;
        }

        if let Some(bytes) = parse_call_output(entry) {
            outputs.push(bytes);
        }
    }

    if outputs.is_empty() {
        return Err(anyhow!(
            "post-trigger simulation response did not contain callable outputs"
        ));
    }

    Ok(outputs)
}

fn parse_call_output(value: &Value) -> Option<Bytes> {
    if let Some(raw) = value.as_str() {
        return parse_bytes(raw);
    }

    for key in ["value", "output", "returnData"] {
        if let Some(raw) = value.get(key).and_then(Value::as_str) {
            return parse_bytes(raw);
        }
    }

    None
}

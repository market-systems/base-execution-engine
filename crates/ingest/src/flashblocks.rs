//! Base Flashblocks pre-confirm WS connector.
//!
//! Flashblocks deliver ~5 sub-blocks per Base block (~200ms cadence) ahead of
//! the canonical block being sealed. Each sub-block is published as a single
//! gzip-compressed JSON message on a dedicated WS endpoint
//! (`wss://mainnet-preconf.base.org` for Base mainnet).
//!
//! Wire format (`FlashblocksPayloadV1`, see
//! <https://docs.base.org/base-chain/flashblocks/overview>):
//!
//! ```text
//! {
//!   "payload_id": "0x...",            // stable across the parent block
//!   "index": 0,                        // sub-block index, 0..N
//!   "base": { ... },                   // parent block header (only on index 0)
//!   "diff": {
//!     "block_hash": "0x...",
//!     "state_root": "0x...",
//!     "gas_used": "0x...",
//!     "transactions": ["0x02f8...", ...] // RLP-encoded raw signed txs
//!   },
//!   "metadata": {
//!     "block_number": 12345,
//!     "receipts": {
//!       "0xtxhash": { "<TxType>": { "logs": [...], ... } }
//!     }
//!   }
//! }
//! ```
//!
//! What this connector does:
//!
//! 1. Opens the WS, ping-pongs as required, decompresses each binary frame.
//! 2. Parses into a permissive [`FlashblockPayload`].
//! 3. Fans the receipt logs out as `Event::Log` events with
//!    `BlockContext::Pending`, channel = `FlashblocksWs`. This feeds the
//!    existing PoolBook / decision pipeline with sub-second pool state
//!    updates without any downstream code changes.
//! 4. Emits one `Event::Flashblock` summary per sub-block for observability
//!    (tx_hashes, log_count, block_hash).
//!
//! What this connector does **not** do (intentional, for now):
//!
//! - Does not decode raw RLP transactions into `Event::Transaction`. That
//!   requires signer recovery + ABI decoding, which is heavy and only useful
//!   for the orderflow side of the engine. The pool-state / log path is the
//!   one that drives arbitrage decisions, and that path is fully covered by
//!   the receipt log fan-out.

use crate::error::IngestError;
use crate::raw::RawLogMessage;
use crate::stream::{
    current_time_ms, decode_status, emit_heartbeat, payload_size_bytes, raw_summary, send_event,
    EventSender, IngestStream, IngestStreamContext, RuntimeEventSender, StreamRuntime,
    StreamSubscription,
};
use async_trait::async_trait;
use flate2::read::GzDecoder;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message},
};
use types::ingest::{
    BlockContext, Channel, Event, Flashblock, Metadata, RawMessageType, StreamStatus,
};
use types::{BlockHash, ChainId, TxHash};

/// Permissive on-the-wire shape of a Flashblocks payload. Every field is
/// optional because the upstream protocol is still evolving and we want a
/// missing field to degrade to a partial event rather than crash the stream.
#[derive(Debug, Clone, Deserialize)]
struct FlashblockPayload {
    #[serde(default)]
    payload_id: Option<String>,
    #[serde(default)]
    index: u64,
    #[serde(default)]
    base: Option<FlashblockBase>,
    #[serde(default)]
    diff: Option<FlashblockDiff>,
    #[serde(default)]
    metadata: Option<FlashblockMetadata>,
}

#[derive(Debug, Clone, Deserialize)]
struct FlashblockBase {
    #[serde(default)]
    parent_hash: Option<String>,
    #[serde(default)]
    block_number: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FlashblockDiff {
    #[serde(default)]
    block_hash: Option<String>,
    #[serde(default)]
    state_root: Option<String>,
    #[serde(default)]
    gas_used: Option<String>,
    /// RLP-encoded signed transactions. Carried through as opaque hex strings.
    /// Currently not surfaced — a future revision will RLP-decode + recover
    /// the signer to emit `Event::Transaction` entries with
    /// `BlockContext::Pending`. See module docs.
    #[serde(default)]
    #[allow(dead_code)]
    transactions: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct FlashblockMetadata {
    #[serde(default)]
    block_number: Option<u64>,
    /// Map of `tx_hash -> receipt`. The receipt schema varies by tx type
    /// (Legacy / Eip1559 / Eip2930 / Deposit) and the Base flashblocks
    /// publisher wraps it in a single-key tagged object. We capture the inner
    /// `logs` array via a recursive Value walk.
    #[serde(default)]
    receipts: serde_json::Map<String, Value>,
}

pub struct FlashblocksStream {
    context: IngestStreamContext,
    runtime: StreamRuntime,
    url: String,
}

impl FlashblocksStream {
    pub fn new(context: IngestStreamContext, url: String) -> Self {
        Self {
            runtime: StreamRuntime::new("flashblocks_stream", StreamSubscription::Logs),
            context,
            url,
        }
    }

    pub fn chain_id(&self) -> ChainId {
        self.context.chain_id
    }

    async fn run_once(
        &mut self,
        sender: &EventSender,
        runtime_sender: &RuntimeEventSender,
    ) -> Result<(), IngestError> {
        self.runtime
            .transition(
                &self.context,
                runtime_sender,
                StreamStatus::Connecting,
                None,
            )
            .await?;

        let (mut ws, _resp) = connect_async(&self.url).await.map_err(ws_to_ingest)?;

        self.runtime
            .transition(
                &self.context,
                runtime_sender,
                StreamStatus::Running,
                Some(format!("flashblocks={}", self.url)),
            )
            .await?;

        let timeout_window = Duration::from_secs(self.context.heartbeat_timeout_secs);
        let mut idle_intervals = 0_usize;

        loop {
            let next = timeout(timeout_window, ws.next()).await;

            let frame = match next {
                Ok(Some(Ok(frame))) => {
                    idle_intervals = 0;
                    frame
                }
                Ok(Some(Err(err))) => return Err(ws_to_ingest(err)),
                Ok(None) => {
                    return Err(IngestError::StreamFailure {
                        stream_name: self.name(),
                        message: "flashblocks websocket closed by peer".to_string(),
                    });
                }
                Err(_) => {
                    idle_intervals += 1;
                    emit_heartbeat(
                        runtime_sender,
                        &self.context,
                        Some(format!(
                            "{} idle for {}s",
                            self.name(),
                            self.context.heartbeat_timeout_secs
                        )),
                    )
                    .await?;

                    if idle_intervals >= 2 {
                        return Err(IngestError::StreamFailure {
                            stream_name: self.name(),
                            message: "flashblocks heartbeat timeout".to_string(),
                        });
                    }

                    continue;
                }
            };

            let bytes = match frame {
                Message::Binary(bytes) => bytes,
                Message::Text(text) => text.into_bytes(),
                Message::Ping(payload) => {
                    ws.send(Message::Pong(payload)).await.map_err(ws_to_ingest)?;
                    continue;
                }
                Message::Pong(_) | Message::Frame(_) => continue,
                Message::Close(frame) => {
                    return Err(IngestError::StreamFailure {
                        stream_name: self.name(),
                        message: format!("flashblocks ws closed: {frame:?}"),
                    });
                }
            };

            let payload = decode_payload(&bytes).map_err(|err| IngestError::InvalidPayload {
                stream_name: self.name(),
                message: format!("flashblocks payload decode failed: {err}"),
            })?;

            self.dispatch(sender, payload, bytes.len()).await?;
        }
    }

    /// Fan the parsed payload out as one `Event::Flashblock` summary plus
    /// one `Event::Log` per receipt log.
    async fn dispatch(
        &self,
        sender: &EventSender,
        payload: FlashblockPayload,
        raw_size: usize,
    ) -> Result<(), IngestError> {
        let (block_number, parent_hash, block_hash, state_root, gas_used, tx_hashes, logs) =
            extract_views(&payload);

        let block_context = BlockContext::Pending;

        // Fan out logs first so consumers see them ahead of the summary that
        // closes the sub-block.
        let mut emitted_logs = 0_usize;
        for log_value in &logs {
            let Some(raw) = RawLogMessage::from_value(log_value) else {
                continue;
            };
            let decoded = raw.decode();
            let metadata = Metadata {
                block_context: block_context.clone(),
                channel: self.channel(),
                observed_at_ms: current_time_ms(),
                chain_id: self.chain_id(),
                tx_hash: raw.tx_hash.clone(),
                decode_status: decode_status(
                    decoded.protocol,
                    decoded.exchange,
                    decoded.decoded_event.is_some()
                        || decoded.event_signature.is_some()
                        || raw.address.is_some(),
                ),
                raw: raw_summary(
                    RawMessageType::Log,
                    payload_size_bytes(log_value).unwrap_or(0),
                    raw.tx_hash.clone().or(decoded.event_signature.clone()),
                    self.subscription(),
                ),
            };
            let log_event = raw.to_log(metadata);
            observability::record_ingest_event(
                self.name(),
                crate::stream::channel_label(self.channel()),
                crate::stream::decode_status_label(log_event.metadata.decode_status),
            );
            send_event(sender, Event::Log(log_event)).await?;
            emitted_logs += 1;
        }

        let metadata = Metadata {
            block_context,
            channel: self.channel(),
            observed_at_ms: current_time_ms(),
            chain_id: self.chain_id(),
            tx_hash: None,
            decode_status: types::ingest::DecodeStatus::Decoded,
            raw: raw_summary(
                RawMessageType::Flashblock,
                raw_size,
                payload.payload_id.clone(),
                self.subscription(),
            ),
        };

        let summary = Flashblock {
            metadata,
            payload_id: payload.payload_id,
            index: payload.index,
            parent_block_number: block_number,
            parent_hash,
            block_hash,
            state_root,
            gas_used,
            tx_hashes,
            log_count: emitted_logs,
        };

        observability::record_ingest_event(
            self.name(),
            crate::stream::channel_label(self.channel()),
            crate::stream::decode_status_label(summary.metadata.decode_status),
        );
        send_event(sender, Event::Flashblock(summary)).await
    }
}

#[async_trait]
impl IngestStream for FlashblocksStream {
    fn name(&self) -> &'static str {
        self.runtime.name
    }

    fn channel(&self) -> Channel {
        Channel::FlashblocksWs
    }

    fn status(&self) -> StreamStatus {
        self.runtime.status
    }

    fn subscription(&self) -> StreamSubscription {
        // Flashblocks is conceptually a logs stream (driving PoolBook updates).
        self.runtime.subscription
    }

    async fn run(
        &mut self,
        sender: EventSender,
        runtime_sender: RuntimeEventSender,
    ) -> Result<(), IngestError> {
        self.runtime.ensure_not_stopped()?;
        let mut backoff_ms = self.context.reconnect_initial_ms;

        loop {
            match self.run_once(&sender, &runtime_sender).await {
                Ok(()) => {
                    self.runtime
                        .transition(
                            &self.context,
                            &runtime_sender,
                            StreamStatus::Stopped,
                            Some("flashblocks stream completed".to_string()),
                        )
                        .await?;
                    return Ok(());
                }
                Err(error) => {
                    let reason = error.to_string();
                    self.runtime
                        .transition(
                            &self.context,
                            &runtime_sender,
                            StreamStatus::Backoff,
                            Some(reason.clone()),
                        )
                        .await?;
                    sleep(Duration::from_millis(backoff_ms)).await;
                    self.runtime
                        .transition(
                            &self.context,
                            &runtime_sender,
                            StreamStatus::Reconnecting,
                            Some(reason),
                        )
                        .await?;
                    backoff_ms = backoff_ms
                        .saturating_mul(2)
                        .min(self.context.reconnect_max_ms);
                }
            }
        }
    }
}

// ---- payload decoding helpers ----

/// Decode a raw frame: try gzip first (the documented format), fall back to
/// raw JSON for endpoints that disable compression.
fn decode_payload(bytes: &[u8]) -> Result<FlashblockPayload, String> {
    let json: Value = if looks_gzipped(bytes) {
        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = Vec::with_capacity(bytes.len() * 4);
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| format!("gzip decompress failed: {e}"))?;
        serde_json::from_slice(&decompressed).map_err(|e| format!("json parse failed: {e}"))?
    } else {
        serde_json::from_slice(bytes).map_err(|e| format!("json parse failed: {e}"))?
    };

    serde_json::from_value(json).map_err(|e| format!("payload schema mismatch: {e}"))
}

/// Cheap gzip sniff via the standard 0x1f 0x8b magic prefix. Avoids paying
/// for a failed decompress when an endpoint is publishing plain JSON.
fn looks_gzipped(bytes: &[u8]) -> bool {
    bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b
}

#[allow(clippy::type_complexity)]
fn extract_views(
    payload: &FlashblockPayload,
) -> (
    Option<u64>,
    Option<BlockHash>,
    Option<BlockHash>,
    Option<String>,
    Option<u64>,
    Vec<TxHash>,
    Vec<Value>,
) {
    let parent_block_number = payload
        .metadata
        .as_ref()
        .and_then(|m| m.block_number)
        .or_else(|| {
            payload
                .base
                .as_ref()
                .and_then(|b| b.block_number.as_deref())
                .and_then(parse_hex_u64)
        });

    let parent_hash = payload
        .base
        .as_ref()
        .and_then(|b| b.parent_hash.clone());

    let (block_hash, state_root, gas_used) = match payload.diff.as_ref() {
        Some(diff) => (
            diff.block_hash.clone(),
            diff.state_root.clone(),
            diff.gas_used.as_deref().and_then(parse_hex_u64),
        ),
        None => (None, None, None),
    };

    let tx_hashes = payload
        .metadata
        .as_ref()
        .map(|m| m.receipts.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();

    let logs = payload
        .metadata
        .as_ref()
        .map(|m| collect_logs(&m.receipts))
        .unwrap_or_default();

    (
        parent_block_number,
        parent_hash,
        block_hash,
        state_root,
        gas_used,
        tx_hashes,
        logs,
    )
}

/// Walk every receipt object and collect its `logs` array entries verbatim
/// as `serde_json::Value`s so the existing `RawLogMessage::from_value`
/// decoder can consume them unchanged. We additionally inject the receipt's
/// owning tx hash + the parent block number (when known) onto each log so
/// downstream consumers see the same metadata they would on a confirmed log.
fn collect_logs(receipts: &serde_json::Map<String, Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for (tx_hash, receipt) in receipts.iter() {
        let inner = unwrap_receipt(receipt);
        let Some(logs) = inner.get("logs").and_then(Value::as_array) else {
            continue;
        };
        for log in logs {
            let Value::Object(map) = log else { continue };
            let mut enriched = map.clone();
            enriched
                .entry("transactionHash".to_string())
                .or_insert_with(|| Value::String(tx_hash.clone()));
            out.push(Value::Object(enriched));
        }
    }
    out
}

/// Receipts are serialised as a single-key tagged enum
/// (`{"Eip1559": { ... }}`, `{"Legacy": { ... }}`, ...). Unwrap the inner
/// map; if the receipt is already a flat object (Deposit txs sometimes are),
/// return it as-is.
fn unwrap_receipt(receipt: &Value) -> &Value {
    if let Some(obj) = receipt.as_object() {
        if obj.len() == 1 {
            if let Some(inner) = obj.values().next() {
                if inner.is_object() {
                    return inner;
                }
            }
        }
    }
    receipt
}

fn parse_hex_u64(input: &str) -> Option<u64> {
    let trimmed = input.strip_prefix("0x").unwrap_or(input);
    u64::from_str_radix(trimmed, 16).ok()
}

fn ws_to_ingest(err: WsError) -> IngestError {
    IngestError::StreamFailure {
        stream_name: "flashblocks_stream",
        message: format!("websocket error: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn decodes_plain_json_payload() {
        let payload = json!({
            "payload_id": "0xabcd",
            "index": 2,
            "base": { "parent_hash": "0x11", "block_number": "0x10" },
            "diff": { "block_hash": "0x22", "state_root": "0x33", "gas_used": "0x4d2" },
            "metadata": {
                "block_number": 16,
                "receipts": {
                    "0xtx1": {
                        "Eip1559": {
                            "logs": [
                                { "address": "0xpool", "topics": ["0xsig"], "data": "0xdead" }
                            ]
                        }
                    },
                    "0xtx2": {
                        "Deposit": {
                            "logs": []
                        }
                    }
                }
            }
        });

        let bytes = serde_json::to_vec(&payload).unwrap();
        let decoded = decode_payload(&bytes).unwrap();

        assert_eq!(decoded.index, 2);
        assert_eq!(decoded.payload_id.as_deref(), Some("0xabcd"));

        let (block_number, parent_hash, block_hash, state_root, gas_used, tx_hashes, logs) =
            extract_views(&decoded);
        assert_eq!(block_number, Some(16));
        assert_eq!(parent_hash.as_deref(), Some("0x11"));
        assert_eq!(block_hash.as_deref(), Some("0x22"));
        assert_eq!(state_root.as_deref(), Some("0x33"));
        assert_eq!(gas_used, Some(0x4d2));
        assert_eq!(tx_hashes.len(), 2);
        assert_eq!(logs.len(), 1);
        // Connector enriched the log with its owning tx hash.
        assert_eq!(
            logs[0].get("transactionHash").and_then(Value::as_str),
            Some("0xtx1")
        );
    }

    #[test]
    fn decodes_gzip_wrapped_payload() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let payload = json!({
            "payload_id": "0xfeed",
            "index": 0,
            "diff": { "block_hash": "0xbb" },
            "metadata": { "block_number": 7, "receipts": {} }
        });
        let raw = serde_json::to_vec(&payload).unwrap();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&raw).unwrap();
        let gz = encoder.finish().unwrap();
        assert!(looks_gzipped(&gz));

        let decoded = decode_payload(&gz).unwrap();
        let (block_number, _, block_hash, _, _, _, logs) = extract_views(&decoded);
        assert_eq!(block_number, Some(7));
        assert_eq!(block_hash.as_deref(), Some("0xbb"));
        assert!(logs.is_empty());
    }

    #[test]
    fn unwrap_receipt_handles_tagged_and_flat() {
        let tagged = json!({ "Eip1559": { "logs": [] } });
        assert!(unwrap_receipt(&tagged).get("logs").is_some());

        let flat = json!({ "logs": [] });
        assert!(unwrap_receipt(&flat).get("logs").is_some());
    }

    #[test]
    fn rejects_non_object_payload() {
        let bytes = b"\"not an object\"";
        let err = decode_payload(bytes).unwrap_err();
        assert!(err.contains("schema mismatch") || err.contains("json parse"));
    }
}

use crate::{
    Address, BlockHash, BlockNumber, ChainId, Selector, Topic, TxHash, UnixTimestampMillis,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockContext {
    /// Preconfirmed context from a Flashblock before the final block is sealed.
    /// See: https://docs.base.org/base-chain/flashblocks/overview
    Pending,
    Block {
        number: BlockNumber,
        hash: Option<BlockHash>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Ipc,
    Ws,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeStatus {
    Decoded,
    Partial,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Unknown,
    UniswapV2,
    UniswapV3,
    UniswapV4,
    Aerodrome,
    Virtuals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exchange {
    Unknown,
    PancakeSwap,
    BaseSwap,
    AlienBase,
    Aerodrome,
    Virtuals,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    Idle,
    Connecting,
    Subscribing,
    Running,
    Backoff,
    Reconnecting,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RawMessageType {
    Transaction,
    Log,
    Block,
    Heartbeat,
    Status,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawPayloadSummary {
    pub message_type: RawMessageType,
    pub payload_size_bytes: usize,
    pub fingerprint: Option<String>,
    pub subscription: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Metadata {
    pub block_context: BlockContext,
    pub channel: Channel,
    pub observed_at_ms: UnixTimestampMillis,
    pub chain_id: ChainId,
    pub tx_hash: Option<TxHash>,
    pub decode_status: DecodeStatus,
    pub raw: RawPayloadSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transaction {
    pub metadata: Metadata,
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: Option<String>,
    pub input: Option<String>,
    pub selector: Option<Selector>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Log {
    pub metadata: Metadata,
    pub address: Option<Address>,
    pub topics: Vec<Topic>,
    pub data: Option<String>,
    pub event_signature: Option<String>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub log_index: Option<u64>,
    pub removed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub metadata: Metadata,
    pub block_hash: Option<BlockHash>,
    pub parent_hash: Option<BlockHash>,
    pub timestamp_secs: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// Runtime heartbeat emitted by ingest to show the stream is still alive.
    pub block_context: BlockContext,
    pub channel: Channel,
    pub observed_at_ms: UnixTimestampMillis,
    pub chain_id: ChainId,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamStatusEvent {
    /// Runtime status change for an ingest stream, not a chain transaction/log/block event.
    pub block_context: BlockContext,
    pub channel: Channel,
    pub observed_at_ms: UnixTimestampMillis,
    pub chain_id: ChainId,
    pub previous: Option<StreamStatus>,
    pub current: StreamStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Transaction(Transaction),
    Log(Log),
    Block(Block),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// Internal ingest runtime signals used for health, lifecycle, and diagnostics.
    Heartbeat(Heartbeat),
    StreamStatus(StreamStatusEvent),
}

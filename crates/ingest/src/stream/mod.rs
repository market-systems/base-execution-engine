mod block;
mod log;
mod rpc;
mod transaction;

pub use block::BlockStream;
pub use log::LogStream;
pub use transaction::TransactionStream;

use crate::channel::IngestEndpoint;
use crate::error::IngestError;
use async_trait::async_trait;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use types::ingest::{
    Channel, DecodeStatus, Event, Heartbeat, RawMessageType, RawPayloadSummary, RuntimeEvent,
    StreamStatus, StreamStatusEvent,
};
use types::ChainId;

pub type EventSender = mpsc::Sender<Event>;
pub type RuntimeEventSender = mpsc::Sender<RuntimeEvent>;
pub type BoxedIngestStream = Box<dyn IngestStream>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSubscription {
    Transactions,
    Logs,
    Blocks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestStreamContext {
    pub chain_id: ChainId,
    pub endpoint: IngestEndpoint,
    pub reconnect_initial_ms: u64,
    pub reconnect_max_ms: u64,
    pub heartbeat_timeout_secs: u64,
}

impl IngestStreamContext {
    pub fn channel(&self) -> Channel {
        self.endpoint.channel()
    }

    pub fn endpoint(&self) -> &IngestEndpoint {
        &self.endpoint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamRuntime {
    pub(crate) name: &'static str,
    pub(crate) status: StreamStatus,
    pub(crate) subscription: StreamSubscription,
}

impl StreamRuntime {
    pub(crate) fn new(name: &'static str, subscription: StreamSubscription) -> Self {
        Self {
            name,
            status: StreamStatus::Idle,
            subscription,
        }
    }

    pub(crate) async fn transition(
        &mut self,
        context: &IngestStreamContext,
        sender: &RuntimeEventSender,
        next: StreamStatus,
        reason: Option<String>,
    ) -> Result<(), IngestError> {
        let previous = Some(self.status);
        self.status = next;

        sender
            .send(RuntimeEvent::StreamStatus(StreamStatusEvent {
                block_context: types::ingest::BlockContext::Pending,
                channel: context.channel(),
                observed_at_ms: current_time_ms(),
                chain_id: context.chain_id,
                previous,
                current: next,
                reason,
            }))
            .await
            .map_err(|_| IngestError::RuntimeEventDeliveryClosed)
    }

    pub(crate) fn ensure_not_stopped(&self) -> Result<(), IngestError> {
        if self.status == StreamStatus::Stopped {
            return Err(IngestError::InvalidStreamState {
                stream_name: self.name,
                message: "stopped stream cannot be started again".to_string(),
            });
        }

        Ok(())
    }
}

#[async_trait]
pub trait IngestStream: Send {
    fn name(&self) -> &'static str;
    fn channel(&self) -> Channel;
    fn status(&self) -> StreamStatus;
    fn subscription(&self) -> StreamSubscription;
    async fn run(
        &mut self,
        sender: EventSender,
        runtime_sender: RuntimeEventSender,
    ) -> Result<(), IngestError>;
}

pub(crate) async fn send_event(sender: &EventSender, event: Event) -> Result<(), IngestError> {
    sender
        .send(event)
        .await
        .map_err(|_| IngestError::EventDeliveryClosed)
}

pub(crate) async fn emit_heartbeat(
    sender: &RuntimeEventSender,
    context: &IngestStreamContext,
    message: Option<String>,
) -> Result<(), IngestError> {
    sender
        .send(RuntimeEvent::Heartbeat(Heartbeat {
            block_context: types::ingest::BlockContext::Pending,
            channel: context.channel(),
            observed_at_ms: current_time_ms(),
            chain_id: context.chain_id,
            message,
        }))
        .await
        .map_err(|_| IngestError::RuntimeEventDeliveryClosed)
}

pub(crate) fn decode_status(
    protocol: Option<types::ingest::Protocol>,
    exchange: Option<types::ingest::Exchange>,
    discriminant_present: bool,
) -> DecodeStatus {
    if protocol.is_some() || exchange.is_some() {
        DecodeStatus::Decoded
    } else if discriminant_present {
        DecodeStatus::Unsupported
    } else {
        DecodeStatus::Partial
    }
}

pub(crate) fn current_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn payload_size_bytes(value: &Value) -> Result<usize, IngestError> {
    Ok(serde_json::to_vec(value)?.len())
}

pub(crate) fn subscription_label(subscription: StreamSubscription) -> &'static str {
    match subscription {
        StreamSubscription::Transactions => "new_pending_transactions",
        StreamSubscription::Logs => "logs",
        StreamSubscription::Blocks => "new_heads",
    }
}

pub(crate) fn raw_summary(
    message_type: RawMessageType,
    payload_size_bytes: usize,
    fingerprint: Option<String>,
    subscription: StreamSubscription,
) -> RawPayloadSummary {
    RawPayloadSummary {
        message_type,
        payload_size_bytes,
        fingerprint,
        subscription: Some(subscription_label(subscription).to_string()),
    }
}

mod block;
mod log;
mod transaction;

pub use block::BlockStream;
pub use log::LogStream;
pub use transaction::TransactionStream;

use crate::error::IngestError;
use async_trait::async_trait;
use tokio::sync::mpsc;
use types::ingest::{Event, Channel, StreamStatus};
use types::ChainId;

pub type EventSender = mpsc::Sender<Event>;
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
    pub channel: Channel,
    pub reconnect_initial_ms: u64,
    pub reconnect_max_ms: u64,
    pub heartbeat_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StreamRuntime {
    pub(crate) name: &'static str,
    pub(crate) channel: Channel,
    pub(crate) status: StreamStatus,
    pub(crate) subscription: StreamSubscription,
}

impl StreamRuntime {
    pub(crate) fn new(
        name: &'static str,
        channel: Channel,
        subscription: StreamSubscription,
    ) -> Self {
        Self {
            name,
            channel,
            status: StreamStatus::Idle,
            subscription,
        }
    }

    pub(crate) fn set_status(&mut self, next: StreamStatus) {
        self.status = next;
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
    async fn run(&mut self, sender: EventSender) -> Result<(), IngestError>;
}

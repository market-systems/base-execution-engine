use crate::error::IngestError;
use crate::stream::{
    EventSender, IngestStream, IngestStreamContext, StreamRuntime, StreamSubscription,
};
use async_trait::async_trait;
use types::ingest::{Channel, StreamStatus};
use types::ChainId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockStream {
    context: IngestStreamContext,
    runtime: StreamRuntime,
}

impl BlockStream {
    pub fn new(context: IngestStreamContext) -> Self {
        Self {
            runtime: StreamRuntime::new(
                "block_stream",
                context.channel,
                StreamSubscription::Blocks,
            ),
            context,
        }
    }

    pub fn chain_id(&self) -> ChainId {
        self.context.chain_id
    }
}

#[async_trait]
impl IngestStream for BlockStream {
    fn name(&self) -> &'static str {
        self.runtime.name
    }

    fn channel(&self) -> Channel {
        self.runtime.channel
    }

    fn status(&self) -> StreamStatus {
        self.runtime.status
    }

    fn subscription(&self) -> StreamSubscription {
        self.runtime.subscription
    }

    async fn run(&mut self, _sender: EventSender) -> Result<(), IngestError> {
        self.runtime.ensure_not_stopped()?;
        self.runtime.set_status(StreamStatus::Connecting);
        self.runtime.set_status(StreamStatus::Subscribing);
        self.runtime.set_status(StreamStatus::Running);
        Ok(())
    }
}

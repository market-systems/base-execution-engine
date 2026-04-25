use crate::error::IngestError;
use crate::raw::RawBlockMessage;
use crate::stream::rpc::AlloyPubsubSession;
use crate::stream::{
    current_time_ms, emit_heartbeat, payload_size_bytes, raw_summary, send_event, EventSender,
    IngestStream, IngestStreamContext, RuntimeEventSender, StreamRuntime, StreamSubscription,
};
use async_trait::async_trait;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use types::ingest::{Channel, DecodeStatus, Event, Metadata, RawMessageType, StreamStatus};
use types::ChainId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockStream {
    context: IngestStreamContext,
    runtime: StreamRuntime,
}

impl BlockStream {
    pub fn new(context: IngestStreamContext) -> Self {
        Self {
            runtime: StreamRuntime::new("block_stream", StreamSubscription::Blocks),
            context,
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
        let session = AlloyPubsubSession::connect(self.context.endpoint(), self.name()).await?;

        self.runtime
            .transition(
                &self.context,
                runtime_sender,
                StreamStatus::Subscribing,
                None,
            )
            .await?;

        let mut handle = session.subscribe(self.subscription()).await?;
        let subscription_id = handle.id().to_string();

        self.runtime
            .transition(
                &self.context,
                runtime_sender,
                StreamStatus::Running,
                Some(format!("subscription={subscription_id}")),
            )
            .await?;

        let timeout_window = Duration::from_secs(self.context.heartbeat_timeout_secs);
        let mut idle_intervals = 0_usize;

        loop {
            let payload = match timeout(timeout_window, handle.next_payload(self.name()))
            .await
            {
                Ok(result) => {
                    idle_intervals = 0;
                    result?
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
                            message: "block subscription heartbeat timeout".to_string(),
                        });
                    }

                    continue;
                }
            };

            let raw = RawBlockMessage::from_value(&payload).ok_or_else(|| {
                IngestError::InvalidPayload {
                    stream_name: self.name(),
                    message: "block payload was not an object".to_string(),
                }
            })?;
            let metadata = Metadata {
                block_context: raw.block_context(),
                channel: self.channel(),
                observed_at_ms: current_time_ms(),
                chain_id: self.chain_id(),
                tx_hash: None,
                decode_status: DecodeStatus::Decoded,
                raw: raw_summary(
                    RawMessageType::Block,
                    payload_size_bytes(&payload)?,
                    raw.block_hash.clone(),
                    self.subscription(),
                ),
            };

            let block_event = raw.to_block(metadata);
            observability::record_ingest_event(
                self.name(),
                crate::stream::channel_label(self.channel()),
                crate::stream::decode_status_label(block_event.metadata.decode_status),
            );
            send_event(sender, Event::Block(block_event)).await?;
        }
    }
}

#[async_trait]
impl IngestStream for BlockStream {
    fn name(&self) -> &'static str {
        self.runtime.name
    }

    fn channel(&self) -> Channel {
        self.context.channel()
    }

    fn status(&self) -> StreamStatus {
        self.runtime.status
    }

    fn subscription(&self) -> StreamSubscription {
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
            self.runtime
                .transition(
                    &self.context,
                    &runtime_sender,
                    StreamStatus::Connecting,
                    None,
                )
                .await?;

            match self.run_once(&sender, &runtime_sender).await {
                Ok(()) => {
                    self.runtime
                        .transition(
                            &self.context,
                            &runtime_sender,
                            StreamStatus::Stopped,
                            Some("block stream completed".to_string()),
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

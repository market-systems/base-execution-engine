use crate::error::IngestError;
use crate::raw::RawTransactionMessage;
use crate::stream::rpc::JsonRpcSession;
use crate::stream::{
    current_time_ms, decode_status, emit_heartbeat, payload_size_bytes, raw_summary, send_event,
    EventSender, IngestStream, IngestStreamContext, RuntimeEventSender, StreamRuntime,
    StreamSubscription,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::{sleep, timeout};
use types::ingest::{Channel, Event, Metadata, RawMessageType, StreamStatus};
use types::ChainId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionStream {
    context: IngestStreamContext,
    runtime: StreamRuntime,
}

impl TransactionStream {
    pub fn new(context: IngestStreamContext) -> Self {
        Self {
            runtime: StreamRuntime::new("transaction_stream", StreamSubscription::Transactions),
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
        let mut session = JsonRpcSession::connect(self.context.endpoint(), self.name()).await?;

        self.runtime
            .transition(
                &self.context,
                runtime_sender,
                StreamStatus::Subscribing,
                None,
            )
            .await?;

        let subscription_id = session.subscribe(self.subscription()).await?;

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
            let payload = match timeout(
                timeout_window,
                session.next_subscription_payload(&subscription_id),
            )
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
                            message: "transaction subscription heartbeat timeout".to_string(),
                        });
                    }

                    continue;
                }
            };

            let materialized = self.materialize_transaction(&mut session, payload).await?;
            if materialized.is_null() {
                continue;
            }

            let raw = RawTransactionMessage::from_value(&materialized).ok_or_else(|| {
                IngestError::InvalidPayload {
                    stream_name: self.name(),
                    message: "transaction payload was not an object".to_string(),
                }
            })?;
            let decoded = raw.decode();
            let metadata = Metadata {
                block_context: raw.block_context(),
                channel: self.channel(),
                observed_at_ms: current_time_ms(),
                chain_id: self.chain_id(),
                tx_hash: raw.tx_hash.clone(),
                decode_status: decode_status(
                    decoded.protocol,
                    decoded.exchange,
                    decoded.selector.is_some() || raw.to.is_some(),
                ),
                raw: raw_summary(
                    RawMessageType::Transaction,
                    payload_size_bytes(&materialized)?,
                    raw.tx_hash.clone().or(decoded.selector.clone()),
                    self.subscription(),
                ),
            };

            send_event(sender, Event::Transaction(raw.to_transaction(metadata))).await?;
        }
    }

    async fn materialize_transaction(
        &self,
        session: &mut JsonRpcSession,
        payload: Value,
    ) -> Result<Value, IngestError> {
        let Some(tx_hash) = payload.as_str() else {
            return Ok(payload);
        };

        match timeout(
            Duration::from_secs(self.context.heartbeat_timeout_secs),
            session.request("eth_getTransactionByHash", json!([tx_hash])),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(IngestError::StreamFailure {
                stream_name: self.name(),
                message: "transaction materialization timed out".to_string(),
            }),
        }
    }
}

#[async_trait]
impl IngestStream for TransactionStream {
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
                            Some("transaction stream completed".to_string()),
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

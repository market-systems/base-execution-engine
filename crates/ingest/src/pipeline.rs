use crate::channel::{IngestEndpoint, IpcChannel, WsChannel};
use crate::error::IngestError;
use crate::stream::{
    BlockStream, BoxedIngestStream, EventSender, IngestStreamContext, LogStream,
    RuntimeEventSender, TransactionStream,
};
use config::IngestConfig;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use types::ingest::{Channel, Event, RuntimeEvent};

pub struct IngestRuntime {
    event_receiver: mpsc::Receiver<Event>,
    runtime_event_receiver: mpsc::Receiver<RuntimeEvent>,
    stream_tasks: Vec<JoinHandle<Result<(), IngestError>>>,
}

impl IngestRuntime {
    pub fn into_parts(
        self,
    ) -> (
        mpsc::Receiver<Event>,
        mpsc::Receiver<RuntimeEvent>,
        Vec<JoinHandle<Result<(), IngestError>>>,
    ) {
        (
            self.event_receiver,
            self.runtime_event_receiver,
            self.stream_tasks,
        )
    }

    pub fn event_receiver(&mut self) -> &mut mpsc::Receiver<Event> {
        &mut self.event_receiver
    }

    pub fn runtime_event_receiver(&mut self) -> &mut mpsc::Receiver<RuntimeEvent> {
        &mut self.runtime_event_receiver
    }

    pub fn abort_all(&mut self) {
        for task in &self.stream_tasks {
            task.abort();
        }
    }
}

pub struct IngestPipeline {
    streams: Vec<BoxedIngestStream>,
    event_channel_capacity: usize,
    runtime_channel_capacity: usize,
}

impl IngestPipeline {
    pub fn new(event_channel_capacity: usize, runtime_channel_capacity: usize) -> Self {
        Self {
            streams: Vec::new(),
            event_channel_capacity,
            runtime_channel_capacity,
        }
    }

    pub fn from_config(config: &IngestConfig) -> Result<Self, IngestError> {
        let endpoint = Self::selected_endpoint(config)?;
        let context = IngestStreamContext {
            chain_id: config.chain_id,
            endpoint,
            reconnect_initial_ms: config.reconnect_initial_ms,
            reconnect_max_ms: config.reconnect_max_ms,
            heartbeat_timeout_secs: config.heartbeat_timeout_secs,
        };

        let mut pipeline = Self::new(
            config.event_channel_capacity,
            config.runtime_channel_capacity,
        );

        if config.subscribe_transactions {
            pipeline.add_stream(Box::new(TransactionStream::new(context.clone())));
        }

        if config.subscribe_logs {
            pipeline.add_stream(Box::new(LogStream::new(context.clone())));
        }

        if config.subscribe_blocks {
            pipeline.add_stream(Box::new(BlockStream::new(context)));
        }

        Ok(pipeline)
    }

    pub fn add_stream(&mut self, stream: BoxedIngestStream) {
        self.streams.push(stream);
    }

    pub fn stream_count(&self) -> usize {
        self.streams.len()
    }

    pub fn streams(&self) -> &[BoxedIngestStream] {
        &self.streams
    }

    pub fn spawn(self) -> IngestRuntime {
        let (event_sender, event_receiver): (EventSender, mpsc::Receiver<Event>) =
            mpsc::channel(self.event_channel_capacity);
        let (runtime_event_sender, runtime_event_receiver): (
            RuntimeEventSender,
            mpsc::Receiver<RuntimeEvent>,
        ) = mpsc::channel(self.runtime_channel_capacity);

        let stream_tasks = self
            .streams
            .into_iter()
            .map(|mut stream| {
                let event_sender = event_sender.clone();
                let runtime_event_sender = runtime_event_sender.clone();

                tokio::spawn(async move { stream.run(event_sender, runtime_event_sender).await })
            })
            .collect::<Vec<_>>();

        IngestRuntime {
            event_receiver,
            runtime_event_receiver,
            stream_tasks,
        }
    }

    pub fn selected_endpoint(config: &IngestConfig) -> Result<IngestEndpoint, IngestError> {
        match config.channel {
            Channel::Ipc => Ok(IpcChannel::from_config(config)?.endpoint().clone()),
            Channel::Ws => Ok(WsChannel::from_config(config)?.endpoint().clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ipc_config() -> IngestConfig {
        IngestConfig {
            chain_id: 8_453,
            channel: Channel::Ipc,
            ipc_file_path: Some("/tmp/reth.ipc".to_string()),
            ws_url: Some("ws://127.0.0.1:8546".to_string()),
            subscribe_transactions: true,
            subscribe_logs: true,
            subscribe_blocks: true,
            reconnect_initial_ms: 500,
            reconnect_max_ms: 10_000,
            heartbeat_timeout_secs: 15,
            dedup_cache_size: 50_000,
            event_channel_capacity: 4_096,
            runtime_channel_capacity: 512,
        }
    }

    #[test]
    fn selects_ipc_endpoint_from_config() {
        let endpoint = IngestPipeline::selected_endpoint(&sample_ipc_config()).unwrap();
        assert_eq!(
            endpoint,
            IngestEndpoint::Ipc {
                file_path: "/tmp/reth.ipc".to_string(),
            }
        );
    }

    #[test]
    fn selects_ws_endpoint_from_config() {
        let mut config = sample_ipc_config();
        config.channel = Channel::Ws;

        let endpoint = IngestPipeline::selected_endpoint(&config).unwrap();
        assert_eq!(
            endpoint,
            IngestEndpoint::Ws {
                url: "ws://127.0.0.1:8546".to_string(),
            }
        );
    }

    #[test]
    fn builds_streams_from_enabled_subscriptions() {
        let mut config = sample_ipc_config();
        config.subscribe_logs = false;

        let pipeline = IngestPipeline::from_config(&config).unwrap();

        assert_eq!(pipeline.stream_count(), 2);
    }
}

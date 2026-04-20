use crate::channel::{IngestEndpoint, IpcChannel, WsChannel};
use crate::error::IngestError;
use crate::stream::BoxedIngestStream;
use config::IngestConfig;
use types::ingest::Channel;

#[derive(Default)]
pub struct IngestPipeline {
    streams: Vec<BoxedIngestStream>,
}

impl IngestPipeline {
    pub fn new() -> Self {
        Self::default()
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

    #[test]
    fn selects_ipc_endpoint_from_config() {
        let config = IngestConfig {
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
        };

        let endpoint = IngestPipeline::selected_endpoint(&config).unwrap();
        assert_eq!(
            endpoint,
            IngestEndpoint::Ipc {
                file_path: "/tmp/reth.ipc".to_string(),
            }
        );
    }

    #[test]
    fn selects_ws_endpoint_from_config() {
        let config = IngestConfig {
            channel: Channel::Ws,
            ipc_file_path: Some("/tmp/reth.ipc".to_string()),
            ws_url: Some("ws://127.0.0.1:8546".to_string()),
            subscribe_transactions: true,
            subscribe_logs: true,
            subscribe_blocks: true,
            reconnect_initial_ms: 500,
            reconnect_max_ms: 10_000,
            heartbeat_timeout_secs: 15,
            dedup_cache_size: 50_000,
        };

        let endpoint = IngestPipeline::selected_endpoint(&config).unwrap();
        assert_eq!(
            endpoint,
            IngestEndpoint::Ws {
                url: "ws://127.0.0.1:8546".to_string(),
            }
        );
    }
}

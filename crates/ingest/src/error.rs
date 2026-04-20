use thiserror::Error;
use types::ingest::Channel;

#[derive(Debug, Error)]
pub enum IngestError {
    #[error("missing channel configuration for {0:?}")]
    MissingChannelConfig(Channel),
    #[error("stream `{stream_name}` failed: {message}")]
    StreamFailure {
        stream_name: &'static str,
        message: String,
    },
    #[error("failed to send normalized event to downstream consumer")]
    EventDeliveryClosed,
    #[error("invalid stream state for `{stream_name}`: {message}")]
    InvalidStreamState {
        stream_name: &'static str,
        message: String,
    },
}

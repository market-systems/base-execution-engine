use serde_json::Error as SerdeJsonError;
use std::io;
use thiserror::Error;
use tokio_tungstenite::tungstenite;
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
    #[error("failed to send runtime event to downstream consumer")]
    RuntimeEventDeliveryClosed,
    #[error("invalid stream state for `{stream_name}`: {message}")]
    InvalidStreamState {
        stream_name: &'static str,
        message: String,
    },
    #[error("transport io error: {0}")]
    TransportIo(#[from] io::Error),
    #[error("json decode error: {0}")]
    Json(#[from] SerdeJsonError),
    #[error("websocket error: {0}")]
    WebSocket(#[from] tungstenite::Error),
    #[error("json-rpc error for `{stream_name}`: {message}")]
    JsonRpc {
        stream_name: &'static str,
        message: String,
    },
    #[error("invalid payload for `{stream_name}`: {message}")]
    InvalidPayload {
        stream_name: &'static str,
        message: String,
    },
}

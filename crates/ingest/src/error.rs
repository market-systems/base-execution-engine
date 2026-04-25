use alloy_transport::TransportError;
use serde_json::Error as SerdeJsonError;
use std::io;
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
    /// Wraps any alloy transport-level failure (ws/ipc handshake errors,
    /// pubsub frontend disconnects, request/response decode errors). The
    /// stream layer treats these as transient and reconnects via backoff.
    #[error("alloy transport error: {0}")]
    AlloyTransport(#[from] TransportError),
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

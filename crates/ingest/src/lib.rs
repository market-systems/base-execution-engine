#![forbid(unsafe_code)]

//! Input boundary for the system.
//!
//! This crate owns the conversion from external chain connectivity into the
//! normalized event stream consumed by `decision`.

pub mod channel;
pub mod error;
pub mod pipeline;
pub mod raw;
pub mod stream;

pub use channel::{IngestEndpoint, IpcChannel, WsChannel};
pub use error::IngestError;
pub use pipeline::IngestPipeline;
pub use raw::{DecodedTransactionFields, RawBlockMessage, RawLogMessage, RawTransactionMessage};
pub use stream::{
    BlockStream, BoxedIngestStream, EventSender, IngestStream, IngestStreamContext,
    LogStream, StreamSubscription, TransactionStream,
};

mod ipc;
mod ws;

pub use ipc::IpcChannel;
pub use ws::WsChannel;

use types::ingest::Channel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestEndpoint {
    Ipc { file_path: String },
    Ws { url: String },
}

impl IngestEndpoint {
    pub fn channel(&self) -> Channel {
        match self {
            Self::Ipc { .. } => Channel::Ipc,
            Self::Ws { .. } => Channel::Ws,
        }
    }
}

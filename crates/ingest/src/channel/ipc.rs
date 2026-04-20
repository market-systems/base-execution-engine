use crate::channel::IngestEndpoint;
use crate::error::IngestError;
use config::IngestConfig;
use types::ingest::Channel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcChannel {
    endpoint: IngestEndpoint,
}

impl IpcChannel {
    pub fn from_config(config: &IngestConfig) -> Result<Self, IngestError> {
        let Some(file_path) = config.ipc_file_path.clone() else {
            return Err(IngestError::MissingChannelConfig(Channel::Ipc));
        };

        Ok(Self {
            endpoint: IngestEndpoint::Ipc { file_path },
        })
    }

    pub fn endpoint(&self) -> &IngestEndpoint {
        &self.endpoint
    }
}

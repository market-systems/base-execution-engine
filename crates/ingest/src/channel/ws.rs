use crate::channel::IngestEndpoint;
use crate::error::IngestError;
use config::IngestConfig;
use types::ingest::Channel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WsChannel {
    endpoint: IngestEndpoint,
}

impl WsChannel {
    pub fn from_config(config: &IngestConfig) -> Result<Self, IngestError> {
        let Some(url) = config.ws_url.clone() else {
            return Err(IngestError::MissingChannelConfig(Channel::Ws));
        };

        Ok(Self {
            endpoint: IngestEndpoint::Ws { url },
        })
    }

    pub fn endpoint(&self) -> &IngestEndpoint {
        &self.endpoint
    }
}

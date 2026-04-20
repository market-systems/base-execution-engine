use types::ingest::{Exchange, Log, Metadata, Protocol};
use types::{Address, Topic};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLogMessage {
    pub address: Option<Address>,
    pub topics: Vec<Topic>,
    pub data: Option<String>,
    pub event_signature: Option<String>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub log_index: Option<u64>,
    pub removed: Option<bool>,
}

impl RawLogMessage {
    pub fn to_log(self, metadata: Metadata) -> Log {
        Log {
            metadata,
            address: self.address,
            topics: self.topics,
            data: self.data,
            event_signature: self.event_signature,
            protocol: self.protocol,
            exchange: self.exchange,
            log_index: self.log_index,
            removed: self.removed,
        }
    }
}

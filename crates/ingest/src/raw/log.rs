use registry::{exchange_from_event_signature, protocol_from_event_signature};
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
    pub fn decode(&self) -> DecodedLogFields {
        let event_signature = self.event_signature();

        DecodedLogFields {
            protocol: event_signature
                .as_deref()
                .and_then(protocol_from_event_signature),
            exchange: event_signature
                .as_deref()
                .and_then(exchange_from_event_signature)
                .or(self.exchange),
            event_signature,
        }
    }

    pub fn event_signature(&self) -> Option<String> {
        self.event_signature
            .clone()
            .or_else(|| self.topics.first().cloned())
    }

    pub fn to_log(self, metadata: Metadata) -> Log {
        let decoded = self.decode();

        Log {
            metadata,
            address: self.address,
            topics: self.topics,
            data: self.data,
            event_signature: decoded.event_signature,
            protocol: decoded.protocol.or(self.protocol),
            exchange: decoded.exchange,
            log_index: self.log_index,
            removed: self.removed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DecodedLogFields {
    pub event_signature: Option<String>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_first_topic_for_event_signature() {
        let message = RawLogMessage {
            address: None,
            topics: vec![
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
                    .to_string(),
            ],
            data: None,
            event_signature: None,
            protocol: None,
            exchange: None,
            log_index: None,
            removed: None,
        };

        let decoded = message.decode();

        assert_eq!(
            decoded.event_signature.as_deref(),
            Some(
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"
            )
        );
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV2));
    }
}

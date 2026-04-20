use crate::raw::{as_object, bool_field, string_field, u64_field};
use registry::resolve_log;
use serde_json::Value;
use types::ingest::{BlockContext, Exchange, Log, Metadata, Protocol};
use types::{Address, BlockHash, Topic, TxHash};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLogMessage {
    pub address: Option<Address>,
    pub topics: Vec<Topic>,
    pub data: Option<String>,
    pub tx_hash: Option<TxHash>,
    pub block_hash: Option<BlockHash>,
    pub block_number: Option<u64>,
    pub event_signature: Option<String>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub log_index: Option<u64>,
    pub removed: Option<bool>,
}

impl RawLogMessage {
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = as_object(value)?;
        let topics = object
            .get("topics")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .map(ToOwned::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Some(Self {
            address: string_field(object, "address"),
            topics,
            data: string_field(object, "data"),
            tx_hash: string_field(object, "transactionHash"),
            block_hash: string_field(object, "blockHash"),
            block_number: u64_field(object, "blockNumber"),
            event_signature: string_field(object, "eventSignature"),
            protocol: None,
            exchange: None,
            log_index: u64_field(object, "logIndex"),
            removed: bool_field(object, "removed"),
        })
    }

    pub fn block_context(&self) -> BlockContext {
        match self.block_number {
            Some(number) => BlockContext::Block {
                number,
                hash: self.block_hash.clone(),
            },
            None => BlockContext::Pending,
        }
    }

    pub fn decode(&self) -> DecodedLogFields {
        let event_signature = self.event_signature();
        let resolution = resolve_log(event_signature.as_deref(), self.address.as_deref());

        DecodedLogFields {
            protocol: resolution.protocol.or(self.protocol),
            exchange: resolution.exchange.or(self.exchange),
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
            protocol: decoded.protocol,
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
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822".to_string(),
            ],
            data: None,
            tx_hash: None,
            block_hash: None,
            block_number: None,
            event_signature: None,
            protocol: None,
            exchange: None,
            log_index: None,
            removed: None,
        };

        let decoded = message.decode();

        assert_eq!(
            decoded.event_signature.as_deref(),
            Some("0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822")
        );
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV2));
    }

    #[test]
    fn resolves_exchange_from_log_address() {
        let message = RawLogMessage {
            address: Some("0x8c1A3cF8F83074169Fe5d7Ad50B978E1cD6b37c7".to_string()),
            topics: vec![
                "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822".to_string(),
            ],
            data: None,
            tx_hash: None,
            block_hash: None,
            block_number: None,
            event_signature: None,
            protocol: None,
            exchange: None,
            log_index: None,
            removed: None,
        };

        let decoded = message.decode();

        assert_eq!(decoded.protocol, Some(Protocol::UniswapV2));
        assert_eq!(decoded.exchange, Some(Exchange::AlienBase));
    }
}

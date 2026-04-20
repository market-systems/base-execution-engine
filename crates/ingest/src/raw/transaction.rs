use crate::raw::{as_object, string_field, u64_field};
use registry::resolve_transaction;
use serde_json::Value;
use types::ingest::{BlockContext, Exchange, Metadata, Protocol, Transaction};
use types::{Address, BlockHash, Selector, TxHash};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTransactionMessage {
    pub tx_hash: Option<TxHash>,
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: Option<String>,
    pub input: Option<String>,
    pub block_hash: Option<BlockHash>,
    pub block_number: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionDecode {
    pub selector: Option<Selector>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
}

impl RawTransactionMessage {
    pub fn from_value(value: &Value) -> Option<Self> {
        let object = as_object(value)?;

        Some(Self {
            tx_hash: string_field(object, "hash"),
            from: string_field(object, "from"),
            to: string_field(object, "to"),
            value: string_field(object, "value"),
            input: string_field(object, "input"),
            block_hash: string_field(object, "blockHash"),
            block_number: u64_field(object, "blockNumber"),
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

    pub fn decode(&self) -> TransactionDecode {
        let selector = self.selector();
        let resolution = resolve_transaction(selector.as_deref(), self.to.as_deref());

        TransactionDecode {
            protocol: resolution.protocol,
            exchange: resolution.exchange,
            selector,
        }
    }

    pub fn selector(&self) -> Option<Selector> {
        let input = self.input.as_deref()?.trim();
        let hex = input.strip_prefix("0x").unwrap_or(input);

        if hex.len() < 8 {
            return None;
        }

        let selector = &hex[..8];
        if !selector.chars().all(|char| char.is_ascii_hexdigit()) {
            return None;
        }

        Some(format!("0x{}", selector.to_ascii_lowercase()))
    }

    pub fn to_transaction(self, metadata: Metadata) -> Transaction {
        let decoded = self.decode();

        Transaction {
            metadata,
            from: self.from,
            to: self.to,
            value: self.value,
            input: self.input,
            selector: decoded.selector,
            protocol: decoded.protocol,
            exchange: decoded.exchange,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ingest::Exchange;

    #[test]
    fn extracts_selector_from_input() {
        let message = RawTransactionMessage {
            tx_hash: None,
            from: None,
            to: None,
            value: None,
            input: Some(
                "0x38ed173900000000000000000000000000000000000000000000000000000000".to_string(),
            ),
            block_hash: None,
            block_number: None,
        };

        assert_eq!(message.selector().as_deref(), Some("0x38ed1739"));
    }

    #[test]
    fn resolves_exchange_from_address_when_selector_is_generic() {
        let message = RawTransactionMessage {
            tx_hash: None,
            from: None,
            to: Some("0x678Aa4bF4E210cf2166753e054d5b7c31cc7fa86".to_string()),
            value: None,
            input: Some("0x414bf38900000000".to_string()),
            block_hash: None,
            block_number: None,
        };

        let decoded = message.decode();

        assert_eq!(decoded.selector.as_deref(), Some("0x414bf389"));
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV3));
        assert_eq!(decoded.exchange, Some(Exchange::PancakeSwap));
    }
}

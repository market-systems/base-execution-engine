use registry::{exchange_from_selector, protocol_from_selector};
use types::ingest::{Exchange, Metadata, Protocol, Transaction};
use types::{Address, Selector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTransactionMessage {
    pub from: Option<Address>,
    pub to: Option<Address>,
    pub value: Option<String>,
    pub input: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionDecode {
    pub selector: Option<Selector>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
}

impl RawTransactionMessage {
    pub fn decode(&self) -> TransactionDecode {
        let selector = self.selector();

        TransactionDecode {
            protocol: selector
                .as_deref()
                .and_then(protocol_from_selector),
            exchange: selector
                .as_deref()
                .and_then(exchange_from_selector),
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

    #[test]
    fn extracts_selector_from_input() {
        let message = RawTransactionMessage {
            from: None,
            to: None,
            value: None,
            input: Some(
                "0x38ed173900000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            ),
        };

        assert_eq!(message.selector().as_deref(), Some("0x38ed1739"));
    }

    #[test]
    fn decodes_known_protocol_from_selector() {
        let message = RawTransactionMessage {
            from: None,
            to: None,
            value: None,
            input: Some("0x414bf38900000000".to_string()),
        };

        let decoded = message.decode();

        assert_eq!(decoded.selector.as_deref(), Some("0x414bf389"));
        assert_eq!(decoded.protocol, Some(Protocol::UniswapV3));
        assert_eq!(decoded.exchange, None);
    }
}

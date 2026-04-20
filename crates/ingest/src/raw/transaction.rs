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
pub struct DecodedTransactionFields {
    pub selector: Option<Selector>,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
}

impl RawTransactionMessage {
    pub fn to_transaction(
        self,
        metadata: Metadata,
        decoded: DecodedTransactionFields,
    ) -> Transaction {
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

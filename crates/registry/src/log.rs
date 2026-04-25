use crate::normalize_hex;
use types::ingest::{Exchange, Protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventSignatureEntry {
    signature: &'static str,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
}

const EVENT_SIGNATURE_ENTRIES: &[EventSignatureEntry] = &[
    EventSignatureEntry {
        signature: "0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    EventSignatureEntry {
        signature: "0x1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    EventSignatureEntry {
        signature: "0x4c209b5fc8ad50758f13e2e108bdbdff36c9b4a5d2f30f6a7c2a25d13f0f5e6d",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    EventSignatureEntry {
        signature: "0xdccd412f0b1252819a70d2cd3248e4723d18f7e41ee2b5f7f8ef4e3d5be2f4d8",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    EventSignatureEntry {
        signature: "0xc42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
    },
];

pub fn protocol_from_event_signature(event_signature: &str) -> Option<Protocol> {
    event_signature_entry(event_signature).and_then(|entry| entry.protocol)
}

pub fn exchange_from_event_signature(event_signature: &str) -> Option<Exchange> {
    event_signature_entry(event_signature).and_then(|entry| entry.exchange)
}

fn event_signature_entry(event_signature: &str) -> Option<&'static EventSignatureEntry> {
    let normalized = normalize_hex(event_signature)?;
    EVENT_SIGNATURE_ENTRIES
        .iter()
        .find(|entry| entry.signature == normalized)
}

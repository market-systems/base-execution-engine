use crate::normalize_hex;
use types::ingest::{Exchange, Protocol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SelectorEntry {
    selector: &'static str,
    protocol: Option<Protocol>,
    exchange: Option<Exchange>,
}

const SELECTOR_ENTRIES: &[SelectorEntry] = &[
    SelectorEntry {
        selector: "0x38ed1739",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x8803dbee",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x7ff36ab5",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0xfb3bdb41",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x18cbafe5",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x4a25d94a",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x5c11d795",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0xb6f9de95",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x791ac947",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x414bf389",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
    },
    SelectorEntry {
        selector: "0xc04b8d59",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
    },
    SelectorEntry {
        selector: "0xdb3e2198",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
    },
    SelectorEntry {
        selector: "0xf28c0498",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
    },
    SelectorEntry {
        selector: "0xac9650d8",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x3593564c",
        protocol: Some(Protocol::UniswapV4),
        exchange: None,
    },
    SelectorEntry {
        selector: "0x24856bc3",
        protocol: Some(Protocol::UniswapV4),
        exchange: None,
    },
];

pub fn protocol_from_selector(selector: &str) -> Option<Protocol> {
    selector_entry(selector).and_then(|entry| entry.protocol)
}

pub fn exchange_from_selector(selector: &str) -> Option<Exchange> {
    selector_entry(selector).and_then(|entry| entry.exchange)
}

fn selector_entry(selector: &str) -> Option<&'static SelectorEntry> {
    let normalized = normalize_hex(selector)?;
    SELECTOR_ENTRIES
        .iter()
        .find(|entry| entry.selector == normalized)
}

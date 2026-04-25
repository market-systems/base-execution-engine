#![forbid(unsafe_code)]

//! Shared protocol and exchange lookup tables.
//!
//! The registry is intentionally data-driven so ingest can classify payloads
//! from multiple weak signals: calldata selectors, event signatures, and known
//! contract addresses on Base.

mod log;
mod transaction;

use types::ingest::{Exchange, Protocol};

pub use log::{exchange_from_event_signature, protocol_from_event_signature};
pub use transaction::{exchange_from_selector, protocol_from_selector};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressType {
    Router,
    Factory,
    PositionManager,
    PoolManager,
    Bonding,
    Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistryMatch {
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
}

impl RegistryMatch {
    pub const fn new(protocol: Option<Protocol>, exchange: Option<Exchange>) -> Self {
        Self { protocol, exchange }
    }

    pub const fn empty() -> Self {
        Self {
            protocol: None,
            exchange: None,
        }
    }

    pub fn merge(self, fallback: Self) -> Self {
        Self {
            protocol: self.protocol.or(fallback.protocol),
            exchange: self.exchange.or(fallback.exchange),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressMetadata {
    pub address: &'static str,
    pub protocol: Option<Protocol>,
    pub exchange: Option<Exchange>,
    pub address_type: AddressType,
    pub label: &'static str,
}

const ADDRESS_BOOK: &[AddressMetadata] = &[
    AddressMetadata {
        address: "0x4752ba5dbc23f44d87826276bf6fd6b1c372ad24",
        protocol: Some(Protocol::UniswapV2),
        exchange: None,
        address_type: AddressType::Router,
        label: "Uniswap V2 Router02",
    },
    AddressMetadata {
        address: "0x2626664c2603336e57b271c5c0b26f421741e481",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
        address_type: AddressType::Router,
        label: "Uniswap V3 SwapRouter02",
    },
    AddressMetadata {
        address: "0x198ef79f1f515f02dfe9e3115ed9fc07183f02fc",
        protocol: Some(Protocol::UniswapV4),
        exchange: None,
        address_type: AddressType::Router,
        label: "Uniswap Universal Router 2",
    },
    AddressMetadata {
        address: "0x33128a8fc17869897dce68ed026d694621f6fdfd",
        protocol: Some(Protocol::UniswapV3),
        exchange: None,
        address_type: AddressType::Factory,
        label: "Uniswap V3 Factory",
    },
    AddressMetadata {
        address: "0xfda619b6d20975be80a10332cd39b9a4b0faa8bb",
        protocol: Some(Protocol::UniswapV2),
        exchange: Some(Exchange::BaseSwap),
        address_type: AddressType::Factory,
        label: "BaseSwap Factory",
    },
    AddressMetadata {
        address: "0x327df1e6de05895d2ab08513aadd9313fe505d86",
        protocol: Some(Protocol::UniswapV2),
        exchange: Some(Exchange::BaseSwap),
        address_type: AddressType::Router,
        label: "BaseSwap Router",
    },
    AddressMetadata {
        address: "0x3e84d913803b02a4a7f027165e8ca42c14c0fde7",
        protocol: Some(Protocol::UniswapV2),
        exchange: Some(Exchange::AlienBase),
        address_type: AddressType::Factory,
        label: "AlienBase Factory",
    },
    AddressMetadata {
        address: "0x8c1a3cf8f83074169fe5d7ad50b978e1cd6b37c7",
        protocol: Some(Protocol::UniswapV2),
        exchange: Some(Exchange::AlienBase),
        address_type: AddressType::Router,
        label: "AlienBase Router",
    },
    AddressMetadata {
        address: "0x0fd83557b2be93617c9c1c1b6fd549401c74558c",
        protocol: Some(Protocol::UniswapV3),
        exchange: Some(Exchange::AlienBase),
        address_type: AddressType::Factory,
        label: "AlienBase Uniswap V3 Factory",
    },
    AddressMetadata {
        address: "0xb20c411fc84fbb27e78608c24d0056d974ea9411",
        protocol: Some(Protocol::UniswapV3),
        exchange: Some(Exchange::AlienBase),
        address_type: AddressType::Router,
        label: "AlienBase V3 Smart Router",
    },
    AddressMetadata {
        address: "0xcf77a3ba9a5ca399b7c97c74d54e5b1beb874e43",
        protocol: Some(Protocol::Aerodrome),
        exchange: Some(Exchange::Aerodrome),
        address_type: AddressType::Router,
        label: "Aerodrome Router",
    },
    AddressMetadata {
        address: "0x6cb442acf35158d5eda88fe602221b67b400be3e",
        protocol: Some(Protocol::Aerodrome),
        exchange: Some(Exchange::Aerodrome),
        address_type: AddressType::Router,
        label: "Aerodrome Universal Router",
    },
    AddressMetadata {
        address: "0x678aa4bf4e210cf2166753e054d5b7c31cc7fa86",
        protocol: Some(Protocol::UniswapV3),
        exchange: Some(Exchange::PancakeSwap),
        address_type: AddressType::Router,
        label: "PancakeSwap V3 Smart Router",
    },
    AddressMetadata {
        address: "0xf66dea7b3e897cd44a5a231c61b6b4423d613259",
        protocol: Some(Protocol::Virtuals),
        exchange: Some(Exchange::Virtuals),
        address_type: AddressType::Bonding,
        label: "Virtuals Bonding Proxy",
    },
    AddressMetadata {
        address: "0x0b3e328455c4059eeb9e3f84b5543f74e24e7e1b",
        protocol: Some(Protocol::Virtuals),
        exchange: Some(Exchange::Virtuals),
        address_type: AddressType::Token,
        label: "Virtuals Token",
    },
];

pub fn protocol_from_address(address: &str) -> Option<Protocol> {
    address_metadata(address).and_then(|entry| entry.protocol)
}

pub fn exchange_from_address(address: &str) -> Option<Exchange> {
    address_metadata(address).and_then(|entry| entry.exchange)
}

pub fn address_metadata(address: &str) -> Option<&'static AddressMetadata> {
    let normalized = normalize_hex(address)?;
    ADDRESS_BOOK
        .iter()
        .find(|entry| entry.address == normalized)
}

pub fn resolve_transaction(selector: Option<&str>, to: Option<&str>) -> RegistryMatch {
    let from_address = RegistryMatch::new(
        to.and_then(protocol_from_address),
        to.and_then(exchange_from_address),
    );
    let from_selector = RegistryMatch::new(
        selector.and_then(protocol_from_selector),
        selector.and_then(exchange_from_selector),
    );

    from_address.merge(from_selector)
}

pub fn resolve_log(event_signature: Option<&str>, address: Option<&str>) -> RegistryMatch {
    let from_address = RegistryMatch::new(
        address.and_then(protocol_from_address),
        address.and_then(exchange_from_address),
    );
    let from_event = RegistryMatch::new(
        event_signature.and_then(protocol_from_event_signature),
        event_signature.and_then(exchange_from_event_signature),
    );

    from_address.merge(from_event)
}

pub(crate) fn normalize_hex(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);

    if hex.is_empty() || !hex.chars().all(|char| char.is_ascii_hexdigit()) {
        return None;
    }

    Some(format!("0x{}", hex.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_mixed_case_hex_keys() {
        assert_eq!(
            normalize_hex("0xB20C411FC84FBB27e78608C24d0056D974ea9411").as_deref(),
            Some("0xb20c411fc84fbb27e78608c24d0056d974ea9411")
        );
    }

    #[test]
    fn resolves_exchange_from_router_address() {
        let resolution = resolve_transaction(
            Some("0x414BF389"),
            Some("0x678Aa4bF4E210cf2166753e054d5b7c31cc7fa86"),
        );

        assert_eq!(resolution.protocol, Some(Protocol::UniswapV3));
        assert_eq!(resolution.exchange, Some(Exchange::PancakeSwap));
    }

    #[test]
    fn prefers_address_over_generic_event_signature() {
        let resolution = resolve_log(
            Some("0xd78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822"),
            Some("0x8c1A3cF8F83074169Fe5d7Ad50B978E1cD6b37c7"),
        );

        assert_eq!(resolution.protocol, Some(Protocol::UniswapV2));
        assert_eq!(resolution.exchange, Some(Exchange::AlienBase));
    }

    #[test]
    fn returns_none_for_unknown_address() {
        assert_eq!(
            address_metadata("0x0000000000000000000000000000000000000001"),
            None
        );
    }
}

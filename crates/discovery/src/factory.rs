//! Factory configuration: where to look for pool-creation events.

use serde::{Deserialize, Serialize};
use types::{Exchange, Protocol};

/// Distinguishes the factory's pool family. The discovery engine uses this to
/// decide which `PairCreated` vs `PoolCreated` ABI to decode and which
/// hydration path to take post-discovery (V2 reserves vs V3 slot0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryKind {
    UniswapV2,
    UniswapV3,
}

/// One factory we want to scan. The user supplies these via configuration —
/// the crate ships well-known Base mainnet addresses as helpers but does not
/// hard-code them into the discovery service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactoryConfig {
    pub address: String,
    pub kind: FactoryKind,
    pub protocol: Protocol,
    pub exchange: Option<Exchange>,
    /// Earliest block to start scanning. Set to the factory's deployment
    /// block to avoid wasted RPC calls on empty ranges.
    pub deployment_block: u64,
    /// Default fee for V2 factories that do not encode the fee in the
    /// `PairCreated` event (almost all of them). Ignored for V3 where the
    /// fee tier is part of the event payload.
    pub default_v2_fee_bps: Option<u32>,
    /// Optional cap on how many blocks each `eth_getLogs` request covers.
    /// Defaults to 5_000 if `None`, which fits comfortably under the
    /// 10k-block ceiling enforced by most public RPCs.
    pub log_chunk_size: Option<u64>,
}

impl FactoryConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.address.trim().is_empty() {
            return Err("factory address must not be empty");
        }
        if matches!(self.kind, FactoryKind::UniswapV2) && self.default_v2_fee_bps.is_none() {
            return Err("v2 factory requires default_v2_fee_bps");
        }
        Ok(())
    }

    pub fn log_chunk_size_or_default(&self) -> u64 {
        self.log_chunk_size.unwrap_or(5_000).max(1)
    }
}

/// Well-known Base mainnet factory addresses. Provided as helpers; not
/// referenced by the discovery service itself so we stay testable.
pub mod base_mainnet {
    use super::{FactoryConfig, FactoryKind};
    use types::{Exchange, Protocol};

    /// UniswapV2 factory on Base.
    pub fn uniswap_v2() -> FactoryConfig {
        FactoryConfig {
            address: "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6".to_string(),
            kind: FactoryKind::UniswapV2,
            protocol: Protocol::UniswapV2,
            exchange: None,
            deployment_block: 6_601_915,
            default_v2_fee_bps: Some(30),
            log_chunk_size: None,
        }
    }

    /// UniswapV3 factory on Base.
    pub fn uniswap_v3() -> FactoryConfig {
        FactoryConfig {
            address: "0x33128a8fC17869897dcE68Ed026d694621f6FDfD".to_string(),
            kind: FactoryKind::UniswapV3,
            protocol: Protocol::UniswapV3,
            exchange: None,
            deployment_block: 1_371_680,
            default_v2_fee_bps: None,
            log_chunk_size: None,
        }
    }

    /// Aerodrome V2 (volatile + stable) factory on Base.
    pub fn aerodrome_v2() -> FactoryConfig {
        FactoryConfig {
            address: "0x420DD381b31aEf6683db6B902084cB0FFECe40Da".to_string(),
            kind: FactoryKind::UniswapV2,
            protocol: Protocol::Aerodrome,
            exchange: Some(Exchange::Aerodrome),
            deployment_block: 3_200_559,
            // Volatile pool default; stable pools also live here at 5 bps but
            // they emit the same PairCreated. Fee detection is left to the
            // multicall hydration step in a follow-up.
            default_v2_fee_bps: Some(30),
            log_chunk_size: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_address() {
        let bad = FactoryConfig {
            address: "".into(),
            kind: FactoryKind::UniswapV2,
            protocol: Protocol::UniswapV2,
            exchange: None,
            deployment_block: 0,
            default_v2_fee_bps: Some(30),
            log_chunk_size: None,
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn rejects_v2_without_default_fee() {
        let bad = FactoryConfig {
            address: "0x1".into(),
            kind: FactoryKind::UniswapV2,
            protocol: Protocol::UniswapV2,
            exchange: None,
            deployment_block: 0,
            default_v2_fee_bps: None,
            log_chunk_size: None,
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn well_known_factories_validate() {
        assert!(base_mainnet::uniswap_v2().validate().is_ok());
        assert!(base_mainnet::uniswap_v3().validate().is_ok());
        assert!(base_mainnet::aerodrome_v2().validate().is_ok());
    }

    #[test]
    fn log_chunk_size_floor() {
        let mut cfg = base_mainnet::uniswap_v2();
        cfg.log_chunk_size = Some(0);
        assert_eq!(cfg.log_chunk_size_or_default(), 1);
        cfg.log_chunk_size = None;
        assert_eq!(cfg.log_chunk_size_or_default(), 5_000);
    }
}

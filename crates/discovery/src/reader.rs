//! `FactoryReader` trait + alloy-backed implementation.
//!
//! The trait carves out the I/O surface so the [`crate::service::DiscoveryService`]
//! can be unit-tested with deterministic fixtures. Production wires
//! [`AlloyFactoryReader`].

use alloy_primitives::{Address, B256, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::{BlockNumberOrTag, Filter, Log as RpcLog};
use alloy_sol_types::{sol, SolCall, SolEvent};
use alloy_transport_http::Http;
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use reqwest::Client as ReqwestClient;
use rpc::EngineProvider;
use std::str::FromStr;

use crate::factory::FactoryConfig;

// ---- Solidity ABI bindings used by discovery -------------------------------

sol! {
    /// UniswapV2 family. `pair` is non-indexed. `tokenN` are indexed.
    #[derive(Debug)]
    event PairCreated(address indexed token0, address indexed token1, address pair, uint256 allPairsLength);

    /// UniswapV3 family. `fee` and tokens are indexed; `tickSpacing` and
    /// `pool` are non-indexed.
    #[derive(Debug)]
    event PoolCreated(address indexed token0, address indexed token1, uint24 indexed fee, int24 tickSpacing, address pool);

    /// V2 reserves. The `blockTimestampLast` slot is read for completeness
    /// but discovery only consumes reserve0/reserve1.
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);

    /// V3 packed slot0. We only need `sqrtPriceX96` and `tick`; the rest is
    /// ignored. Returning the full struct is cheaper than emitting a
    /// dedicated wrapper getter on the contract side.
    function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);

    function liquidity() external view returns (uint128);

    /// Multicall3 (https://github.com/mds1/multicall) `aggregate3` entry
    /// point. We use the failure-tolerant variant so a single broken pool
    /// does not nuke a 500-call batch.
    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }

    struct Result {
        bool success;
        bytes returnData;
    }

    function aggregate3(Call3[] calldata calls) external payable returns (Result[] memory returnData);
}

/// V2 pool surfaced by the factory log scanner. Reserves and fee are filled
/// in by the multicall hydration step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredV2Pool {
    pub address: String,
    pub token0: String,
    pub token1: String,
    pub reserve0: u128,
    pub reserve1: u128,
    pub fee_bps: u32,
}

/// V3 pool surfaced by the factory log scanner. `slot0` and active
/// liquidity are filled in by the multicall hydration step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredV3Pool {
    pub address: String,
    pub token0: String,
    pub token1: String,
    pub fee_pips: u32,
    pub tick_spacing: i32,
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
}

/// I/O surface needed by the discovery engine. Pinned `async_trait` for
/// dyn dispatch because the service holds it behind `Arc<dyn FactoryReader>`.
#[async_trait]
pub trait FactoryReader: Send + Sync {
    /// Latest block number visible to the underlying provider.
    async fn latest_block(&self) -> anyhow::Result<u64>;

    /// Stream V2 `PairCreated` events for the factory in `[from_block,
    /// to_block]` (inclusive). Implementations are expected to honour the
    /// factory's `log_chunk_size_or_default()` ceiling.
    async fn fetch_v2_pair_created(
        &self,
        factory: &FactoryConfig,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<DiscoveredV2Pool>>;

    /// Stream V3 `PoolCreated` events for the factory in `[from_block,
    /// to_block]` (inclusive).
    async fn fetch_v3_pool_created(
        &self,
        factory: &FactoryConfig,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<DiscoveredV3Pool>>;

    /// Hydrate `getReserves()` for every pool in `pools`. The returned
    /// vector is index-aligned with the input — entries that failed (e.g.
    /// the contract was self-destructed) are returned with reserves left at
    /// their incoming `u128::MAX` sentinel and should be filtered by the
    /// caller. The fee field is preserved verbatim.
    async fn hydrate_v2_reserves(
        &self,
        multicall3: Address,
        pools: Vec<DiscoveredV2Pool>,
    ) -> anyhow::Result<Vec<DiscoveredV2Pool>>;

    /// Hydrate `slot0` + `liquidity()` for every pool in `pools`.
    async fn hydrate_v3_state(
        &self,
        multicall3: Address,
        pools: Vec<DiscoveredV3Pool>,
    ) -> anyhow::Result<Vec<DiscoveredV3Pool>>;
}

/// Production reader. Holds an `EngineProvider` and uses its inner
/// `RootProvider` for `eth_getLogs` / `eth_call` directly — discovery is a
/// rare, batched operation so we don't need the full retry layer; we keep
/// retries at the call site.
pub struct AlloyFactoryReader {
    provider: EngineProvider,
}

impl AlloyFactoryReader {
    pub fn new(provider: EngineProvider) -> Self {
        Self { provider }
    }

    fn provider(&self) -> &alloy_provider::RootProvider<Http<ReqwestClient>> {
        self.provider.inner()
    }
}

#[async_trait]
impl FactoryReader for AlloyFactoryReader {
    async fn latest_block(&self) -> anyhow::Result<u64> {
        let n = self
            .provider()
            .get_block_number()
            .await
            .context("eth_blockNumber for discovery latest_block")?;
        Ok(n)
    }

    async fn fetch_v2_pair_created(
        &self,
        factory: &FactoryConfig,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<DiscoveredV2Pool>> {
        let factory_address = parse_address(&factory.address)
            .context("v2 factory address must be valid hex")?;
        let topic0: B256 = PairCreated::SIGNATURE_HASH;
        let chunk = factory.log_chunk_size_or_default();
        let mut out = Vec::new();

        let mut cursor = from_block;
        while cursor <= to_block {
            let chunk_end = (cursor + chunk - 1).min(to_block);
            let filter = Filter::new()
                .address(factory_address)
                .event_signature(topic0)
                .from_block(BlockNumberOrTag::Number(cursor))
                .to_block(BlockNumberOrTag::Number(chunk_end));
            let logs: Vec<RpcLog> = self
                .provider()
                .get_logs(&filter)
                .await
                .with_context(|| format!("eth_getLogs failed for v2 factory chunk {cursor}-{chunk_end}"))?;
            for log in logs {
                if let Some(pool) = decode_v2_pair_created(&log, factory) {
                    out.push(pool);
                }
            }
            cursor = chunk_end.saturating_add(1);
        }
        Ok(out)
    }

    async fn fetch_v3_pool_created(
        &self,
        factory: &FactoryConfig,
        from_block: u64,
        to_block: u64,
    ) -> anyhow::Result<Vec<DiscoveredV3Pool>> {
        let factory_address = parse_address(&factory.address)
            .context("v3 factory address must be valid hex")?;
        let topic0: B256 = PoolCreated::SIGNATURE_HASH;
        let chunk = factory.log_chunk_size_or_default();
        let mut out = Vec::new();

        let mut cursor = from_block;
        while cursor <= to_block {
            let chunk_end = (cursor + chunk - 1).min(to_block);
            let filter = Filter::new()
                .address(factory_address)
                .event_signature(topic0)
                .from_block(BlockNumberOrTag::Number(cursor))
                .to_block(BlockNumberOrTag::Number(chunk_end));
            let logs: Vec<RpcLog> = self
                .provider()
                .get_logs(&filter)
                .await
                .with_context(|| format!("eth_getLogs failed for v3 factory chunk {cursor}-{chunk_end}"))?;
            for log in logs {
                if let Some(pool) = decode_v3_pool_created(&log) {
                    out.push(pool);
                }
            }
            cursor = chunk_end.saturating_add(1);
        }
        Ok(out)
    }

    async fn hydrate_v2_reserves(
        &self,
        multicall3: Address,
        pools: Vec<DiscoveredV2Pool>,
    ) -> anyhow::Result<Vec<DiscoveredV2Pool>> {
        if pools.is_empty() {
            return Ok(pools);
        }
        let calls: Vec<Call3> = pools
            .iter()
            .map(|p| {
                let target = parse_address(&p.address).unwrap_or(Address::ZERO);
                Call3 {
                    target,
                    allowFailure: true,
                    callData: getReservesCall {}.abi_encode().into(),
                }
            })
            .collect();
        let results = self
            .multicall_aggregate3(multicall3, calls)
            .await
            .context("v2 multicall hydration failed")?;
        let mut hydrated = pools;
        for (slot, res) in hydrated.iter_mut().zip(results.into_iter()) {
            if !res.success {
                continue;
            }
            if let Ok(decoded) = getReservesCall::abi_decode_returns(&res.returnData, true) {
                slot.reserve0 = decoded.reserve0.to::<u128>();
                slot.reserve1 = decoded.reserve1.to::<u128>();
            }
        }
        Ok(hydrated)
    }

    async fn hydrate_v3_state(
        &self,
        multicall3: Address,
        pools: Vec<DiscoveredV3Pool>,
    ) -> anyhow::Result<Vec<DiscoveredV3Pool>> {
        if pools.is_empty() {
            return Ok(pools);
        }
        // We interleave slot0 and liquidity calls so each pool occupies two
        // adjacent multicall slots. Index `2k` is slot0 for pool k, `2k+1`
        // is liquidity.
        let mut calls: Vec<Call3> = Vec::with_capacity(pools.len() * 2);
        for p in &pools {
            let target = parse_address(&p.address).unwrap_or(Address::ZERO);
            calls.push(Call3 {
                target,
                allowFailure: true,
                callData: slot0Call {}.abi_encode().into(),
            });
            calls.push(Call3 {
                target,
                allowFailure: true,
                callData: liquidityCall {}.abi_encode().into(),
            });
        }
        let results = self
            .multicall_aggregate3(multicall3, calls)
            .await
            .context("v3 multicall hydration failed")?;
        let mut hydrated = pools;
        for (k, slot) in hydrated.iter_mut().enumerate() {
            let slot0_res = &results[2 * k];
            let liq_res = &results[2 * k + 1];
            if !slot0_res.success || !liq_res.success {
                continue;
            }
            if let Ok(decoded) = slot0Call::abi_decode_returns(&slot0_res.returnData, true) {
                slot.sqrt_price_x96 = U256::from(decoded.sqrtPriceX96);
                slot.tick = decoded.tick.as_i32();
            }
            if let Ok(decoded) = liquidityCall::abi_decode_returns(&liq_res.returnData, true) {
                slot.liquidity = decoded._0;
            }
        }
        Ok(hydrated)
    }
}

impl AlloyFactoryReader {
    async fn multicall_aggregate3(
        &self,
        multicall3: Address,
        calls: Vec<Call3>,
    ) -> anyhow::Result<Vec<Result>> {
        let calldata = aggregate3Call { calls }.abi_encode();
        let request = alloy_rpc_types_eth::TransactionRequest {
            to: Some(multicall3.into()),
            input: alloy_rpc_types_eth::TransactionInput::new(calldata.into()),
            ..Default::default()
        };
        let raw = self
            .provider()
            .call(&request)
            .await
            .context("eth_call to multicall3.aggregate3 failed")?;
        let decoded = aggregate3Call::abi_decode_returns(raw.as_ref(), true)
            .context("failed to decode multicall3 aggregate3 returns")?;
        Ok(decoded.returnData)
    }
}

fn parse_address(input: &str) -> anyhow::Result<Address> {
    Address::from_str(input.trim())
        .map_err(|_| anyhow!("invalid hex address `{input}`"))
}

fn decode_v2_pair_created(log: &RpcLog, factory: &FactoryConfig) -> Option<DiscoveredV2Pool> {
    // SAFETY: we already filtered by topic0 on the RPC side, so a decode
    // failure here is genuinely malformed data and we drop it silently.
    let inner = log.inner.clone();
    let evt = PairCreated::decode_log(&inner, true).ok()?;
    Some(DiscoveredV2Pool {
        address: format!("{:#x}", evt.pair),
        token0: format!("{:#x}", evt.token0),
        token1: format!("{:#x}", evt.token1),
        // Reserves and fee are filled by the hydration pass; sentinel zero
        // here means "unknown, don't insert into the book yet".
        reserve0: 0,
        reserve1: 0,
        fee_bps: factory.default_v2_fee_bps.unwrap_or(30),
    })
}

fn decode_v3_pool_created(log: &RpcLog) -> Option<DiscoveredV3Pool> {
    let inner = log.inner.clone();
    let evt = PoolCreated::decode_log(&inner, true).ok()?;
    Some(DiscoveredV3Pool {
        address: format!("{:#x}", evt.pool),
        token0: format!("{:#x}", evt.token0),
        token1: format!("{:#x}", evt.token1),
        // `fee` is `uint24` (alloy `Uint<24, 1>`); funnel through the
        // canonical `to::<u32>()` instead of `From` since alloy doesn't
        // implement `From<Uint<24, 1>> for u32`.
        fee_pips: evt.fee.to::<u32>(),
        tick_spacing: evt.tickSpacing.as_i32(),
        // Hydrated by the multicall pass.
        sqrt_price_x96: U256::ZERO,
        tick: 0,
        liquidity: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_address_round_trip() {
        let raw = "0x4200000000000000000000000000000000000006";
        let parsed = parse_address(raw).unwrap();
        assert_eq!(format!("{parsed:#x}"), raw);
    }

    #[test]
    fn parse_address_rejects_garbage() {
        assert!(parse_address("0xnope").is_err());
        assert!(parse_address("").is_err());
    }
}

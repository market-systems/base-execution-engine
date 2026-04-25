//! V3 discovery sub-module: hydrated `DiscoveredV3Pool` → `V3PoolState` →
//! [`PoolBook`].

use markets::{PoolBook, V3PoolState};
use tracing::warn;

use crate::factory::FactoryConfig;
use crate::reader::DiscoveredV3Pool;
use crate::DiscoveryError;

/// Build `V3PoolState` instances from hydrated discovery results, then
/// insert them into `book`. Pools with `liquidity == 0` are skipped: the
/// in-range quote requires non-zero active liquidity.
pub fn ingest_v3(
    book: &mut PoolBook,
    factory: &FactoryConfig,
    pools: Vec<DiscoveredV3Pool>,
) -> Result<usize, DiscoveryError> {
    let mut inserted = 0usize;
    for pool in pools {
        if pool.liquidity == 0 || pool.sqrt_price_x96.is_zero() {
            continue;
        }
        let state = match V3PoolState::new(
            pool.address.clone(),
            factory.protocol,
            factory.exchange,
            pool.token0,
            pool.token1,
            pool.fee_pips,
            pool.tick_spacing,
            pool.sqrt_price_x96,
            pool.liquidity,
            pool.tick,
        ) {
            Ok(state) => state,
            Err(err) => {
                warn!(
                    pool_address = %pool.address,
                    error = %err,
                    "discovered v3 pool failed validation, skipping"
                );
                continue;
            }
        };
        book.insert_v3_pool(state);
        inserted += 1;
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::FactoryKind;
    use alloy_primitives::U256;
    use types::Protocol;

    fn factory() -> FactoryConfig {
        FactoryConfig {
            address: "0xfactory".into(),
            kind: FactoryKind::UniswapV3,
            protocol: Protocol::UniswapV3,
            exchange: None,
            deployment_block: 0,
            default_v2_fee_bps: None,
            log_chunk_size: None,
        }
    }

    fn pool(address: &str, liquidity: u128) -> DiscoveredV3Pool {
        DiscoveredV3Pool {
            address: address.into(),
            token0: "0xa".into(),
            token1: "0xb".into(),
            fee_pips: 500,
            tick_spacing: 10,
            sqrt_price_x96: U256::from(1u128) << 96,
            tick: 0,
            liquidity,
        }
    }

    #[test]
    fn skips_uninitialised_pools() {
        let mut book = PoolBook::new();
        let inserted = ingest_v3(
            &mut book,
            &factory(),
            vec![pool("0xp1", 0), pool("0xp2", 10u128.pow(18))],
        )
        .unwrap();
        assert_eq!(inserted, 1);
        assert!(book.v3_pool("0xp1").is_none());
        assert!(book.v3_pool("0xp2").is_some());
    }
}

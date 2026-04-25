//! V2 discovery sub-module: keeps the V2-specific filtering / `PoolBook`
//! injection logic out of the generic service code.

use markets::{PoolBook, V2PoolState};
use tracing::warn;

use crate::factory::FactoryConfig;
use crate::reader::DiscoveredV2Pool;
use crate::DiscoveryError;

/// Build `V2PoolState` instances from hydrated discovery results, then
/// insert them into `book`. Pools whose reserves remain at the sentinel
/// (`0`) are skipped: the V2 quote math requires non-zero reserves.
///
/// Returns the count of pools actually inserted.
pub fn ingest_v2(
    book: &mut PoolBook,
    factory: &FactoryConfig,
    pools: Vec<DiscoveredV2Pool>,
) -> Result<usize, DiscoveryError> {
    let mut inserted = 0usize;
    for pool in pools {
        if pool.reserve0 == 0 || pool.reserve1 == 0 {
            // The hydration call either failed for this pool or the pool was
            // initialised but never seeded with liquidity. Either way it is
            // not yet quotable.
            continue;
        }
        let state = match V2PoolState::new(
            pool.address.clone(),
            factory.protocol,
            factory.exchange,
            pool.token0,
            pool.token1,
            pool.reserve0,
            pool.reserve1,
            pool.fee_bps,
        ) {
            Ok(state) => state,
            Err(err) => {
                warn!(
                    pool_address = %pool.address,
                    error = %err,
                    "discovered v2 pool failed validation, skipping"
                );
                continue;
            }
        };
        book.insert_v2_pool(state);
        inserted += 1;
    }
    Ok(inserted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::FactoryKind;
    use types::Protocol;

    fn factory() -> FactoryConfig {
        FactoryConfig {
            address: "0xfactory".into(),
            kind: FactoryKind::UniswapV2,
            protocol: Protocol::UniswapV2,
            exchange: None,
            deployment_block: 0,
            default_v2_fee_bps: Some(30),
            log_chunk_size: None,
        }
    }

    fn pool(address: &str, reserve0: u128, reserve1: u128) -> DiscoveredV2Pool {
        DiscoveredV2Pool {
            address: address.into(),
            token0: "0xa".into(),
            token1: "0xb".into(),
            reserve0,
            reserve1,
            fee_bps: 30,
        }
    }

    #[test]
    fn skips_pools_without_reserves() {
        let mut book = PoolBook::new();
        let inserted = ingest_v2(
            &mut book,
            &factory(),
            vec![pool("0xp1", 0, 0), pool("0xp2", 100, 200)],
        )
        .unwrap();
        assert_eq!(inserted, 1);
        assert!(book.v2_pool("0xp1").is_none());
        assert!(book.v2_pool("0xp2").is_some());
    }

    #[test]
    fn skips_invalid_pools_silently() {
        let mut book = PoolBook::new();
        let mut bad = pool("0xp1", 100, 200);
        bad.token0 = "0xsame".into();
        bad.token1 = "0xsame".into();
        let inserted = ingest_v2(&mut book, &factory(), vec![bad]).unwrap();
        assert_eq!(inserted, 0);
        assert!(book.v2_pool("0xp1").is_none());
    }
}

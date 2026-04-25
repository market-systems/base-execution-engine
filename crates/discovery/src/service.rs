//! Discovery orchestration: factory scan → multicall hydration → `PoolBook`.

use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::Address;
use markets::PoolBook;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::factory::{FactoryConfig, FactoryKind};
use crate::reader::FactoryReader;
use crate::{v2, v3, DiscoveryError};

/// Result of a single discovery pass. Useful for logging / metrics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    pub factory_address: String,
    pub kind: FactoryKind,
    pub from_block: u64,
    pub to_block: u64,
    pub pools_scanned: usize,
    pub pools_inserted: usize,
}

/// Stateless orchestrator. Holds an `Arc<dyn FactoryReader>` so production
/// code can pass an alloy-backed reader and tests can inject a mock.
pub struct DiscoveryService {
    reader: Arc<dyn FactoryReader>,
    multicall3: Address,
    /// If `Some`, the discovery scan will stop at the configured head block
    /// instead of asking the reader; useful for replays.
    pinned_head: Option<u64>,
}

impl std::fmt::Debug for DiscoveryService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoveryService")
            .field("multicall3", &self.multicall3)
            .field("pinned_head", &self.pinned_head)
            .finish()
    }
}

impl DiscoveryService {
    pub fn new(reader: Arc<dyn FactoryReader>, multicall3: &str) -> Result<Self, DiscoveryError> {
        let multicall3 = Address::from_str(multicall3.trim())
            .map_err(|_| DiscoveryError::InvalidFactory("multicall3 address is not valid hex"))?;
        Ok(Self {
            reader,
            multicall3,
            pinned_head: None,
        })
    }

    pub fn with_pinned_head(mut self, head: u64) -> Self {
        self.pinned_head = Some(head);
        self
    }

    /// Run discovery for a single factory. The caller is responsible for
    /// looping over multiple factories — this keeps cancellation semantics
    /// simple at the call site.
    pub async fn run_factory(
        &self,
        factory: &FactoryConfig,
        book: &mut PoolBook,
    ) -> Result<DiscoveryReport, DiscoveryError> {
        if let Err(reason) = factory.validate() {
            return Err(DiscoveryError::InvalidFactory(reason));
        }
        let head = match self.pinned_head {
            Some(h) => h,
            None => self.reader.latest_block().await?,
        };
        if head < factory.deployment_block {
            warn!(
                factory_address = %factory.address,
                head_block = head,
                deployment_block = factory.deployment_block,
                "head block is behind factory deployment; skipping"
            );
            return Ok(DiscoveryReport {
                factory_address: factory.address.clone(),
                kind: factory.kind,
                from_block: factory.deployment_block,
                to_block: head,
                pools_scanned: 0,
                pools_inserted: 0,
            });
        }

        let from_block = factory.deployment_block;
        let to_block = head;

        let inserted = match factory.kind {
            FactoryKind::UniswapV2 => {
                let pools = self
                    .reader
                    .fetch_v2_pair_created(factory, from_block, to_block)
                    .await?;
                let scanned = pools.len();
                let hydrated = self
                    .reader
                    .hydrate_v2_reserves(self.multicall3, pools)
                    .await?;
                let inserted = v2::ingest_v2(book, factory, hydrated)?;
                info!(
                    factory_address = %factory.address,
                    scanned,
                    inserted,
                    "v2 discovery complete"
                );
                (scanned, inserted)
            }
            FactoryKind::UniswapV3 => {
                let pools = self
                    .reader
                    .fetch_v3_pool_created(factory, from_block, to_block)
                    .await?;
                let scanned = pools.len();
                let hydrated = self
                    .reader
                    .hydrate_v3_state(self.multicall3, pools)
                    .await?;
                let inserted = v3::ingest_v3(book, factory, hydrated)?;
                info!(
                    factory_address = %factory.address,
                    scanned,
                    inserted,
                    "v3 discovery complete"
                );
                (scanned, inserted)
            }
        };

        Ok(DiscoveryReport {
            factory_address: factory.address.clone(),
            kind: factory.kind,
            from_block,
            to_block,
            pools_scanned: inserted.0,
            pools_inserted: inserted.1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::factory::FactoryConfig;
    use crate::reader::{DiscoveredV2Pool, DiscoveredV3Pool, FactoryReader};
    use alloy_primitives::U256;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use types::Protocol;

    /// Mock reader. Records every call and returns scripted fixtures so
    /// the orchestrator's flow can be asserted end-to-end without touching
    /// the network.
    #[derive(Default)]
    struct MockReader {
        calls: Mutex<Vec<String>>,
        latest_block: u64,
        v2_pools: Vec<DiscoveredV2Pool>,
        v2_hydrated: Vec<DiscoveredV2Pool>,
        v3_pools: Vec<DiscoveredV3Pool>,
        v3_hydrated: Vec<DiscoveredV3Pool>,
    }

    #[async_trait]
    impl FactoryReader for MockReader {
        async fn latest_block(&self) -> anyhow::Result<u64> {
            self.calls.lock().unwrap().push("latest_block".into());
            Ok(self.latest_block)
        }

        async fn fetch_v2_pair_created(
            &self,
            _factory: &FactoryConfig,
            from_block: u64,
            to_block: u64,
        ) -> anyhow::Result<Vec<DiscoveredV2Pool>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("fetch_v2:{from_block}-{to_block}"));
            Ok(self.v2_pools.clone())
        }

        async fn fetch_v3_pool_created(
            &self,
            _factory: &FactoryConfig,
            from_block: u64,
            to_block: u64,
        ) -> anyhow::Result<Vec<DiscoveredV3Pool>> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("fetch_v3:{from_block}-{to_block}"));
            Ok(self.v3_pools.clone())
        }

        async fn hydrate_v2_reserves(
            &self,
            _multicall3: Address,
            _pools: Vec<DiscoveredV2Pool>,
        ) -> anyhow::Result<Vec<DiscoveredV2Pool>> {
            self.calls.lock().unwrap().push("hydrate_v2".into());
            Ok(self.v2_hydrated.clone())
        }

        async fn hydrate_v3_state(
            &self,
            _multicall3: Address,
            _pools: Vec<DiscoveredV3Pool>,
        ) -> anyhow::Result<Vec<DiscoveredV3Pool>> {
            self.calls.lock().unwrap().push("hydrate_v3".into());
            Ok(self.v3_hydrated.clone())
        }
    }

    fn v2_factory() -> FactoryConfig {
        FactoryConfig {
            address: "0x8909Dc15e40173Ff4699343b6eB8132c65e18eC6".into(),
            kind: FactoryKind::UniswapV2,
            protocol: Protocol::UniswapV2,
            exchange: None,
            deployment_block: 100,
            default_v2_fee_bps: Some(30),
            log_chunk_size: Some(50),
        }
    }

    fn v3_factory() -> FactoryConfig {
        FactoryConfig {
            address: "0x33128a8fC17869897dcE68Ed026d694621f6FDfD".into(),
            kind: FactoryKind::UniswapV3,
            protocol: Protocol::UniswapV3,
            exchange: None,
            deployment_block: 100,
            default_v2_fee_bps: None,
            log_chunk_size: None,
        }
    }

    #[tokio::test]
    async fn runs_full_v2_pipeline() {
        let reader = Arc::new(MockReader {
            latest_block: 200,
            v2_pools: vec![DiscoveredV2Pool {
                address: "0xpool".into(),
                token0: "0xa".into(),
                token1: "0xb".into(),
                reserve0: 0,
                reserve1: 0,
                fee_bps: 30,
            }],
            v2_hydrated: vec![DiscoveredV2Pool {
                address: "0xpool".into(),
                token0: "0xa".into(),
                token1: "0xb".into(),
                reserve0: 1_000,
                reserve1: 2_000,
                fee_bps: 30,
            }],
            ..Default::default()
        });
        let svc = DiscoveryService::new(
            reader.clone(),
            "0xcA11bde05977b3631167028862bE2a173976CA11",
        )
        .unwrap();
        let mut book = PoolBook::new();
        let report = svc.run_factory(&v2_factory(), &mut book).await.unwrap();
        assert_eq!(report.pools_inserted, 1);
        assert_eq!(report.pools_scanned, 1);
        assert!(book.v2_pool("0xpool").is_some());

        let calls = reader.calls.lock().unwrap().clone();
        assert_eq!(calls[0], "latest_block");
        assert_eq!(calls[1], "fetch_v2:100-200");
        assert_eq!(calls[2], "hydrate_v2");
    }

    #[tokio::test]
    async fn runs_full_v3_pipeline() {
        let reader = Arc::new(MockReader {
            latest_block: 200,
            v3_pools: vec![DiscoveredV3Pool {
                address: "0xpool3".into(),
                token0: "0xa".into(),
                token1: "0xb".into(),
                fee_pips: 500,
                tick_spacing: 10,
                sqrt_price_x96: U256::ZERO,
                tick: 0,
                liquidity: 0,
            }],
            v3_hydrated: vec![DiscoveredV3Pool {
                address: "0xpool3".into(),
                token0: "0xa".into(),
                token1: "0xb".into(),
                fee_pips: 500,
                tick_spacing: 10,
                sqrt_price_x96: U256::from(1u128) << 96,
                tick: 0,
                liquidity: 10u128.pow(18),
            }],
            ..Default::default()
        });
        let svc = DiscoveryService::new(
            reader,
            "0xcA11bde05977b3631167028862bE2a173976CA11",
        )
        .unwrap();
        let mut book = PoolBook::new();
        let report = svc.run_factory(&v3_factory(), &mut book).await.unwrap();
        assert_eq!(report.pools_inserted, 1);
        assert!(book.v3_pool("0xpool3").is_some());
    }

    #[tokio::test]
    async fn skips_when_head_is_behind_deployment() {
        let reader = Arc::new(MockReader {
            latest_block: 50,
            ..Default::default()
        });
        let svc = DiscoveryService::new(
            reader,
            "0xcA11bde05977b3631167028862bE2a173976CA11",
        )
        .unwrap();
        let mut book = PoolBook::new();
        let report = svc.run_factory(&v2_factory(), &mut book).await.unwrap();
        assert_eq!(report.pools_scanned, 0);
        assert_eq!(report.pools_inserted, 0);
    }

    #[test]
    fn rejects_invalid_multicall3_address() {
        let reader: Arc<dyn FactoryReader> = Arc::new(MockReader::default());
        let err = DiscoveryService::new(reader, "0xnope").unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidFactory(_)));
    }
}

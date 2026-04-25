#![forbid(unsafe_code)]

//! Pool discovery layer.
//!
//! Discovery is a one-shot bootstrap pass (and an optional incremental
//! catch-up) that turns *factory addresses* into *pool state* the
//! `markets::PoolBook` can consume:
//!
//! 1. **Factory log scan** — walk `PairCreated` (UniswapV2 family) or
//!    `PoolCreated` (UniswapV3 family) events from the factory deployment
//!    block to a configurable head, in `eth_getLogs` chunks small enough to
//!    sit under provider response-size caps.
//! 2. **Multicall3 hydration** — for every discovered pool, batch-read the
//!    on-chain state needed for quoting (V2: `getReserves()`; V3: `slot0()`
//!    + `liquidity()`).
//! 3. **`PoolBook` injection** — wrap the results in `V2PoolState` /
//!    `V3PoolState` and insert them into a fresh book.
//!
//! The crate exposes a transport trait ([`FactoryReader`]) so the discovery
//! engine can be unit-tested with a deterministic mock instead of a live
//! provider, and an alloy-backed implementation ([`AlloyFactoryReader`])
//! for production.
//!
//! Out of scope for the first slice (tracked separately):
//! - Continuous discovery via `PairCreated`/`PoolCreated` subscriptions
//!   (today the scan is a one-off bootstrap; new pools must trigger a
//!   manual rerun).
//! - V3 tick bitmap hydration. `slot0`/`liquidity` are sufficient for the
//!   in-range quote shipped in `markets::v3`.

pub mod factory;
pub mod reader;
pub mod service;
pub mod v2;
pub mod v3;

pub use factory::{FactoryConfig, FactoryKind};
pub use reader::{AlloyFactoryReader, DiscoveredV2Pool, DiscoveredV3Pool, FactoryReader};
pub use service::{DiscoveryReport, DiscoveryService};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("factory misconfigured: {0}")]
    InvalidFactory(&'static str),
    #[error("rpc transport error: {0}")]
    Transport(#[from] anyhow::Error),
    #[error("decoded pool failed validation: {0}")]
    InvalidPool(#[from] markets::MarketError),
    #[error("multicall3 address required but not configured")]
    MissingMulticall3,
}

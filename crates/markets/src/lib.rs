#![forbid(unsafe_code)]

//! Market state and deterministic quote math.
//!
//! Two pool families are modelled today:
//!
//! - **V2 / constant product** — covers UniswapV2, BaseSwap, Aerodrome
//!   stable+volatile, AlienBase, etc. Quote math is the standard
//!   `x*y=k` formula with a per-pool `fee_bps`.
//! - **V3 / concentrated liquidity** — covers UniswapV3 and Aerodrome
//!   Slipstream. State carries `sqrt_price_x96`, active `liquidity`, and
//!   `tick`; quote math uses the closed-form single-tick swap step. See
//!   [`v3`] for scope and limitations (tick crossing is deferred).
//!
//! Both families are united behind [`PoolState`], inserted into a
//! [`PoolBook`], and updated incrementally from `DecodedLogEvent`s the
//! ingest layer ships through `apply_log`.

pub mod v3;

pub use v3::V3PoolState;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use types::ingest::{BlockContext, DecodedLogKind, Log as IngestLog};
use types::{Address, Amount, BlockNumber, Exchange, Protocol, TxHash};

const BPS_DENOMINATOR: u128 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolKind {
    ConstantProduct,
    ConcentratedLiquidity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolQuote {
    pub pool_address: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: Amount,
    pub amount_out: Amount,
    pub fee_bps: u32,
    pub price_impact_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct V2PoolState {
    pub address: Address,
    pub protocol: Protocol,
    pub exchange: Option<Exchange>,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: Amount,
    pub reserve1: Amount,
    pub fee_bps: u32,
    pub last_updated_block: Option<BlockNumber>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketEventOutcome {
    Applied,
    Ignored,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AppliedLogKey {
    tx_hash: TxHash,
    log_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct V2SyncSnapshot {
    reserve0: Amount,
    reserve1: Amount,
    last_updated_block: Option<BlockNumber>,
}

#[derive(Debug, Default)]
pub struct PoolBook {
    pools: HashMap<String, PoolState>,
    applied_sync_history: HashMap<AppliedLogKey, V2SyncSnapshot>,
}

impl V2PoolState {
    pub fn new(
        address: impl Into<Address>,
        protocol: Protocol,
        exchange: Option<Exchange>,
        token0: impl Into<Address>,
        token1: impl Into<Address>,
        reserve0: Amount,
        reserve1: Amount,
        fee_bps: u32,
    ) -> Result<Self, MarketError> {
        let state = Self {
            address: address.into(),
            protocol,
            exchange,
            token0: token0.into(),
            token1: token1.into(),
            reserve0,
            reserve1,
            fee_bps,
            last_updated_block: None,
        };

        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), MarketError> {
        if self.token0 == self.token1 {
            return Err(MarketError::InvalidPool(
                "pool tokens must be distinct addresses",
            ));
        }

        if self.reserve0 == 0 || self.reserve1 == 0 {
            return Err(MarketError::InvalidPool(
                "pool reserves must be greater than zero",
            ));
        }

        if u128::from(self.fee_bps) >= BPS_DENOMINATOR {
            return Err(MarketError::InvalidPool(
                "fee_bps must be lower than 10_000",
            ));
        }

        Ok(())
    }

    pub fn apply_sync(
        &mut self,
        reserve0: Amount,
        reserve1: Amount,
        block_number: Option<BlockNumber>,
    ) -> Result<(), MarketError> {
        if reserve0 == 0 || reserve1 == 0 {
            return Err(MarketError::InvalidPool(
                "sync reserves must be greater than zero",
            ));
        }

        self.reserve0 = reserve0;
        self.reserve1 = reserve1;
        self.last_updated_block = block_number;
        Ok(())
    }

    pub fn quote_exact_in(
        &self,
        token_in: &str,
        amount_in: Amount,
    ) -> Result<PoolQuote, MarketError> {
        if amount_in == 0 {
            return Err(MarketError::InsufficientInput);
        }

        let (reserve_in, reserve_out, token_out) = if token_in.eq_ignore_ascii_case(&self.token0) {
            (self.reserve0, self.reserve1, self.token1.clone())
        } else if token_in.eq_ignore_ascii_case(&self.token1) {
            (self.reserve1, self.reserve0, self.token0.clone())
        } else {
            return Err(MarketError::UnknownToken {
                pool_address: self.address.clone(),
                token: token_in.to_string(),
            });
        };

        let fee_multiplier = BPS_DENOMINATOR
            .checked_sub(u128::from(self.fee_bps))
            .ok_or(MarketError::ArithmeticOverflow)?;
        let amount_in_with_fee = amount_in
            .checked_mul(fee_multiplier)
            .ok_or(MarketError::ArithmeticOverflow)?;
        let numerator = amount_in_with_fee
            .checked_mul(reserve_out)
            .ok_or(MarketError::ArithmeticOverflow)?;
        let denominator = reserve_in
            .checked_mul(BPS_DENOMINATOR)
            .and_then(|value| value.checked_add(amount_in_with_fee))
            .ok_or(MarketError::ArithmeticOverflow)?;

        let amount_out = numerator / denominator;
        if amount_out == 0 {
            return Err(MarketError::InsufficientLiquidity);
        }

        let price_impact_bps =
            estimate_price_impact_bps(reserve_in, reserve_out, amount_in, amount_out)?;

        Ok(PoolQuote {
            pool_address: self.address.clone(),
            token_in: token_in.to_string(),
            token_out,
            amount_in,
            amount_out,
            fee_bps: self.fee_bps,
            price_impact_bps,
        })
    }

    pub fn contains_token(&self, token: &str) -> bool {
        self.token0.eq_ignore_ascii_case(token) || self.token1.eq_ignore_ascii_case(token)
    }

    pub fn other_token(&self, token: &str) -> Option<&str> {
        if self.token0.eq_ignore_ascii_case(token) {
            Some(&self.token1)
        } else if self.token1.eq_ignore_ascii_case(token) {
            Some(&self.token0)
        } else {
            None
        }
    }

    pub fn apply_log(&mut self, log: &IngestLog) -> Result<MarketEventOutcome, MarketError> {
        let Some(address) = &log.address else {
            return Ok(MarketEventOutcome::Ignored);
        };

        if !address.eq_ignore_ascii_case(&self.address) {
            return Ok(MarketEventOutcome::Ignored);
        }

        let Some(decoded) = &log.decoded_event else {
            return Ok(MarketEventOutcome::Ignored);
        };

        match decoded.kind {
            DecodedLogKind::V2Sync => {
                let reserve0 = decoded
                    .reserve0
                    .ok_or(MarketError::MissingDecodedField("reserve0"))?;
                let reserve1 = decoded
                    .reserve1
                    .ok_or(MarketError::MissingDecodedField("reserve1"))?;
                self.apply_sync(
                    reserve0,
                    reserve1,
                    block_number(&log.metadata.block_context),
                )?;
                Ok(MarketEventOutcome::Applied)
            }
            _ => Ok(MarketEventOutcome::Ignored),
        }
    }

    fn snapshot(&self) -> V2SyncSnapshot {
        V2SyncSnapshot {
            reserve0: self.reserve0,
            reserve1: self.reserve1,
            last_updated_block: self.last_updated_block,
        }
    }

    fn restore(&mut self, snapshot: &V2SyncSnapshot) {
        self.reserve0 = snapshot.reserve0;
        self.reserve1 = snapshot.reserve1;
        self.last_updated_block = snapshot.last_updated_block;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PoolState {
    V2(V2PoolState),
    V3(V3PoolState),
}

impl PoolState {
    pub fn kind(&self) -> PoolKind {
        match self {
            Self::V2(_) => PoolKind::ConstantProduct,
            Self::V3(_) => PoolKind::ConcentratedLiquidity,
        }
    }

    pub fn address(&self) -> &str {
        match self {
            Self::V2(pool) => &pool.address,
            Self::V3(pool) => &pool.address,
        }
    }

    /// Best-effort unified quote dispatch. The decision layer prefers the
    /// concrete `quote_exact_in` on each pool kind, but this helper is
    /// useful for code paths that walk a heterogeneous route.
    pub fn quote_exact_in(
        &self,
        token_in: &str,
        amount_in: Amount,
    ) -> Result<PoolQuote, MarketError> {
        match self {
            Self::V2(pool) => pool.quote_exact_in(token_in, amount_in),
            Self::V3(pool) => pool.quote_exact_in(token_in, amount_in),
        }
    }
}

impl PoolBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_v2_pool(&mut self, pool: V2PoolState) {
        self.pools
            .insert(normalize_address_key(&pool.address), PoolState::V2(pool));
    }

    pub fn insert_v3_pool(&mut self, pool: V3PoolState) {
        self.pools
            .insert(normalize_address_key(&pool.address), PoolState::V3(pool));
    }

    pub fn pool(&self, address: &str) -> Option<&PoolState> {
        self.pools.get(&normalize_address_key(address))
    }

    pub fn v2_pool(&self, address: &str) -> Option<&V2PoolState> {
        match self.pool(address) {
            Some(PoolState::V2(pool)) => Some(pool),
            _ => None,
        }
    }

    pub fn v3_pool(&self, address: &str) -> Option<&V3PoolState> {
        match self.pool(address) {
            Some(PoolState::V3(pool)) => Some(pool),
            _ => None,
        }
    }

    pub fn v2_pools(&self) -> impl Iterator<Item = &V2PoolState> {
        self.pools.values().filter_map(|pool| match pool {
            PoolState::V2(pool) => Some(pool),
            _ => None,
        })
    }

    pub fn v3_pools(&self) -> impl Iterator<Item = &V3PoolState> {
        self.pools.values().filter_map(|pool| match pool {
            PoolState::V3(pool) => Some(pool),
            _ => None,
        })
    }

    pub fn apply_log(&mut self, log: &IngestLog) -> Result<MarketEventOutcome, MarketError> {
        let Some(address) = &log.address else {
            return Ok(MarketEventOutcome::Ignored);
        };
        let key = normalize_address_key(address);
        let Some(mut pool_state) = self.pools.remove(&key) else {
            return Ok(MarketEventOutcome::Ignored);
        };

        let outcome = match &mut pool_state {
            PoolState::V2(pool) => self.apply_v2_log(pool, log),
            PoolState::V3(pool) => pool.apply_log(log),
        }?;

        self.pools.insert(key, pool_state);
        Ok(outcome)
    }

    fn apply_v2_log(
        &mut self,
        pool: &mut V2PoolState,
        log: &IngestLog,
    ) -> Result<MarketEventOutcome, MarketError> {
        let Some(decoded) = &log.decoded_event else {
            return Ok(MarketEventOutcome::Ignored);
        };

        match decoded.kind {
            DecodedLogKind::V2Sync => {
                let applied_key = applied_log_key(log)?;
                if log.removed.unwrap_or(false) {
                    let Some(previous) = self.applied_sync_history.remove(&applied_key) else {
                        return Err(MarketError::MissingRevertSnapshot {
                            tx_hash: applied_key.tx_hash,
                            log_index: applied_key.log_index,
                        });
                    };
                    pool.restore(&previous);
                    Ok(MarketEventOutcome::Reverted)
                } else {
                    let snapshot = pool.snapshot();
                    let outcome = pool.apply_log(log)?;
                    if outcome == MarketEventOutcome::Applied {
                        self.applied_sync_history.insert(applied_key, snapshot);
                    }
                    Ok(outcome)
                }
            }
            _ => Ok(MarketEventOutcome::Ignored),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MarketError {
    #[error("invalid pool: {0}")]
    InvalidPool(&'static str),
    #[error("token `{token}` is not part of pool `{pool_address}`")]
    UnknownToken {
        pool_address: Address,
        token: Address,
    },
    #[error("input amount must be greater than zero")]
    InsufficientInput,
    #[error("pool cannot produce a non-zero output for the requested input")]
    InsufficientLiquidity,
    #[error("arithmetic overflow while quoting pool")]
    ArithmeticOverflow,
    #[error("decoded log event missing required field `{0}`")]
    MissingDecodedField(&'static str),
    #[error("log is missing tx hash or log index required for deterministic state application")]
    MissingLogIdentity,
    #[error("no revert snapshot found for removed log {tx_hash}:{log_index}")]
    MissingRevertSnapshot { tx_hash: TxHash, log_index: u64 },
}

fn block_number(context: &BlockContext) -> Option<BlockNumber> {
    match context {
        BlockContext::Pending => None,
        BlockContext::Block { number, .. } => Some(*number),
    }
}

fn estimate_price_impact_bps(
    reserve_in: Amount,
    reserve_out: Amount,
    amount_in: Amount,
    amount_out: Amount,
) -> Result<u32, MarketError> {
    let quoted_price_numerator = reserve_out
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(MarketError::ArithmeticOverflow)?;
    let quoted_price = quoted_price_numerator / reserve_in;

    let realized_price_numerator = amount_out
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(MarketError::ArithmeticOverflow)?;
    let realized_price = realized_price_numerator / amount_in.max(1);

    if quoted_price == 0 || realized_price >= quoted_price {
        return Ok(0);
    }

    let delta = quoted_price - realized_price;
    let impact = delta
        .checked_mul(BPS_DENOMINATOR)
        .ok_or(MarketError::ArithmeticOverflow)?
        / quoted_price;

    Ok(impact.min(u128::from(u32::MAX)) as u32)
}

fn applied_log_key(log: &IngestLog) -> Result<AppliedLogKey, MarketError> {
    Ok(AppliedLogKey {
        tx_hash: log
            .metadata
            .tx_hash
            .clone()
            .ok_or(MarketError::MissingLogIdentity)?,
        log_index: log.log_index.ok_or(MarketError::MissingLogIdentity)?,
    })
}

fn normalize_address_key(address: &str) -> String {
    let trimmed = address.trim();
    let hex = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    format!("0x{}", hex.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ingest::{
        Channel, DecodeStatus, DecodedLogEvent, DecodedLogKind, Metadata, RawMessageType,
        RawPayloadSummary,
    };

    fn sample_pool() -> V2PoolState {
        V2PoolState::new(
            "0xpool",
            Protocol::UniswapV2,
            Some(Exchange::Aerodrome),
            "0xweth",
            "0xusdc",
            10_000_000_000_000_000_000_u128,
            25_000_000_000_u128,
            30,
        )
        .unwrap()
    }

    #[test]
    fn quotes_from_token0_to_token1() {
        let quote = sample_pool()
            .quote_exact_in("0xweth", 1_000_000_000_000_000_000_u128)
            .unwrap();

        assert_eq!(quote.token_out, "0xusdc");
        assert!(quote.amount_out > 0);
        assert_eq!(quote.fee_bps, 30);
    }

    #[test]
    fn quotes_from_token1_to_token0() {
        let quote = sample_pool()
            .quote_exact_in("0xusdc", 1_000_000_000_u128)
            .unwrap();

        assert_eq!(quote.token_out, "0xweth");
        assert!(quote.amount_out > 0);
    }

    #[test]
    fn rejects_unknown_token() {
        let error = sample_pool().quote_exact_in("0xnope", 1).unwrap_err();

        assert!(matches!(error, MarketError::UnknownToken { .. }));
    }

    #[test]
    fn sync_updates_reserves_and_block() {
        let mut pool = sample_pool();
        pool.apply_sync(9, 11, Some(123)).unwrap();

        assert_eq!(pool.reserve0, 9);
        assert_eq!(pool.reserve1, 11);
        assert_eq!(pool.last_updated_block, Some(123));
    }

    #[test]
    fn applies_v2_sync_log_to_pool_state() {
        let mut pool = sample_pool();
        let log = sample_sync_log(123, 77, 88, false);

        let outcome = pool.apply_log(&log).unwrap();

        assert_eq!(outcome, MarketEventOutcome::Applied);
        assert_eq!(pool.reserve0, 77);
        assert_eq!(pool.reserve1, 88);
        assert_eq!(pool.last_updated_block, Some(123));
    }

    #[test]
    fn pool_book_applies_and_reverts_v2_sync_logs() {
        let mut book = PoolBook::new();
        book.insert_v2_pool(sample_pool());

        let applied = book
            .apply_log(&sample_sync_log(123, 77, 88, false))
            .unwrap();
        let reverted = book.apply_log(&sample_sync_log(123, 77, 88, true)).unwrap();

        assert_eq!(applied, MarketEventOutcome::Applied);
        assert_eq!(reverted, MarketEventOutcome::Reverted);

        let pool = book.v2_pool("0xpool").unwrap();
        assert_eq!(pool.reserve0, 10_000_000_000_000_000_000_u128);
        assert_eq!(pool.reserve1, 25_000_000_000_u128);
        assert_eq!(pool.last_updated_block, None);
    }

    #[test]
    fn removed_log_without_snapshot_errors() {
        let mut book = PoolBook::new();
        book.insert_v2_pool(sample_pool());

        let error = book
            .apply_log(&sample_sync_log(123, 77, 88, true))
            .unwrap_err();

        assert_eq!(
            error,
            MarketError::MissingRevertSnapshot {
                tx_hash: "0xtx".to_string(),
                log_index: 1,
            }
        );
    }

    fn sample_sync_log(
        block_number: u64,
        reserve0: Amount,
        reserve1: Amount,
        removed: bool,
    ) -> IngestLog {
        IngestLog {
            metadata: Metadata {
                block_context: BlockContext::Block {
                    number: block_number,
                    hash: None,
                },
                channel: Channel::Ws,
                observed_at_ms: 1,
                chain_id: 8453,
                tx_hash: Some("0xtx".to_string()),
                decode_status: DecodeStatus::Decoded,
                raw: RawPayloadSummary {
                    message_type: RawMessageType::Log,
                    payload_size_bytes: 0,
                    fingerprint: None,
                    subscription: None,
                },
            },
            address: Some("0xpool".to_string()),
            topics: Vec::new(),
            data: None,
            event_signature: None,
            decoded_event: Some(DecodedLogEvent {
                kind: DecodedLogKind::V2Sync,
                pool: Some("0xpool".to_string()),
                sender: None,
                recipient: None,
                owner: None,
                reserve0: Some(reserve0),
                reserve1: Some(reserve1),
                amount0_in: None,
                amount1_in: None,
                amount0_out: None,
                amount1_out: None,
                amount0: None,
                amount1: None,
                liquidity: None,
                sqrt_price_x96: None,
                tick: None,
                tick_lower: None,
                tick_upper: None,
                protocol: Some(Protocol::UniswapV2),
                exchange: Some(Exchange::Aerodrome),
            }),
            protocol: Some(Protocol::UniswapV2),
            exchange: Some(Exchange::Aerodrome),
            log_index: Some(1),
            removed: Some(removed),
        }
    }
}

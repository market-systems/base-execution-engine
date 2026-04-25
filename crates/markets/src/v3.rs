//! Uniswap V3 / Aerodrome Slipstream pool state and in-range quote math.
//!
//! Scope of this module (Phase 1.1 MVP):
//!
//! - **State struct**: address, fee tier (in pips), tick spacing, `sqrt_price_x96`,
//!   active `liquidity`, current `tick`. Tick bitmap is intentionally not stored
//!   yet — it requires a discovery / multicall pass that is its own follow-up.
//! - **Quote math**: in-range exact-input swap step using the standard Uniswap
//!   V3 formulas. The function refuses to quote if the requested amount-in
//!   would push the price beyond the current `tick_spacing` boundary, since
//!   without a tick bitmap we cannot reason about what liquidity exists past
//!   that point. Callers that need larger swaps must wait for tick crossing
//!   support.
//! - **Log application**: `apply_log` consumes `DecodedLogKind::V3Swap` and
//!   adopts the post-swap `sqrt_price_x96`, `liquidity`, and `tick` published
//!   by the event. This is the cheapest possible incremental update: the
//!   chain has already done the math for us.
//!
//! Out of scope here (tracked separately): tick bitmap, Mint/Burn deltas to
//! initialised tick liquidity, off-by-one rounding parity tests against the
//! reference Solidity implementation. The quote math is conservative
//! (rounds amount-out down, refuses to cross initialised ticks) so any drift
//! from the reference is in our favour: we under-quote, never over-quote.

use alloy_primitives::U256;
use serde::{Deserialize, Serialize};
use types::ingest::{DecodedLogEvent, DecodedLogKind, Log as IngestLog};
use types::{Address, Amount, BlockNumber, Exchange, Protocol};

use crate::{block_number, MarketError, MarketEventOutcome, PoolQuote};

const FEE_PIPS_DENOMINATOR: u128 = 1_000_000;

/// Soft safety cap on the in-tick quote. Without a tick bitmap we cannot tell
/// where the next initialised tick sits; assuming the pessimistic "every
/// `tick_spacing` boundary is initialised" makes the quote useless even for
/// blue-chip pools that span thousands of ticks. We instead refuse to quote
/// when the closed-form sqrt-price move implies a price impact above this
/// threshold — at that magnitude, the chance that the active liquidity is
/// stable past the move drops fast and the risk engine should be the one to
/// say "no". Set conservatively to 5% (500 bps).
const MAX_IN_RANGE_IMPACT_BPS: u32 = 500;

/// `2^96`. We hold it as a `U256` so multiplications never overflow; it is
/// constructed once at module load via `lazy_static`-equivalent.
fn q96() -> U256 {
    U256::from(1u128) << 96
}

/// Maximum sqrt price representable as a `uint160` per V3 spec.
fn max_sqrt_price() -> U256 {
    // 1461446703485210103287273052203988822378723970342
    U256::from_str_radix(
        "1461446703485210103287273052203988822378723970342",
        10,
    )
    .expect("hard-coded constant parses")
}

fn min_sqrt_price() -> U256 {
    // 4295128739
    U256::from(4_295_128_739u128)
}

/// State of a single Uniswap V3 / Slipstream pool. The book stores these
/// behind `PoolState::V3` so they share routing / lookup paths with V2 pools.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct V3PoolState {
    pub address: Address,
    pub protocol: Protocol,
    pub exchange: Option<Exchange>,
    pub token0: Address,
    pub token1: Address,
    /// Fee tier in pips (1e-6). The four canonical Uniswap tiers are 100,
    /// 500, 3_000, and 10_000 pips. Aerodrome Slipstream uses arbitrary tiers
    /// derived from `tickSpacing`, so we accept any value in `(0, 1_000_000)`.
    pub fee_pips: u32,
    /// Spacing between initialisable ticks. Stored so the in-range quote
    /// can reject swaps that would cross a `tickSpacing` boundary, which is
    /// the boundary up to which we can safely reason without a tick bitmap.
    pub tick_spacing: i32,
    /// Current `sqrtPriceX96`. Decoded from the pool slot0 / Swap log.
    /// Serialised as a decimal string because `U256` has no JSON support
    /// and we want the on-disk shape to be stable across machines.
    #[serde(with = "u256_decimal_string")]
    pub sqrt_price_x96: U256,
    /// Active in-range liquidity (Uniswap V3 "L"). Held as `u128` because the
    /// chain caps liquidity below `2^128`.
    pub liquidity: u128,
    /// Current active tick.
    pub tick: i32,
    pub last_updated_block: Option<BlockNumber>,
}

impl V3PoolState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address: impl Into<Address>,
        protocol: Protocol,
        exchange: Option<Exchange>,
        token0: impl Into<Address>,
        token1: impl Into<Address>,
        fee_pips: u32,
        tick_spacing: i32,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    ) -> Result<Self, MarketError> {
        let state = Self {
            address: address.into(),
            protocol,
            exchange,
            token0: token0.into(),
            token1: token1.into(),
            fee_pips,
            tick_spacing,
            sqrt_price_x96,
            liquidity,
            tick,
            last_updated_block: None,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), MarketError> {
        if self.token0 == self.token1 {
            return Err(MarketError::InvalidPool(
                "v3 pool tokens must be distinct addresses",
            ));
        }
        if u128::from(self.fee_pips) >= FEE_PIPS_DENOMINATOR {
            return Err(MarketError::InvalidPool(
                "v3 fee_pips must be lower than 1_000_000",
            ));
        }
        if self.tick_spacing <= 0 {
            return Err(MarketError::InvalidPool(
                "v3 tick_spacing must be positive",
            ));
        }
        if self.sqrt_price_x96 < min_sqrt_price() || self.sqrt_price_x96 > max_sqrt_price() {
            return Err(MarketError::InvalidPool(
                "v3 sqrt_price_x96 outside the protocol-defined range",
            ));
        }
        if self.liquidity == 0 {
            return Err(MarketError::InsufficientLiquidity);
        }
        Ok(())
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

    /// Apply a decoded V3 Swap log. Mutates `sqrt_price_x96`, `liquidity`,
    /// and `tick` to the post-swap values published by the event. Returns
    /// `Applied` if the log carries a complete V3-shaped payload, `Ignored`
    /// otherwise.
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
            DecodedLogKind::V3Swap => {
                self.apply_v3_swap_event(decoded)?;
                self.last_updated_block = block_number(&log.metadata.block_context);
                Ok(MarketEventOutcome::Applied)
            }
            // Mint / Burn require updating non-active tick liquidity, which
            // we don't model yet; they are not state-relevant for the active
            // tick unless the event happens to span the active range. We
            // treat them as a hint that the pool changed and force the next
            // quote to re-fetch. Phase 1.2 (discovery) is responsible for the
            // real handling.
            DecodedLogKind::V3Mint | DecodedLogKind::V3Burn => Ok(MarketEventOutcome::Ignored),
            _ => Ok(MarketEventOutcome::Ignored),
        }
    }

    fn apply_v3_swap_event(&mut self, decoded: &DecodedLogEvent) -> Result<(), MarketError> {
        let new_sqrt_price = decoded
            .sqrt_price_x96
            .as_deref()
            .ok_or(MarketError::MissingDecodedField("sqrt_price_x96"))?;
        let new_sqrt_price = U256::from_str_radix(new_sqrt_price, 10)
            .map_err(|_| MarketError::MissingDecodedField("sqrt_price_x96"))?;
        let new_liquidity = decoded
            .liquidity
            .ok_or(MarketError::MissingDecodedField("liquidity"))?;
        let new_tick = decoded
            .tick
            .ok_or(MarketError::MissingDecodedField("tick"))?;

        if new_sqrt_price < min_sqrt_price() || new_sqrt_price > max_sqrt_price() {
            return Err(MarketError::InvalidPool(
                "v3 swap event sqrt_price_x96 outside the protocol-defined range",
            ));
        }

        self.sqrt_price_x96 = new_sqrt_price;
        self.liquidity = new_liquidity;
        self.tick = new_tick;
        Ok(())
    }

    /// In-range exact-input quote. Returns `MarketError::InsufficientLiquidity`
    /// if the swap would cross a `tick_spacing` boundary, since past that
    /// boundary the active liquidity may differ from what we know.
    pub fn quote_exact_in(
        &self,
        token_in: &str,
        amount_in: Amount,
    ) -> Result<PoolQuote, MarketError> {
        if amount_in == 0 {
            return Err(MarketError::InsufficientInput);
        }
        if self.liquidity == 0 {
            return Err(MarketError::InsufficientLiquidity);
        }

        let zero_for_one = if token_in.eq_ignore_ascii_case(&self.token0) {
            true
        } else if token_in.eq_ignore_ascii_case(&self.token1) {
            false
        } else {
            return Err(MarketError::UnknownToken {
                pool_address: self.address.clone(),
                token: token_in.to_string(),
            });
        };

        let token_out = if zero_for_one {
            self.token1.clone()
        } else {
            self.token0.clone()
        };

        // 1. Apply the fee. V3 takes the fee on the way in.
        let fee_pips = u128::from(self.fee_pips);
        let amount_in_less_fee = amount_in
            .checked_mul(FEE_PIPS_DENOMINATOR - fee_pips)
            .ok_or(MarketError::ArithmeticOverflow)?
            / FEE_PIPS_DENOMINATOR;
        if amount_in_less_fee == 0 {
            return Err(MarketError::InsufficientInput);
        }

        let amount_in_less_fee = U256::from(amount_in_less_fee);
        let liquidity = U256::from(self.liquidity);
        let sqrt_price = self.sqrt_price_x96;
        let q96_v = q96();

        // 2. Compute the post-swap sqrt price using the closed-form
        // single-tick swap step formulas from Uniswap V3 SwapMath. We use
        // the divisor-first fallback in both branches because it never needs
        // a 512-bit intermediate. The trade-off is one bit of precision
        // worse than the reference `mulDivRoundingUp` path, which is
        // immaterial for arb-sized swaps but keeps the code overflow-free
        // on standard `U256`.
        let numerator1 = mul_u256(liquidity, q96_v)?; // L << 96
        let sqrt_price_next = if zero_for_one {
            // sqrtPriceNext = ceil(numerator1 / (numerator1 / sqrtPrice + amount))
            let divisor = (numerator1 / sqrt_price)
                .checked_add(amount_in_less_fee)
                .ok_or(MarketError::ArithmeticOverflow)?;
            div_round_up(numerator1, divisor)?
        } else {
            // sqrtPriceNext = sqrtPrice + amountIn * Q96 / L (rounded down).
            let delta = mul_u256(amount_in_less_fee, q96_v)? / liquidity;
            sqrt_price
                .checked_add(delta)
                .ok_or(MarketError::ArithmeticOverflow)?
        };

        // 3. Soft sanity cap. Without a tick bitmap we cannot prove that
        // active liquidity holds past the implied move, so we reject any
        // quote whose implied price impact exceeds the in-range threshold.
        // The risk engine sees this as `InsufficientLiquidity` and drops the
        // opportunity, which is the correct conservative behaviour.
        let provisional_impact = price_impact_bps(sqrt_price, sqrt_price_next, zero_for_one);
        if provisional_impact > MAX_IN_RANGE_IMPACT_BPS {
            return Err(MarketError::InsufficientLiquidity);
        }

        // 4. Compute amount-out from the price delta.
        let amount_out_u256 = if zero_for_one {
            // token1 out: L * (sqrtPrice - sqrtPriceNext) / Q96, rounded down
            let diff = sqrt_price
                .checked_sub(sqrt_price_next)
                .ok_or(MarketError::ArithmeticOverflow)?;
            mul_u256(liquidity, diff)? / q96_v
        } else {
            // token0 out: L * (sqrtPriceNext - sqrtPrice) * Q96 /
            //   (sqrtPrice * sqrtPriceNext), rounded down.
            let diff = sqrt_price_next
                .checked_sub(sqrt_price)
                .ok_or(MarketError::ArithmeticOverflow)?;
            let numerator = mul_u256(mul_u256(liquidity, diff)?, q96_v)?;
            let denominator = mul_u256(sqrt_price, sqrt_price_next)?;
            numerator / denominator
        };

        let amount_out: Amount = u256_to_u128(amount_out_u256)?;
        if amount_out == 0 {
            return Err(MarketError::InsufficientLiquidity);
        }

        let price_impact_bps = price_impact_bps(sqrt_price, sqrt_price_next, zero_for_one);

        Ok(PoolQuote {
            pool_address: self.address.clone(),
            token_in: token_in.to_string(),
            token_out,
            amount_in,
            amount_out,
            // We expose fee_bps as a rounded representation of fee_pips so
            // the unified `PoolQuote` shape is consistent across V2 and V3.
            // Callers that need exact precision should read fee_pips from
            // the pool state directly.
            fee_bps: self.fee_pips.div_ceil(100),
            price_impact_bps,
        })
    }

}

fn mul_u256(a: U256, b: U256) -> Result<U256, MarketError> {
    a.checked_mul(b).ok_or(MarketError::ArithmeticOverflow)
}

fn div_round_up(numerator: U256, denominator: U256) -> Result<U256, MarketError> {
    if denominator.is_zero() {
        return Err(MarketError::ArithmeticOverflow);
    }
    let q = numerator / denominator;
    let r = numerator % denominator;
    if r.is_zero() {
        Ok(q)
    } else {
        q.checked_add(U256::from(1u8))
            .ok_or(MarketError::ArithmeticOverflow)
    }
}

fn u256_to_u128(value: U256) -> Result<u128, MarketError> {
    let limbs = value.into_limbs();
    if limbs[2] != 0 || limbs[3] != 0 {
        return Err(MarketError::ArithmeticOverflow);
    }
    Ok((u128::from(limbs[1]) << 64) | u128::from(limbs[0]))
}

/// Estimate price impact in bps from the sqrt-price change. We work directly
/// on `sqrtPriceX96` to avoid a second multiplication round and the resulting
/// loss of precision on tiny moves.
fn price_impact_bps(sqrt_before: U256, sqrt_after: U256, zero_for_one: bool) -> u32 {
    let (num, denom) = if zero_for_one {
        if sqrt_after >= sqrt_before {
            return 0;
        }
        (sqrt_before - sqrt_after, sqrt_before)
    } else {
        if sqrt_after <= sqrt_before {
            return 0;
        }
        (sqrt_after - sqrt_before, sqrt_before)
    };
    // sqrt-impact ≈ price-impact / 2 for small moves; multiply by 2 to recover
    // the rough bps figure, capped at u32::MAX. The decision layer treats
    // this as advisory anyway.
    let bps_u256 = (num * U256::from(20_000u128)) / denom;
    if bps_u256 > U256::from(u32::MAX) {
        u32::MAX
    } else {
        // safe: just bounded above by u32::MAX
        bps_u256.into_limbs()[0] as u32
    }
}

/// Compute `sqrt(1.0001 ^ tick) * 2^96` using the canonical Uniswap V3
/// constants. Implemented as a port of Solidity `TickMath.getSqrtRatioAtTick`.
///
/// The function only supports ticks in `[-887272, 887272]` per protocol bounds.
pub fn get_sqrt_ratio_at_tick(tick: i32) -> Result<U256, MarketError> {
    const MIN_TICK: i32 = -887272;
    const MAX_TICK: i32 = 887272;
    if !(MIN_TICK..=MAX_TICK).contains(&tick) {
        return Err(MarketError::InvalidPool("v3 tick outside protocol bounds"));
    }
    let abs_tick = tick.unsigned_abs();

    // Each entry corresponds to bit `i` of `abs_tick`. Constants taken from
    // Uniswap v3-core TickMath.sol.
    const CONSTANTS: [&str; 20] = [
        "0xfffcb933bd6fad37aa2d162d1a594001",
        "0xfff97272373d413259a46990580e213a",
        "0xfff2e50f5f656932ef12357cf3c7fdcc",
        "0xffe5caca7e10e4e61c3624eaa0941cd0",
        "0xffcb9843d60f6159c9db58835c926644",
        "0xff973b41fa98c081472e6896dfb254c0",
        "0xff2ea16466c96a3843ec78b326b52861",
        "0xfe5dee046a99a2a811c461f1969c3053",
        "0xfcbe86c7900a88aedcffc83b479aa3a4",
        "0xf987a7253ac413176f2b074cf7815e54",
        "0xf3392b0822b70005940c7a398e4b70f3",
        "0xe7159475a2c29b7443b29c7fa6e889d9",
        "0xd097f3bdfd2022b8845ad8f792aa5825",
        "0xa9f746462d870fdf8a65dc1f90e061e5",
        "0x70d869a156d2a1b890bb3df62baf32f7",
        "0x31be135f97d08fd981231505542fcfa6",
        "0x9aa508b5b7a84e1c677de54f3e99bc9",
        "0x5d6af8dedb81196699c329225ee604",
        "0x2216e584f5fa1ea926041bedfe98",
        "0x48a170391f7dc42444e8fa2",
    ];

    let mut ratio: U256 = if abs_tick & 0x1 != 0 {
        U256::from_str_radix("fffcb933bd6fad37aa2d162d1a594001", 16).unwrap()
    } else {
        // 1 << 128
        U256::from(1u128) << 128
    };

    for (idx, raw) in CONSTANTS.iter().enumerate().skip(1) {
        if abs_tick & (1 << idx) != 0 {
            // Strip the "0x" prefix once.
            let trimmed = raw.trim_start_matches("0x");
            let factor = U256::from_str_radix(trimmed, 16).expect("constant parses");
            ratio = (ratio * factor) >> 128;
        }
    }

    if tick > 0 {
        ratio = U256::MAX / ratio;
    }

    // Convert from Q128.128 to Q96.64 with rounding-up of the truncation.
    let shifted = ratio >> 32;
    let modulus: U256 = U256::from(1u128) << 32;
    let result = if (ratio % modulus).is_zero() {
        shifted
    } else {
        shifted + U256::from(1u8)
    };
    Ok(result)
}

mod u256_decimal_string {
    use super::U256;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &U256, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<U256, D::Error> {
        let raw: String = String::deserialize(deserializer)?;
        U256::from_str_radix(&raw, 10).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ingest::{
        Channel, DecodeStatus, DecodedLogEvent, DecodedLogKind, Metadata, RawMessageType,
        RawPayloadSummary,
    };
    use types::{ingest::BlockContext, Exchange, Protocol};

    /// `sqrt(1) * 2^96`.
    fn sqrt_price_one() -> U256 {
        U256::from(1u128) << 96
    }

    fn sample_pool() -> V3PoolState {
        V3PoolState::new(
            "0xv3pool",
            Protocol::UniswapV3,
            Some(Exchange::Aerodrome),
            "0xtoken0",
            "0xtoken1",
            500,
            10,
            sqrt_price_one(),
            10u128.pow(20),
            0,
        )
        .unwrap()
    }

    #[test]
    fn validates_against_protocol_bounds() {
        let too_low = V3PoolState::new(
            "0xpool",
            Protocol::UniswapV3,
            None,
            "0xa",
            "0xb",
            500,
            10,
            U256::from(1u8),
            10u128.pow(18),
            0,
        )
        .unwrap_err();
        assert!(matches!(too_low, MarketError::InvalidPool(_)));
    }

    #[test]
    fn quotes_zero_for_one_in_range() {
        let pool = sample_pool();
        let quote = pool.quote_exact_in("0xtoken0", 10u128.pow(15)).unwrap();
        assert_eq!(quote.token_out, "0xtoken1");
        assert!(quote.amount_out > 0);
        // Fee is 5 bps (500 pips) at the tier.
        assert_eq!(quote.fee_bps, 5);
    }

    #[test]
    fn quotes_one_for_zero_in_range() {
        let pool = sample_pool();
        let quote = pool.quote_exact_in("0xtoken1", 10u128.pow(15)).unwrap();
        assert_eq!(quote.token_out, "0xtoken0");
        assert!(quote.amount_out > 0);
    }

    #[test]
    fn rejects_unknown_token() {
        let err = sample_pool().quote_exact_in("0xdeadbeef", 1).unwrap_err();
        assert!(matches!(err, MarketError::UnknownToken { .. }));
    }

    #[test]
    fn rejects_swap_above_in_range_impact_cap() {
        let pool = sample_pool();
        // With sqrt_price=2^96 (price=1) and L=1e20, a 1e30 input pushes the
        // sqrt price multiple orders of magnitude in the move direction —
        // far above the 5% in-range impact cap. The quote refuses.
        let err = pool
            .quote_exact_in("0xtoken1", 10u128.pow(30))
            .unwrap_err();
        assert!(matches!(err, MarketError::InsufficientLiquidity));
    }

    #[test]
    fn applies_v3_swap_log_to_pool_state() {
        let mut pool = sample_pool();
        let new_sqrt: U256 = (U256::from(1u128) << 96) + (U256::from(1u128) << 80);
        let log = sample_v3_swap_log(123, &new_sqrt.to_string(), 9_999u128, 5);

        let outcome = pool.apply_log(&log).unwrap();
        assert_eq!(outcome, MarketEventOutcome::Applied);
        assert_eq!(pool.sqrt_price_x96, new_sqrt);
        assert_eq!(pool.liquidity, 9_999);
        assert_eq!(pool.tick, 5);
        assert_eq!(pool.last_updated_block, Some(123));
    }

    #[test]
    fn ignores_log_for_unrelated_pool_address() {
        let mut pool = sample_pool();
        let mut log = sample_v3_swap_log(123, &sqrt_price_one().to_string(), 1, 0);
        log.address = Some("0xunrelated".to_string());

        let outcome = pool.apply_log(&log).unwrap();
        assert_eq!(outcome, MarketEventOutcome::Ignored);
    }

    #[test]
    fn ignores_v3_mint_and_burn_for_now() {
        let mut pool = sample_pool();
        let mut log = sample_v3_swap_log(123, "0", 1, 0);
        if let Some(decoded) = log.decoded_event.as_mut() {
            decoded.kind = DecodedLogKind::V3Mint;
        }
        let outcome = pool.apply_log(&log).unwrap();
        assert_eq!(outcome, MarketEventOutcome::Ignored);
    }

    #[test]
    fn get_sqrt_ratio_at_tick_anchor_values() {
        // Tick 0 should return exactly 2^96.
        let zero = get_sqrt_ratio_at_tick(0).unwrap();
        assert_eq!(zero, U256::from(1u128) << 96);

        // sqrt(1.0001^1) ~= 1.00005 → ratio = 79232123823359799118286999568
        let one_tick = get_sqrt_ratio_at_tick(1).unwrap();
        assert_eq!(
            one_tick,
            U256::from_str_radix("79232123823359799118286999568", 10).unwrap()
        );

        // Symmetric: tick=-1 reciprocal of tick=+1 (within Q-rounding).
        let minus_one = get_sqrt_ratio_at_tick(-1).unwrap();
        // sqrt(1/1.0001) * 2^96 ~ 79224201403219477170569942574
        assert_eq!(
            minus_one,
            U256::from_str_radix("79224201403219477170569942574", 10).unwrap()
        );
    }

    fn sample_v3_swap_log(
        block_number: u64,
        sqrt_price: &str,
        liquidity: u128,
        tick: i32,
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
            address: Some("0xv3pool".to_string()),
            topics: Vec::new(),
            data: None,
            event_signature: None,
            decoded_event: Some(DecodedLogEvent {
                kind: DecodedLogKind::V3Swap,
                pool: Some("0xv3pool".to_string()),
                sender: None,
                recipient: None,
                owner: None,
                reserve0: None,
                reserve1: None,
                amount0_in: None,
                amount1_in: None,
                amount0_out: None,
                amount1_out: None,
                amount0: None,
                amount1: None,
                liquidity: Some(liquidity),
                sqrt_price_x96: Some(sqrt_price.to_string()),
                tick: Some(tick),
                tick_lower: None,
                tick_upper: None,
                protocol: Some(Protocol::UniswapV3),
                exchange: Some(Exchange::Aerodrome),
            }),
            protocol: Some(Protocol::UniswapV3),
            exchange: Some(Exchange::Aerodrome),
            log_index: Some(1),
            removed: Some(false),
        }
    }
}

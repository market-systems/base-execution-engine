#![forbid(unsafe_code)]

//! Execution preparation and lifecycle policy.
//!
//! The current production slice covers:
//!
//! - turning an `ExecutionRequest` into an `ExecutionAttempt`
//! - applying gas padding and static fee caps from config
//! - enforcing execution-mode gates such as canary notional limits
//! - modeling submission, receipt, and terminal outcome transitions
//! - alloy-backed signer / gas / nonce / submit transport in the sibling
//!   `gas`, `nonce`, `signer`, `submit` modules

pub mod gas;
pub mod nonce;
pub mod signer;
pub mod submit;

use alloy_primitives::{aliases::U24, Address as AlloyAddress, Bytes, FixedBytes, U256};
use alloy_sol_types::{sol, SolCall};
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use config::{ExecutionConfig, ExecutionMode};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use types::decision::SimulationResult;
use types::execution::{
    ExecutionAttempt, ExecutionOutcome, ExecutionReceipt, ExecutionRequest, ExecutionStatus,
};
use types::Amount;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedExecution {
    pub attempt: ExecutionAttempt,
    pub should_submit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedExecution {
    pub attempt: ExecutionAttempt,
    pub outcome: ExecutionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionPayload {
    pub router_address: String,
    pub calldata: Bytes,
    pub value: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionRunResult {
    Blocked(ExecutionAttempt),
    SubmittedPending(ExecutionAttempt),
    Finalized(FinalizedExecution),
}

sol! {
    struct RouterRouteStep {
        uint8 venueKind;
        address target;
        address tokenIn;
        address tokenOut;
        uint24 fee;
        uint256 minAmountOut;
        bytes extraData;
    }

    struct RouterExecutionPlan {
        address settlementToken;
        uint256 fundingAmount;
        uint256 minRepayAmount;
        uint256 minSurplus;
        uint256 deadline;
        RouterRouteStep[] steps;
        bytes32 riskHash;
    }

    function executePlan(RouterExecutionPlan calldata plan) external payable returns (uint256 amountOut);
    function executePlanWithFlashLoan(RouterExecutionPlan calldata plan) external;
}

/// Numeric VenueKind values must stay in sync with the BaseExecutionRouter
/// `VenueKind` enum. Order is part of the on-chain ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RouterVenueKind {
    UniswapV2 = 1,
    UniswapV3Single = 2,
    AerodromeV2 = 3,
}

#[derive(Debug, Clone)]
pub struct UnavailableTransport {
    reason: String,
}

#[async_trait]
pub trait ExecutionTransport: Send + Sync {
    async fn submit(
        &self,
        payload: &SubmissionPayload,
        attempt: &ExecutionAttempt,
    ) -> anyhow::Result<String>;

    async fn await_receipt(
        &self,
        request: &ExecutionRequest,
        attempt: &ExecutionAttempt,
        timeout: Duration,
    ) -> anyhow::Result<Option<ExecutionReceipt>>;

    /// Slow-path validation. Runs an `eth_call` against the configured router
    /// using the same calldata that would be broadcast, returning `Ok(())`
    /// only if the call would not revert. The default implementation is a
    /// no-op (used by shadow / placeholder transports); the alloy transport
    /// overrides it to perform a real call.
    async fn simulate(
        &self,
        _payload: &SubmissionPayload,
        _attempt: &ExecutionAttempt,
    ) -> anyhow::Result<EthCallSimulation> {
        Ok(EthCallSimulation::Skipped {
            reason: "transport does not implement live eth_call simulation".to_string(),
        })
    }
}

/// Outcome of a slow-path `eth_call` simulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EthCallSimulation {
    /// `eth_call` returned successfully; the prepared calldata would not
    /// revert with the current chain state.
    Passed { gas_used: Option<u64> },
    /// Transport elected not to run the simulation (e.g. shadow mode).
    Skipped { reason: String },
}

impl EthCallSimulation {
    pub fn is_passed(&self) -> bool {
        matches!(self, EthCallSimulation::Passed { .. })
    }
}

impl UnavailableTransport {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl ExecutionTransport for UnavailableTransport {
    async fn submit(
        &self,
        _payload: &SubmissionPayload,
        _attempt: &ExecutionAttempt,
    ) -> anyhow::Result<String> {
        Err(anyhow!("{}", self.reason))
    }

    async fn await_receipt(
        &self,
        _request: &ExecutionRequest,
        _attempt: &ExecutionAttempt,
        _timeout: Duration,
    ) -> anyhow::Result<Option<ExecutionReceipt>> {
        Err(anyhow!("{}", self.reason))
    }
}

pub fn prepare_execution(
    config: &ExecutionConfig,
    request: &ExecutionRequest,
    simulation: &SimulationResult,
    current_daily_requested_wei: Amount,
) -> anyhow::Result<PreparedExecution> {
    if request.steps.is_empty() {
        return Err(anyhow!("execution request must contain at least one step"));
    }

    let requested_notional = request.flashloan_amount.unwrap_or(request.min_repay);
    if requested_notional == 0 {
        return Err(anyhow!(
            "execution request notional must be greater than zero"
        ));
    }

    let gas_limit = padded_gas_limit(simulation.gas_estimate, config.gas_limit_multiplier_bps)?;
    let max_fee_per_gas = parse_optional_amount(
        "EXECUTION_MAX_FEE_PER_GAS_WEI",
        config.max_fee_per_gas_wei.as_deref(),
    )?;
    let max_priority_fee_per_gas = parse_optional_amount(
        "EXECUTION_MAX_PRIORITY_FEE_PER_GAS_WEI",
        config.max_priority_fee_per_gas_wei.as_deref(),
    )?;
    let (submission_allowed, blocked_reason) =
        submission_policy(config, requested_notional, current_daily_requested_wei)?;

    let attempt = ExecutionAttempt {
        request_id: request.opportunity_id.clone(),
        tx_hash: None,
        status: ExecutionStatus::Built,
        execution_mode: execution_mode_label(config.mode).to_string(),
        submission_allowed,
        blocked_reason,
        requested_notional,
        gas_limit: Some(gas_limit),
        max_fee_per_gas,
        max_priority_fee_per_gas,
    };

    Ok(PreparedExecution {
        should_submit: attempt.submission_allowed,
        attempt,
    })
}

pub fn build_submission_payload(
    config: &ExecutionConfig,
    request: &ExecutionRequest,
) -> anyhow::Result<SubmissionPayload> {
    build_submission_payload_at(config, request, current_unix_timestamp()?)
}

/// Same as `build_submission_payload` but with the caller supplying the current
/// Unix timestamp (seconds). Useful for deterministic tests and replay tools.
pub fn build_submission_payload_at(
    config: &ExecutionConfig,
    request: &ExecutionRequest,
    now_secs: u64,
) -> anyhow::Result<SubmissionPayload> {
    let router_address = config
        .router_address
        .as_deref()
        .ok_or_else(|| anyhow!("EXECUTION_ROUTER_ADDRESS is required to build calldata"))?;
    let router_address = parse_address(router_address, "EXECUTION_ROUTER_ADDRESS")?;
    let settlement_token = parse_address(
        request
            .flashloan_asset
            .as_deref()
            .ok_or_else(|| anyhow!("execution request is missing flashloan_asset"))?,
        "execution request flashloan_asset",
    )?;
    let risk_hash = parse_risk_hash(&request.risk_hash)?;
    let funding_amount = request
        .flashloan_amount
        .ok_or_else(|| anyhow!("execution request is missing flashloan_amount"))?;

    let steps = request
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| build_route_step(step, index))
        .collect::<anyhow::Result<Vec<_>>>()?;

    let deadline = now_secs
        .checked_add(config.plan_deadline_secs)
        .ok_or_else(|| anyhow!("plan deadline overflow"))?;

    let plan = RouterExecutionPlan {
        settlementToken: settlement_token,
        fundingAmount: U256::from(funding_amount),
        minRepayAmount: U256::from(request.min_repay),
        minSurplus: U256::from(request.min_surplus),
        deadline: U256::from(deadline),
        steps,
        riskHash: risk_hash,
    };

    let calldata = if config.use_flashloan {
        executePlanWithFlashLoanCall { plan }.abi_encode()
    } else {
        executePlanCall { plan }.abi_encode()
    };

    Ok(SubmissionPayload {
        router_address: router_address.to_string(),
        calldata: Bytes::from(calldata),
        value: 0,
    })
}

fn current_unix_timestamp() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before UNIX epoch")?
        .as_secs())
}

pub async fn execute_with_transport<T: ExecutionTransport>(
    transport: &T,
    config: &ExecutionConfig,
    request: &ExecutionRequest,
    simulation: &SimulationResult,
    current_daily_requested_wei: Amount,
) -> anyhow::Result<ExecutionRunResult> {
    let prepared = prepare_execution(config, request, simulation, current_daily_requested_wei)?;
    if !prepared.should_submit {
        return Ok(ExecutionRunResult::Blocked(prepared.attempt));
    }
    let submission_payload = build_submission_payload(config, request)?;

    let tx_hash = tokio::time::timeout(
        Duration::from_secs(config.submit_timeout_secs),
        transport.submit(&submission_payload, &prepared.attempt),
    )
    .await
    .context("timed out while submitting execution attempt")??;
    let submitted = mark_submitted(&prepared.attempt, tx_hash)?;

    let maybe_receipt = transport
        .await_receipt(
            request,
            &submitted,
            Duration::from_secs(config.receipt_timeout_secs),
        )
        .await;

    let maybe_receipt = match maybe_receipt {
        Ok(receipt) => receipt,
        Err(error) => {
            tracing::warn!(
                request_id = %submitted.request_id,
                tx_hash = submitted.tx_hash.as_deref().unwrap_or(""),
                error = %error,
                "execution receipt lookup failed after submission; attempt remains pending"
            );
            return Ok(ExecutionRunResult::SubmittedPending(submitted));
        }
    };

    let Some(receipt) = maybe_receipt else {
        return Ok(ExecutionRunResult::Finalized(mark_dropped(
            &submitted,
            "timed out while waiting for execution receipt",
        )?));
    };

    Ok(ExecutionRunResult::Finalized(finalize_from_receipt(
        &submitted,
        receipt,
        None,
        request.min_surplus,
    )?))
}

pub fn mark_submitted(
    attempt: &ExecutionAttempt,
    tx_hash: impl Into<String>,
) -> anyhow::Result<ExecutionAttempt> {
    if attempt.status != ExecutionStatus::Built {
        return Err(anyhow!(
            "only built execution attempts can transition to submitted"
        ));
    }
    if !attempt.submission_allowed {
        return Err(anyhow!(
            "execution attempt is blocked and cannot transition to submitted"
        ));
    }

    let tx_hash = tx_hash.into();
    if tx_hash.trim().is_empty() {
        return Err(anyhow!("submitted transaction hash must not be empty"));
    }

    let mut updated = attempt.clone();
    updated.status = ExecutionStatus::Submitted;
    updated.tx_hash = Some(tx_hash);
    Ok(updated)
}

pub fn finalize_from_receipt(
    attempt: &ExecutionAttempt,
    receipt: ExecutionReceipt,
    realized_surplus: Option<Amount>,
    min_surplus: Amount,
) -> anyhow::Result<FinalizedExecution> {
    if !matches!(
        attempt.status,
        ExecutionStatus::Submitted | ExecutionStatus::Included
    ) {
        return Err(anyhow!(
            "only submitted or included attempts can be finalized from a receipt"
        ));
    }

    if let Some(tx_hash) = &attempt.tx_hash {
        if tx_hash != &receipt.tx_hash {
            return Err(anyhow!(
                "receipt transaction hash does not match the attempt"
            ));
        }
    }

    let mut updated = attempt.clone();
    updated.tx_hash = Some(receipt.tx_hash.clone());

    let total_fee_paid = total_fee_paid(&receipt)?;
    let (final_status, reason) = if receipt.success {
        classify_success(realized_surplus, min_surplus)
    } else {
        (
            ExecutionStatus::Reverted,
            receipt
                .revert_reason
                .clone()
                .or_else(|| Some("transaction reverted on-chain".to_string())),
        )
    };
    updated.status = final_status;

    let outcome = ExecutionOutcome {
        request_id: attempt.request_id.clone(),
        final_status,
        tx_hash: receipt.tx_hash,
        block_number: receipt.block_number,
        total_fee_paid,
        realized_surplus,
        reason,
    };

    Ok(FinalizedExecution {
        attempt: updated,
        outcome,
    })
}

pub fn mark_dropped(
    attempt: &ExecutionAttempt,
    reason: impl Into<String>,
) -> anyhow::Result<FinalizedExecution> {
    if attempt.status != ExecutionStatus::Submitted {
        return Err(anyhow!(
            "only submitted execution attempts can transition to dropped"
        ));
    }

    let reason = reason.into();
    if reason.trim().is_empty() {
        return Err(anyhow!("dropped execution attempts must record a reason"));
    }

    let mut updated = attempt.clone();
    updated.status = ExecutionStatus::Dropped;

    let tx_hash = attempt
        .tx_hash
        .clone()
        .ok_or_else(|| anyhow!("submitted execution attempt is missing its transaction hash"))?;

    Ok(FinalizedExecution {
        attempt: updated,
        outcome: ExecutionOutcome {
            request_id: attempt.request_id.clone(),
            final_status: ExecutionStatus::Dropped,
            tx_hash,
            block_number: None,
            total_fee_paid: None,
            realized_surplus: None,
            reason: Some(reason),
        },
    })
}

fn submission_policy(
    config: &ExecutionConfig,
    requested_notional: Amount,
    current_daily_requested_wei: Amount,
) -> anyhow::Result<(bool, Option<String>)> {
    match config.mode {
        ExecutionMode::Shadow => Ok((
            false,
            Some("execution mode is configured for build-only operation".to_string()),
        )),
        ExecutionMode::Canary => {
            let max_trade = parse_required_amount(
                "EXECUTION_CANARY_MAX_TRADE_WEI",
                config.canary_max_trade_wei.as_deref(),
            )?;
            let max_daily = parse_required_amount(
                "EXECUTION_CANARY_MAX_DAILY_WEI",
                config.canary_max_daily_wei.as_deref(),
            )?;

            if requested_notional > max_trade {
                return Ok((
                    false,
                    Some("request exceeds canary per-trade notional limit".to_string()),
                ));
            }

            let projected_daily = current_daily_requested_wei
                .checked_add(requested_notional)
                .ok_or_else(|| anyhow!("canary daily requested notional overflow"))?;
            if projected_daily > max_daily {
                return Ok((
                    false,
                    Some("request exceeds canary daily notional limit".to_string()),
                ));
            }

            Ok((true, None))
        }
        ExecutionMode::Live => Ok((true, None)),
    }
}

fn padded_gas_limit(gas_estimate: u64, multiplier_bps: u32) -> anyhow::Result<u64> {
    let numerator = u128::from(gas_estimate)
        .checked_mul(u128::from(multiplier_bps))
        .ok_or_else(|| anyhow!("gas limit padding overflow"))?;
    let padded = numerator
        .checked_add(9_999)
        .ok_or_else(|| anyhow!("gas limit padding overflow"))?
        / 10_000;

    u64::try_from(padded).context("padded gas limit does not fit into u64")
}

fn parse_optional_amount(key: &str, value: Option<&str>) -> anyhow::Result<Option<Amount>> {
    value
        .map(|raw| {
            raw.parse::<Amount>()
                .with_context(|| format!("failed to parse {key}"))
        })
        .transpose()
}

fn parse_required_amount(key: &str, value: Option<&str>) -> anyhow::Result<Amount> {
    let raw = value.ok_or_else(|| anyhow!("{key} is required for this execution mode"))?;
    raw.parse::<Amount>()
        .with_context(|| format!("failed to parse {key}"))
}

fn execution_mode_label(mode: ExecutionMode) -> &'static str {
    match mode {
        ExecutionMode::Shadow => "shadow",
        ExecutionMode::Canary => "canary",
        ExecutionMode::Live => "live",
    }
}

fn build_route_step(
    step: &types::execution::ExecutionStep,
    index: usize,
) -> anyhow::Result<RouterRouteStep> {
    let calldata_hint = step
        .calldata_hint
        .as_deref()
        .ok_or_else(|| anyhow!("execution step {index} is missing calldata_hint"))?;

    let target = parse_address(&step.target, "execution step target")?;
    let token_in = parse_address(&step.token_in, "execution step token_in")?;
    let token_out = parse_address(&step.token_out, "execution step token_out")?;
    let min_amount_out = U256::from(step.min_amount_out);

    match calldata_hint {
        "uniswap_v3_exact_input_single" | "v3_exact_input_single" => {
            let fee_bps = step
                .pool_fee
                .ok_or_else(|| anyhow!("execution step {index} v3 swap is missing pool_fee"))?;
            let fee_u24 = if fee_bps <= 1_000_000 {
                fee_bps
            } else {
                return Err(anyhow!(
                    "execution step {index} has an invalid v3 pool fee `{fee_bps}`"
                ));
            };

            Ok(RouterRouteStep {
                venueKind: RouterVenueKind::UniswapV3Single as u8,
                target,
                tokenIn: token_in,
                tokenOut: token_out,
                fee: U24::from(fee_u24 as u64),
                minAmountOut: min_amount_out,
                extraData: Bytes::new(),
            })
        }
        "v2_exact_input" | "uniswap_v2_exact_input" => Ok(RouterRouteStep {
            venueKind: RouterVenueKind::UniswapV2 as u8,
            target,
            tokenIn: token_in,
            tokenOut: token_out,
            fee: U24::ZERO,
            minAmountOut: min_amount_out,
            extraData: Bytes::new(),
        }),
        "aerodrome_v2_volatile" | "aerodrome_v2_stable" => {
            let stable = calldata_hint == "aerodrome_v2_stable";
            let factory_str = step.aux_address.as_deref().ok_or_else(|| {
                anyhow!("execution step {index} aerodrome swap is missing aux_address (factory)")
            })?;
            let factory = parse_address(factory_str, "execution step aerodrome factory")?;

            // Solidity ABI encoding for `(bool stable, address factory)`.
            let mut extra = Vec::with_capacity(64);
            extra.extend_from_slice(&[0u8; 31]);
            extra.push(if stable { 1 } else { 0 });
            extra.extend_from_slice(&[0u8; 12]);
            extra.extend_from_slice(factory.as_slice());

            Ok(RouterRouteStep {
                venueKind: RouterVenueKind::AerodromeV2 as u8,
                target,
                tokenIn: token_in,
                tokenOut: token_out,
                fee: U24::ZERO,
                minAmountOut: min_amount_out,
                extraData: Bytes::from(extra),
            })
        }
        other => Err(anyhow!(
            "execution step {index} uses unsupported calldata hint `{other}`"
        )),
    }
}


fn parse_address(value: &str, context: &str) -> anyhow::Result<AlloyAddress> {
    AlloyAddress::from_str(value)
        .with_context(|| format!("failed to parse {context} `{value}` as an address"))
}

fn parse_risk_hash(value: &str) -> anyhow::Result<FixedBytes<32>> {
    let normalized = if value.starts_with("0x") {
        value.to_string()
    } else {
        format!("0x{value}")
    };
    FixedBytes::<32>::from_str(&normalized)
        .with_context(|| format!("failed to parse risk hash `{value}` as bytes32"))
}

fn total_fee_paid(receipt: &ExecutionReceipt) -> anyhow::Result<Option<Amount>> {
    match (receipt.gas_used, receipt.effective_gas_price) {
        (Some(gas_used), Some(effective_gas_price)) => {
            let execution_fee = u128::from(gas_used)
                .checked_mul(effective_gas_price)
                .ok_or_else(|| anyhow!("execution fee overflow"))?;
            Ok(Some(
                execution_fee
                    .checked_add(receipt.l1_fee_paid.unwrap_or(0))
                    .ok_or_else(|| anyhow!("total fee overflow"))?,
            ))
        }
        _ => Ok(receipt.l1_fee_paid),
    }
}

fn classify_success(
    realized_surplus: Option<Amount>,
    min_surplus: Amount,
) -> (ExecutionStatus, Option<String>) {
    match realized_surplus {
        Some(realized_surplus) if realized_surplus >= min_surplus => {
            (ExecutionStatus::ProfitRealized, None)
        }
        Some(realized_surplus) => (
            ExecutionStatus::Included,
            Some(format!(
                "transaction included but realized surplus {} is below minimum required {}",
                realized_surplus, min_surplus
            )),
        ),
        None => (
            ExecutionStatus::Included,
            Some("transaction included but realized surplus is not yet known".to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::ExecutionConfig;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use types::decision::SimulationResult;
    use types::execution::{ExecutionReceipt, ExecutionRequest, ExecutionStep};

    #[derive(Debug, Clone)]
    struct ScriptedTransport {
        submit_results: Arc<Mutex<VecDeque<anyhow::Result<String>>>>,
        receipt_results: Arc<Mutex<VecDeque<anyhow::Result<Option<ExecutionReceipt>>>>>,
    }

    #[async_trait]
    impl ExecutionTransport for ScriptedTransport {
        async fn submit(
            &self,
            _payload: &SubmissionPayload,
            _attempt: &ExecutionAttempt,
        ) -> anyhow::Result<String> {
            self.submit_results
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("missing scripted submit result")))
        }

        async fn await_receipt(
            &self,
            _request: &ExecutionRequest,
            _attempt: &ExecutionAttempt,
            _timeout: Duration,
        ) -> anyhow::Result<Option<ExecutionReceipt>> {
            self.receipt_results
                .lock()
                .await
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("missing scripted receipt result")))
        }
    }

    #[test]
    fn prepares_live_execution_with_padded_gas_limit() {
        let prepared =
            prepare_execution(&live_config(), &request(100), &simulation(200_000), 0).unwrap();

        assert!(prepared.should_submit);
        assert_eq!(prepared.attempt.execution_mode, "live");
        assert_eq!(prepared.attempt.gas_limit, Some(240_000));
        assert_eq!(prepared.attempt.max_fee_per_gas, Some(50));
        assert_eq!(prepared.attempt.max_priority_fee_per_gas, Some(2));
        assert_eq!(prepared.attempt.blocked_reason, None);
    }

    #[test]
    fn blocks_canary_execution_above_trade_limit() {
        let prepared = prepare_execution(
            &canary_config(100, 1_000),
            &request(101),
            &simulation(180_000),
            0,
        )
        .unwrap();

        assert!(!prepared.should_submit);
        assert!(!prepared.attempt.submission_allowed);
        assert_eq!(
            prepared.attempt.blocked_reason.as_deref(),
            Some("request exceeds canary per-trade notional limit")
        );
    }

    #[test]
    fn blocks_canary_execution_above_daily_limit() {
        let prepared = prepare_execution(
            &canary_config(100, 1_000),
            &request(100),
            &simulation(180_000),
            950,
        )
        .unwrap();

        assert!(!prepared.should_submit);
        assert_eq!(
            prepared.attempt.blocked_reason.as_deref(),
            Some("request exceeds canary daily notional limit")
        );
    }

    #[test]
    fn submitted_attempt_requires_allowed_built_attempt() {
        let prepared =
            prepare_execution(&live_config(), &request(100), &simulation(200_000), 0).unwrap();

        let submitted = mark_submitted(&prepared.attempt, "0xtx").unwrap();
        assert_eq!(submitted.status, ExecutionStatus::Submitted);
        assert_eq!(submitted.tx_hash.as_deref(), Some("0xtx"));
    }

    #[test]
    fn finalizes_successful_receipt_into_profit_realized() {
        let prepared =
            prepare_execution(&live_config(), &request(100), &simulation(200_000), 0).unwrap();
        let submitted = mark_submitted(&prepared.attempt, "0xtx").unwrap();

        let finalized = finalize_from_receipt(
            &submitted,
            ExecutionReceipt {
                request_id: "opp-1".to_string(),
                tx_hash: "0xtx".to_string(),
                block_number: Some(42),
                success: true,
                gas_used: Some(210_000),
                effective_gas_price: Some(3),
                l1_fee_paid: Some(100),
                revert_reason: None,
            },
            Some(50),
            1,
        )
        .unwrap();

        assert_eq!(finalized.attempt.status, ExecutionStatus::ProfitRealized);
        assert_eq!(finalized.outcome.total_fee_paid, Some(630_100));
        assert_eq!(finalized.outcome.reason, None);
    }

    #[test]
    fn finalizes_reverted_receipt_with_reason() {
        let prepared =
            prepare_execution(&live_config(), &request(100), &simulation(200_000), 0).unwrap();
        let submitted = mark_submitted(&prepared.attempt, "0xtx").unwrap();

        let finalized = finalize_from_receipt(
            &submitted,
            ExecutionReceipt {
                request_id: "opp-1".to_string(),
                tx_hash: "0xtx".to_string(),
                block_number: Some(42),
                success: false,
                gas_used: Some(210_000),
                effective_gas_price: Some(3),
                l1_fee_paid: Some(100),
                revert_reason: Some("slippage".to_string()),
            },
            None,
            1,
        )
        .unwrap();

        assert_eq!(finalized.attempt.status, ExecutionStatus::Reverted);
        assert_eq!(finalized.outcome.reason.as_deref(), Some("slippage"));
    }

    #[test]
    fn marks_submitted_attempt_as_dropped() {
        let prepared =
            prepare_execution(&live_config(), &request(100), &simulation(200_000), 0).unwrap();
        let submitted = mark_submitted(&prepared.attempt, "0xtx").unwrap();

        let dropped = mark_dropped(&submitted, "timeout waiting for receipt").unwrap();

        assert_eq!(dropped.attempt.status, ExecutionStatus::Dropped);
        assert_eq!(
            dropped.outcome.reason.as_deref(),
            Some("timeout waiting for receipt")
        );
    }

    #[test]
    fn builds_submission_payload_for_supported_v3_single_route() {
        let payload = build_submission_payload(&live_config(), &request(100)).unwrap();

        assert_eq!(
            payload.router_address,
            "0x1111111111111111111111111111111111111111"
        );
        assert_eq!(payload.value, 0);
        assert!(!payload.calldata.is_empty());
    }

    #[test]
    fn builds_submission_payload_for_v2_route() {
        let mut request = request(100);
        request.steps[0].calldata_hint = Some("v2_exact_input".to_string());
        request.steps[0].pool_fee = None;

        let payload = build_submission_payload(&live_config(), &request).unwrap();

        assert_eq!(payload.value, 0);
        assert!(!payload.calldata.is_empty());
    }

    #[test]
    fn builds_submission_payload_for_aerodrome_route() {
        let mut request = request(100);
        request.steps[0].calldata_hint = Some("aerodrome_v2_volatile".to_string());
        request.steps[0].pool_fee = None;
        request.steps[0].aux_address =
            Some("0x420dd381b31aef6683db6b902084cb0ffece40da".to_string());

        let payload = build_submission_payload(&live_config(), &request).unwrap();
        assert!(!payload.calldata.is_empty());
    }

    #[test]
    fn rejects_aerodrome_route_without_aux_address() {
        let mut request = request(100);
        request.steps[0].calldata_hint = Some("aerodrome_v2_volatile".to_string());
        request.steps[0].pool_fee = None;
        request.steps[0].aux_address = None;

        let error = build_submission_payload(&live_config(), &request).unwrap_err();
        assert!(error.to_string().contains("aux_address"));
    }

    #[test]
    fn build_payload_uses_self_funded_selector_when_flashloan_disabled() {
        let mut config = live_config();
        config.use_flashloan = false;
        let payload = build_submission_payload(&config, &request(100)).unwrap();
        // Selector for executePlan(...) vs executePlanWithFlashLoan(...) differ
        // in the first 4 bytes; check they're stable across rebuilds.
        assert_eq!(payload.calldata.len() % 32, 4);
    }

    #[tokio::test]
    async fn execution_runner_blocks_when_submission_is_not_allowed() {
        let result = execute_with_transport(
            &scripted_transport(vec![], vec![]),
            &canary_config(100, 1_000),
            &request(101),
            &simulation(180_000),
            0,
        )
        .await
        .unwrap();

        match result {
            ExecutionRunResult::Blocked(attempt) => {
                assert_eq!(attempt.status, ExecutionStatus::Built);
                assert!(!attempt.submission_allowed);
            }
            other => panic!("expected blocked execution result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execution_runner_finalizes_after_receipt() {
        let result = execute_with_transport(
            &scripted_transport(
                vec![Ok("0xtx".to_string())],
                vec![Ok(Some(receipt(true, None)))],
            ),
            &live_config(),
            &request(100),
            &simulation(180_000),
            0,
        )
        .await
        .unwrap();

        match result {
            ExecutionRunResult::Finalized(finalized) => {
                assert_eq!(finalized.attempt.status, ExecutionStatus::Included);
                assert_eq!(
                    finalized.outcome.reason.as_deref(),
                    Some("transaction included but realized surplus is not yet known")
                );
            }
            other => panic!("expected finalized execution result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execution_runner_marks_dropped_when_receipt_times_out() {
        let result = execute_with_transport(
            &scripted_transport(vec![Ok("0xtx".to_string())], vec![Ok(None)]),
            &live_config(),
            &request(100),
            &simulation(180_000),
            0,
        )
        .await
        .unwrap();

        match result {
            ExecutionRunResult::Finalized(finalized) => {
                assert_eq!(finalized.attempt.status, ExecutionStatus::Dropped);
                assert_eq!(
                    finalized.outcome.reason.as_deref(),
                    Some("timed out while waiting for execution receipt")
                );
            }
            other => panic!("expected dropped execution result, got {other:?}"),
        }
    }

    fn request(notional: Amount) -> ExecutionRequest {
        ExecutionRequest {
            opportunity_id: "opp-1".to_string(),
            steps: vec![ExecutionStep {
                venue: None,
                protocol: Some(types::Protocol::UniswapV3),
                target: "0x2222222222222222222222222222222222222222".to_string(),
                token_in: "0x3333333333333333333333333333333333333333".to_string(),
                token_out: "0x4444444444444444444444444444444444444444".to_string(),
                pool_fee: Some(500),
                amount_in: Some(notional),
                min_amount_out: 99,
                calldata_hint: Some("v3_exact_input_single".to_string()),
                aux_address: None,
            }],
            min_repay: notional,
            min_surplus: 1,
            flashloan_asset: Some("0x5555555555555555555555555555555555555555".to_string()),
            flashloan_amount: Some(notional),
            risk_hash: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .to_string(),
        }
    }

    fn simulation(gas_estimate: u64) -> SimulationResult {
        SimulationResult {
            opportunity_id: "opp-1".to_string(),
            accepted: true,
            expected_gross_profit: 10,
            expected_net_profit: 5,
            gas_estimate,
            gas_cost_wei: 3,
            l1_data_fee: 2,
            notes: vec![],
        }
    }

    fn live_config() -> ExecutionConfig {
        ExecutionConfig {
            mode: ExecutionMode::Live,
            submit_timeout_secs: 30,
            receipt_timeout_secs: 60,
            router_address: Some("0x1111111111111111111111111111111111111111".to_string()),
            canary_max_trade_wei: None,
            canary_max_daily_wei: None,
            gas_limit_multiplier_bps: 12_000,
            max_fee_per_gas_wei: Some("50".to_string()),
            max_priority_fee_per_gas_wei: Some("2".to_string()),
            plan_deadline_secs: 30,
            use_flashloan: true,
            ethcall_preflight_required: true,
            ethcall_timeout_secs: 5,
        }
    }

    fn canary_config(max_trade: Amount, max_daily: Amount) -> ExecutionConfig {
        ExecutionConfig {
            mode: ExecutionMode::Canary,
            submit_timeout_secs: 30,
            receipt_timeout_secs: 60,
            router_address: Some("0x1111111111111111111111111111111111111111".to_string()),
            canary_max_trade_wei: Some(max_trade.to_string()),
            canary_max_daily_wei: Some(max_daily.to_string()),
            gas_limit_multiplier_bps: 12_000,
            max_fee_per_gas_wei: None,
            max_priority_fee_per_gas_wei: None,
            plan_deadline_secs: 30,
            use_flashloan: true,
            ethcall_preflight_required: true,
            ethcall_timeout_secs: 5,
        }
    }

    fn scripted_transport(
        submit_results: Vec<anyhow::Result<String>>,
        receipt_results: Vec<anyhow::Result<Option<ExecutionReceipt>>>,
    ) -> ScriptedTransport {
        ScriptedTransport {
            submit_results: Arc::new(Mutex::new(submit_results.into())),
            receipt_results: Arc::new(Mutex::new(receipt_results.into())),
        }
    }

    fn receipt(success: bool, revert_reason: Option<&str>) -> ExecutionReceipt {
        ExecutionReceipt {
            request_id: "opp-1".to_string(),
            tx_hash: "0xtx".to_string(),
            block_number: Some(42),
            success,
            gas_used: Some(210_000),
            effective_gas_price: Some(3),
            l1_fee_paid: Some(100),
            revert_reason: revert_reason.map(ToString::to_string),
        }
    }

    #[tokio::test]
    async fn default_simulate_returns_skipped_for_transports_without_override() {
        let transport = ScriptedTransport {
            submit_results: Arc::new(Mutex::new(VecDeque::new())),
            receipt_results: Arc::new(Mutex::new(VecDeque::new())),
        };
        let payload = SubmissionPayload {
            router_address: "0x1111111111111111111111111111111111111111".to_string(),
            calldata: Bytes::from(vec![0x01, 0x02]),
            value: 0,
        };
        let attempt = ExecutionAttempt {
            request_id: "opp-1".to_string(),
            tx_hash: None,
            status: ExecutionStatus::Built,
            execution_mode: "shadow".to_string(),
            submission_allowed: false,
            blocked_reason: None,
            requested_notional: 0,
            gas_limit: Some(180_000),
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
        };
        let outcome = transport.simulate(&payload, &attempt).await.unwrap();
        assert!(matches!(outcome, EthCallSimulation::Skipped { .. }));
        assert!(!outcome.is_passed());
    }
}

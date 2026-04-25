//! Risk policy engine.
//!
//! `RiskEngine` couples a stateless [`RiskPolicy`] (immutable view of
//! [`config::RiskConfig`]) with the runtime state needed to enforce
//! per-block / per-day notional caps, consecutive-revert circuit breakers,
//! and an operator-controlled kill-switch file.
//!
//! Every opportunity goes through three layers in `assess`:
//!
//! 1. Hard gates — kill switch, denylist, allowlist, slippage ceiling, profit
//!    floors. These cannot be soft-overridden.
//! 2. Notional caps — per-trade, per-block, daily rolling. Per-trade is a
//!    pure pre-check; the latter two count "committed but not yet reverted"
//!    notional.
//! 3. Circuit breaker — the engine stops accepting new opportunities once
//!    `max_consecutive_reverts` is reached. The breaker is reset on the next
//!    successful settlement.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use config::RiskConfig;
use sha2::{Digest, Sha256};
use types::decision::{Opportunity, RiskDecision, SimulationResult};
use types::ingest::BlockContext;
use types::{Amount, BlockNumber};

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct RiskPolicy {
    pub min_net_profit_wei: Amount,
    pub min_profit_bps: u32,
    pub max_trade_notional_wei: Option<Amount>,
    pub max_per_block_notional_wei: Option<Amount>,
    pub max_daily_notional_wei: Option<Amount>,
    pub max_slippage_bps: u32,
    pub token_allowlist: HashSet<String>,
    pub token_denylist: HashSet<String>,
    pub max_consecutive_reverts: u32,
    pub kill_switch_path: Option<PathBuf>,
}

impl RiskPolicy {
    pub fn from_config(config: &RiskConfig) -> anyhow::Result<Self> {
        let min_net_profit_wei = config
            .min_net_profit_wei
            .parse::<Amount>()
            .map_err(|err| anyhow::anyhow!("invalid RISK_MIN_NET_PROFIT_WEI: {err}"))?;

        let max_trade_notional_wei = config
            .max_trade_notional_wei
            .as_deref()
            .map(parse_amount)
            .transpose()?;
        let max_per_block_notional_wei = config
            .max_per_block_notional_wei
            .as_deref()
            .map(parse_amount)
            .transpose()?;
        let max_daily_notional_wei = config
            .max_daily_notional_wei
            .as_deref()
            .map(parse_amount)
            .transpose()?;

        Ok(Self {
            min_net_profit_wei,
            min_profit_bps: config.min_profit_bps,
            max_trade_notional_wei,
            max_per_block_notional_wei,
            max_daily_notional_wei,
            max_slippage_bps: config.max_slippage_bps,
            token_allowlist: config
                .token_allowlist
                .iter()
                .map(|addr| addr.to_ascii_lowercase())
                .collect(),
            token_denylist: config
                .token_denylist
                .iter()
                .map(|addr| addr.to_ascii_lowercase())
                .collect(),
            max_consecutive_reverts: config.max_consecutive_reverts,
            kill_switch_path: config.kill_switch_path.as_deref().map(PathBuf::from),
        })
    }
}

fn parse_amount(raw: &str) -> anyhow::Result<Amount> {
    raw.parse::<Amount>()
        .map_err(|err| anyhow::anyhow!("invalid amount `{raw}`: {err}"))
}

/// Source of "now" used by the daily rolling window. Tests inject a fake clock
/// to keep the rolling-window logic deterministic without sleeping.
pub trait Clock: Send + Sync {
    fn now_unix_secs(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_secs(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

#[derive(Debug, Default)]
struct RiskState {
    /// (block_number, total notional committed in this block). The block
    /// number is `BlockNumber::MAX` when the opportunity carries a pending
    /// Flashblocks context, in which case the per-block cap collapses onto
    /// the current preconfirm window.
    block_notional: Option<(BlockNumber, Amount)>,
    /// FIFO of (unix_seconds, notional) entries inside the rolling daily window.
    daily_entries: std::collections::VecDeque<(u64, Amount)>,
    daily_total: Amount,
    consecutive_reverts: u32,
    operator_killed: bool,
}

pub struct RiskEngine {
    policy: RiskPolicy,
    clock: Box<dyn Clock>,
    state: Mutex<RiskState>,
}

impl std::fmt::Debug for RiskEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RiskEngine")
            .field("policy", &self.policy)
            .finish()
    }
}

impl RiskEngine {
    pub fn new(policy: RiskPolicy) -> Self {
        Self::with_clock(policy, Box::new(SystemClock))
    }

    pub fn with_clock(policy: RiskPolicy, clock: Box<dyn Clock>) -> Self {
        Self {
            policy,
            clock,
            state: Mutex::new(RiskState::default()),
        }
    }

    pub fn policy(&self) -> &RiskPolicy {
        &self.policy
    }

    /// Force the kill switch on. Used by ops to halt trading from a sibling
    /// process; the file-based switch is checked on every assessment too.
    pub fn trigger_kill_switch(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.operator_killed = true;
        }
    }

    /// Record the outcome of an executed opportunity.
    /// `success = true` resets the revert counter; `false` increments it and
    /// trips the breaker once `max_consecutive_reverts` is reached.
    pub fn record_outcome(&self, success: bool) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if success {
            state.consecutive_reverts = 0;
        } else {
            state.consecutive_reverts = state.consecutive_reverts.saturating_add(1);
        }
    }

    /// Stateless plus stateful assessment. Returns a `RiskDecision` with
    /// `accepted = false` and a populated `reason` when any gate trips.
    /// Stateful counters (block / daily notional) are updated only when the
    /// returned decision is `accepted`.
    pub fn assess(
        &self,
        opportunity: &Opportunity,
        simulation: &SimulationResult,
    ) -> RiskDecision {
        let block_number = match &opportunity.block_context {
            BlockContext::Block { number, .. } => *number,
            // Pending block context: collapse onto a sentinel "pending" bucket
            // so per-block caps still apply across multiple preconfirm flashes.
            BlockContext::Pending => BlockNumber::MAX,
        };
        let now = self.clock.now_unix_secs();

        if let Some(reason) = self.stateless_reject(opportunity, simulation) {
            return self.deny(opportunity, reason, simulation);
        }

        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };

        if state.operator_killed {
            return self.deny(
                opportunity,
                "operator kill switch is engaged".to_string(),
                simulation,
            );
        }

        if state.consecutive_reverts >= self.policy.max_consecutive_reverts
            && self.policy.max_consecutive_reverts > 0
        {
            return self.deny(
                opportunity,
                format!(
                    "circuit breaker tripped after {} consecutive reverts",
                    state.consecutive_reverts
                ),
                simulation,
            );
        }

        // Per-block cap.
        let block_used = match state.block_notional {
            Some((current_block, used)) if current_block == block_number => used,
            _ => 0,
        };
        if let Some(cap) = self.policy.max_per_block_notional_wei {
            let projected = block_used.saturating_add(opportunity.input_amount);
            if projected > cap {
                return self.deny(
                    opportunity,
                    format!(
                        "per-block notional {projected} would exceed cap {cap}"
                    ),
                    simulation,
                );
            }
        }

        // Daily rolling window.
        prune_daily_entries(&mut state, now);
        if let Some(cap) = self.policy.max_daily_notional_wei {
            let projected = state.daily_total.saturating_add(opportunity.input_amount);
            if projected > cap {
                return self.deny(
                    opportunity,
                    format!("daily notional {projected} would exceed cap {cap}"),
                    simulation,
                );
            }
        }

        // Commit the notional usage; this is reversible only via outcome
        // accounting (we deliberately do NOT decrement on revert because the
        // reverted call still consumed gas and bandwidth, and the spec wants a
        // strict "used" counter, not a "P&L" counter).
        let new_block_used = block_used.saturating_add(opportunity.input_amount);
        state.block_notional = Some((block_number, new_block_used));
        state.daily_entries.push_back((now, opportunity.input_amount));
        state.daily_total = state.daily_total.saturating_add(opportunity.input_amount);

        let min_surplus = self.policy.min_net_profit_wei;
        RiskDecision {
            opportunity_id: opportunity.id.clone(),
            accepted: true,
            reason: None,
            risk_hash: risk_hash(opportunity, simulation, min_surplus),
            min_surplus,
        }
    }

    /// Run only the stateless gates. Returns `Some(reason)` on rejection.
    /// Exposed publicly so ad-hoc tools (replays, simulations) can reuse the
    /// policy without touching the engine's stateful counters.
    pub fn stateless_reject(
        &self,
        opportunity: &Opportunity,
        simulation: &SimulationResult,
    ) -> Option<String> {
        if let Some(path) = &self.policy.kill_switch_path {
            if path.exists() {
                return Some(format!(
                    "kill switch file is present at {}",
                    path.display()
                ));
            }
        }

        if !simulation.accepted {
            return Some("simulation rejected the opportunity".to_string());
        }

        if simulation.expected_net_profit < self.policy.min_net_profit_wei {
            return Some(format!(
                "expected net profit {} is below the minimum {}",
                simulation.expected_net_profit, self.policy.min_net_profit_wei
            ));
        }

        if self.policy.min_profit_bps > 0 && opportunity.input_amount > 0 {
            // bps over notional. Use u128 math; with Amount = u128 this can
            // overflow only for absurd notionals, so we saturate defensively.
            let bps_threshold = (opportunity.input_amount.saturating_mul(
                self.policy.min_profit_bps as u128,
            )) / 10_000;
            if simulation.expected_net_profit < bps_threshold {
                return Some(format!(
                    "expected net profit {} is below {} bps of notional ({})",
                    simulation.expected_net_profit, self.policy.min_profit_bps, bps_threshold
                ));
            }
        }

        if let Some(cap) = self.policy.max_trade_notional_wei {
            if opportunity.input_amount > cap {
                return Some(format!(
                    "trade notional {} exceeds per-trade cap {cap}",
                    opportunity.input_amount
                ));
            }
        }

        let route_tokens = opportunity_token_iter(opportunity);
        for token in route_tokens.iter() {
            let normalized = token.to_ascii_lowercase();
            if self.policy.token_denylist.contains(&normalized) {
                return Some(format!("token {token} is on the denylist"));
            }
            if !self.policy.token_allowlist.is_empty()
                && !self.policy.token_allowlist.contains(&normalized)
            {
                return Some(format!("token {token} is not on the allowlist"));
            }
        }

        if self.policy.max_slippage_bps < 10_000 {
            // The aggregate path slippage is `1 - expected_out / input`. We
            // approximate that by comparing expected_output_amount against
            // input_amount; it's a conservative check appropriate for cyclic
            // routes (where output should be >= input + profit floor).
            if opportunity.expected_output_amount < opportunity.input_amount {
                let shortfall = opportunity
                    .input_amount
                    .saturating_sub(opportunity.expected_output_amount);
                let cap_value = (opportunity.input_amount.saturating_mul(
                    self.policy.max_slippage_bps as u128,
                )) / 10_000;
                if shortfall > cap_value {
                    return Some(format!(
                        "path shortfall {shortfall} exceeds {} bps of notional",
                        self.policy.max_slippage_bps
                    ));
                }
            }
        }

        None
    }

    fn deny(
        &self,
        opportunity: &Opportunity,
        reason: String,
        simulation: &SimulationResult,
    ) -> RiskDecision {
        let min_surplus = self.policy.min_net_profit_wei;
        RiskDecision {
            opportunity_id: opportunity.id.clone(),
            accepted: false,
            reason: Some(reason),
            risk_hash: risk_hash(opportunity, simulation, min_surplus),
            min_surplus,
        }
    }
}

fn prune_daily_entries(state: &mut RiskState, now: u64) {
    let horizon = now.saturating_sub(SECONDS_PER_DAY);
    while let Some(&(ts, amount)) = state.daily_entries.front() {
        if ts < horizon {
            state.daily_entries.pop_front();
            state.daily_total = state.daily_total.saturating_sub(amount);
        } else {
            break;
        }
    }
}

fn opportunity_token_iter(opportunity: &Opportunity) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::with_capacity(opportunity.route.len() * 2 + 1);
    tokens.push(opportunity.settlement_token.clone());
    for leg in &opportunity.route {
        tokens.push(leg.token_in.clone());
        tokens.push(leg.token_out.clone());
    }
    tokens.sort();
    tokens.dedup();
    tokens
}

pub(crate) fn risk_hash(
    opportunity: &Opportunity,
    simulation: &SimulationResult,
    min_surplus: Amount,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(opportunity.id.as_bytes());
    hasher.update(opportunity.expected_output_amount.to_le_bytes());
    hasher.update(simulation.expected_net_profit.to_le_bytes());
    hasher.update(simulation.gas_cost_wei.to_le_bytes());
    hasher.update(simulation.l1_data_fee.to_le_bytes());
    hasher.update(min_surplus.to_le_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use types::decision::{Opportunity, OpportunityKind, RouteLeg, SimulationResult};
    use types::{Exchange, Protocol};

    struct FakeClock {
        now: Arc<AtomicU64>,
    }

    impl Clock for FakeClock {
        fn now_unix_secs(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }
    }

    fn opp(id: &str, notional: Amount, expected_output: Amount, block: u64) -> Opportunity {
        Opportunity {
            id: id.to_string(),
            kind: OpportunityKind::TwoLegLoop,
            block_context: BlockContext::Block {
                number: block,
                hash: None,
            },
            settlement_token: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            input_amount: notional,
            expected_output_amount: expected_output,
            expected_net_profit: expected_output.saturating_sub(notional),
            route: vec![
                RouteLeg {
                    venue: Some(Exchange::BaseSwap),
                    protocol: Some(Protocol::UniswapV2),
                    target: "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
                    pool_address: Some("0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string()),
                    token_in: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                    token_out: "0xcccccccccccccccccccccccccccccccccccccccc".to_string(),
                    amount_in: Some(notional),
                    min_amount_out: Some(1),
                },
                RouteLeg {
                    venue: Some(Exchange::BaseSwap),
                    protocol: Some(Protocol::UniswapV2),
                    target: "0xdddddddddddddddddddddddddddddddddddddddd".to_string(),
                    pool_address: Some("0xffffffffffffffffffffffffffffffffffffffff".to_string()),
                    token_in: "0xcccccccccccccccccccccccccccccccccccccccc".to_string(),
                    token_out: "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
                    amount_in: None,
                    min_amount_out: Some(expected_output),
                },
            ],
            trigger_tx_hash: None,
        }
    }

    fn sim(net_profit: Amount, accepted: bool) -> SimulationResult {
        SimulationResult {
            opportunity_id: "irrelevant".to_string(),
            accepted,
            expected_gross_profit: net_profit + 100,
            expected_net_profit: net_profit,
            gas_estimate: 180_000,
            gas_cost_wei: 50,
            l1_data_fee: 50,
            notes: Vec::new(),
        }
    }

    fn baseline_policy() -> RiskPolicy {
        RiskPolicy {
            min_net_profit_wei: 10,
            min_profit_bps: 0,
            max_trade_notional_wei: Some(1_000_000),
            max_per_block_notional_wei: None,
            max_daily_notional_wei: Some(10_000_000),
            max_slippage_bps: 10_000,
            token_allowlist: HashSet::new(),
            token_denylist: HashSet::new(),
            max_consecutive_reverts: 3,
            kill_switch_path: None,
        }
    }

    fn engine_with_clock(policy: RiskPolicy, now: u64) -> (RiskEngine, Arc<AtomicU64>) {
        let clock = Arc::new(AtomicU64::new(now));
        let fake = FakeClock { now: clock.clone() };
        (RiskEngine::with_clock(policy, Box::new(fake)), clock)
    }

    #[test]
    fn accepts_opportunity_meeting_all_gates() {
        let (engine, _) = engine_with_clock(baseline_policy(), 1_700_000_000);
        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(50, true));
        assert!(decision.accepted, "expected accept, got: {decision:?}");
    }

    #[test]
    fn rejects_when_simulation_already_rejected() {
        let (engine, _) = engine_with_clock(baseline_policy(), 1_700_000_000);
        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(50, false));
        assert!(!decision.accepted);
        assert!(decision
            .reason
            .as_deref()
            .unwrap()
            .contains("simulation rejected"));
    }

    #[test]
    fn rejects_below_absolute_profit_floor() {
        let (engine, _) = engine_with_clock(baseline_policy(), 1_700_000_000);
        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(5, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("minimum"));
    }

    #[test]
    fn rejects_below_relative_profit_floor() {
        let mut policy = baseline_policy();
        policy.min_profit_bps = 100; // 1%
        let (engine, _) = engine_with_clock(policy, 1_700_000_000);
        let decision = engine.assess(&opp("o1", 10_000, 10_050, 1), &sim(50, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("bps of notional"));
    }

    #[test]
    fn rejects_above_per_trade_notional_cap() {
        let (engine, _) = engine_with_clock(baseline_policy(), 1_700_000_000);
        let decision = engine.assess(&opp("o1", 5_000_000, 5_000_500, 1), &sim(500, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("per-trade"));
    }

    #[test]
    fn rejects_above_per_block_notional_cap() {
        let mut policy = baseline_policy();
        policy.max_per_block_notional_wei = Some(150);
        let (engine, _) = engine_with_clock(policy, 1_700_000_000);

        let first = engine.assess(&opp("o1", 100, 200, 1), &sim(50, true));
        assert!(first.accepted);

        let second = engine.assess(&opp("o2", 100, 200, 1), &sim(50, true));
        assert!(!second.accepted);
        assert!(second.reason.as_deref().unwrap().contains("per-block"));

        let next_block = engine.assess(&opp("o3", 100, 200, 2), &sim(50, true));
        assert!(next_block.accepted, "next block should reset the per-block budget");
    }

    #[test]
    fn rolls_off_daily_notional_outside_window() {
        let mut policy = baseline_policy();
        policy.max_daily_notional_wei = Some(150);
        let (engine, clock) = engine_with_clock(policy, 1_700_000_000);

        assert!(engine
            .assess(&opp("o1", 100, 200, 1), &sim(50, true))
            .accepted);
        assert!(!engine
            .assess(&opp("o2", 100, 200, 2), &sim(50, true))
            .accepted);

        clock.store(1_700_000_000 + SECONDS_PER_DAY + 1, Ordering::SeqCst);
        assert!(engine
            .assess(&opp("o3", 100, 200, 3), &sim(50, true))
            .accepted);
    }

    #[test]
    fn denylist_short_circuits_assessment() {
        let mut policy = baseline_policy();
        policy
            .token_denylist
            .insert("0xcccccccccccccccccccccccccccccccccccccccc".to_string());
        let (engine, _) = engine_with_clock(policy, 1_700_000_000);
        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(50, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("denylist"));
    }

    #[test]
    fn allowlist_blocks_unknown_tokens() {
        let mut policy = baseline_policy();
        policy
            .token_allowlist
            .insert("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
        let (engine, _) = engine_with_clock(policy, 1_700_000_000);
        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(50, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("allowlist"));
    }

    #[test]
    fn circuit_breaker_trips_after_consecutive_reverts() {
        let mut policy = baseline_policy();
        policy.max_consecutive_reverts = 2;
        let (engine, _) = engine_with_clock(policy, 1_700_000_000);

        engine.record_outcome(false);
        engine.record_outcome(false);

        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(50, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("circuit breaker"));

        engine.record_outcome(true);
        let decision2 = engine.assess(&opp("o2", 100, 200, 1), &sim(50, true));
        assert!(decision2.accepted, "successful outcome should reset breaker");
    }

    #[test]
    fn manual_kill_switch_blocks_all_assessments() {
        let (engine, _) = engine_with_clock(baseline_policy(), 1_700_000_000);
        engine.trigger_kill_switch();
        let decision = engine.assess(&opp("o1", 100, 200, 1), &sim(50, true));
        assert!(!decision.accepted);
        assert!(decision.reason.as_deref().unwrap().contains("kill switch"));
    }
}

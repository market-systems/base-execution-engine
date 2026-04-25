#![forbid(unsafe_code)]

//! Core decision logic.
//!
//! The currently supported production slice is:
//!
//! - detect two-leg V2 loops from a `PoolBook`
//! - simulate them with an explicit cost model
//! - apply a deterministic risk gate via [`risk::RiskEngine`]
//! - build an `ExecutionRequest` for downstream execution

pub mod risk;

pub use risk::{RiskEngine, RiskPolicy};

use config::DecisionConfig;
use markets::{PoolBook, V2PoolState};
use types::decision::{Opportunity, OpportunityKind, RiskDecision, RouteLeg, SimulationResult};
use types::execution::{ExecutionRequest, ExecutionStep};
use types::{Address, Amount, BlockContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwoLegLoopInput {
    pub block_context: BlockContext,
    pub settlement_token: Address,
    pub amount_in: Amount,
    pub first_pool: V2PoolState,
    pub second_pool: V2PoolState,
    pub trigger_tx_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimulationConfig {
    pub gas_estimate: u64,
    pub gas_cost_wei: Amount,
    pub l1_data_fee_wei: Amount,
}

impl SimulationConfig {
    pub fn from_config(config: &DecisionConfig) -> anyhow::Result<Self> {
        Ok(Self {
            gas_estimate: config.simulation_gas_estimate,
            gas_cost_wei: config
                .simulation_gas_cost_wei
                .parse::<Amount>()
                .map_err(|_| anyhow::anyhow!("failed to parse DECISION_SIMULATION_GAS_COST_WEI"))?,
            l1_data_fee_wei: config
                .simulation_l1_data_fee_wei
                .parse::<Amount>()
                .map_err(|_| {
                    anyhow::anyhow!("failed to parse DECISION_SIMULATION_L1_DATA_FEE_WEI")
                })?,
        })
    }
}

pub fn detect_two_leg_loop(input: TwoLegLoopInput) -> Option<Opportunity> {
    if input.amount_in == 0 {
        tracing::debug!("skipping two-leg loop check with zero amount");
        return None;
    }

    let first_quote = input
        .first_pool
        .quote_exact_in(&input.settlement_token, input.amount_in)
        .ok()?;
    let second_quote = input
        .second_pool
        .quote_exact_in(&first_quote.token_out, first_quote.amount_out)
        .ok()?;

    if second_quote.token_out != input.settlement_token {
        tracing::debug!("two-leg loop does not settle back into the settlement asset");
        return None;
    }

    let expected_net_profit = second_quote.amount_out.checked_sub(input.amount_in)?;
    if expected_net_profit == 0 {
        return None;
    }

    let opportunity_id = format!(
        "loop:{}:{}:{}:{}",
        input.settlement_token,
        input.first_pool.address,
        input.second_pool.address,
        input.amount_in
    );

    Some(Opportunity {
        id: opportunity_id,
        kind: OpportunityKind::TwoLegLoop,
        block_context: input.block_context,
        settlement_token: input.settlement_token,
        input_amount: input.amount_in,
        expected_output_amount: second_quote.amount_out,
        expected_net_profit,
        route: vec![
            RouteLeg {
                venue: input.first_pool.exchange,
                protocol: Some(input.first_pool.protocol),
                target: input.first_pool.address.clone(),
                pool_address: Some(input.first_pool.address),
                token_in: first_quote.token_in,
                token_out: first_quote.token_out.clone(),
                amount_in: Some(first_quote.amount_in),
                min_amount_out: Some(first_quote.amount_out),
            },
            RouteLeg {
                venue: input.second_pool.exchange,
                protocol: Some(input.second_pool.protocol),
                target: input.second_pool.address.clone(),
                pool_address: Some(input.second_pool.address),
                token_in: first_quote.token_out,
                token_out: second_quote.token_out,
                amount_in: Some(second_quote.amount_in),
                min_amount_out: Some(second_quote.amount_out),
            },
        ],
        trigger_tx_hash: input.trigger_tx_hash,
    })
}

pub fn detect_two_leg_loops_from_book(
    book: &PoolBook,
    block_context: BlockContext,
    settlement_token: &str,
    amount_in: Amount,
    trigger_tx_hash: Option<String>,
) -> Vec<Opportunity> {
    let settlement_token = settlement_token.to_string();
    let mut opportunities = Vec::new();

    for first_pool in book.v2_pools() {
        if !first_pool.contains_token(&settlement_token) {
            continue;
        }

        let Some(intermediate_token) = first_pool.other_token(&settlement_token) else {
            continue;
        };

        for second_pool in book.v2_pools() {
            if first_pool
                .address
                .eq_ignore_ascii_case(&second_pool.address)
            {
                continue;
            }

            if !second_pool.contains_token(intermediate_token)
                || !second_pool.contains_token(&settlement_token)
            {
                continue;
            }

            if let Some(opportunity) = detect_two_leg_loop(TwoLegLoopInput {
                block_context: block_context.clone(),
                settlement_token: settlement_token.clone(),
                amount_in,
                first_pool: first_pool.clone(),
                second_pool: second_pool.clone(),
                trigger_tx_hash: trigger_tx_hash.clone(),
            }) {
                opportunities.push(opportunity);
            }
        }
    }

    opportunities.sort_by(|left, right| {
        right
            .expected_net_profit
            .cmp(&left.expected_net_profit)
            .then_with(|| left.id.cmp(&right.id))
    });
    opportunities.dedup_by(|left, right| left.id == right.id);
    opportunities
}

pub fn simulate_two_leg_opportunity(
    opportunity: &Opportunity,
    simulation: SimulationConfig,
) -> Option<SimulationResult> {
    if opportunity.kind != OpportunityKind::TwoLegLoop || opportunity.route.len() != 2 {
        return None;
    }

    let expected_gross_profit = opportunity
        .expected_output_amount
        .checked_sub(opportunity.input_amount)?;
    let expected_net_profit = expected_gross_profit
        .checked_sub(simulation.gas_cost_wei)?
        .checked_sub(simulation.l1_data_fee_wei)?;

    Some(SimulationResult {
        opportunity_id: opportunity.id.clone(),
        accepted: expected_net_profit > 0,
        expected_gross_profit,
        expected_net_profit,
        gas_estimate: simulation.gas_estimate,
        gas_cost_wei: simulation.gas_cost_wei,
        l1_data_fee: simulation.l1_data_fee_wei,
        notes: vec![
            "simulation path uses deterministic local V2 quotes".to_string(),
            "gas and L1 data fee are config-driven cost baselines".to_string(),
        ],
    })
}

/// Stateless legacy entry point retained for backwards compatibility with
/// scripts and tests that have not yet been migrated to [`RiskEngine`]. It
/// applies only the absolute profit floor pulled from
/// `DecisionConfig::min_net_profit_wei`. Production callers MUST use
/// [`RiskEngine`] for full coverage of notional caps, allowlists, slippage
/// ceilings, and the kill switch.
pub fn assess_risk(
    opportunity: &Opportunity,
    simulation: &SimulationResult,
    config: &DecisionConfig,
) -> Option<RiskDecision> {
    let min_surplus = config.min_net_profit_wei.parse::<Amount>().ok()?;
    let accepted = simulation.accepted && simulation.expected_net_profit >= min_surplus;
    let reason = if accepted {
        None
    } else {
        Some("expected net profit is below the configured minimum surplus".to_string())
    };

    Some(RiskDecision {
        opportunity_id: opportunity.id.clone(),
        accepted,
        reason,
        risk_hash: risk::risk_hash(opportunity, simulation, min_surplus),
        min_surplus,
    })
}

pub fn build_execution_request(
    opportunity: &Opportunity,
    risk: &RiskDecision,
) -> Option<ExecutionRequest> {
    if !risk.accepted {
        return None;
    }

    let mut steps = Vec::with_capacity(opportunity.route.len());
    for leg in &opportunity.route {
        steps.push(ExecutionStep {
            venue: leg.venue,
            protocol: leg.protocol,
            target: leg.target.clone(),
            token_in: leg.token_in.clone(),
            token_out: leg.token_out.clone(),
            pool_fee: None,
            amount_in: leg.amount_in,
            min_amount_out: leg.min_amount_out?,
            calldata_hint: Some("v2_exact_input".to_string()),
            aux_address: None,
        });
    }

    Some(ExecutionRequest {
        opportunity_id: opportunity.id.clone(),
        steps,
        min_repay: opportunity.input_amount,
        min_surplus: risk.min_surplus,
        flashloan_asset: Some(opportunity.settlement_token.clone()),
        flashloan_amount: Some(opportunity.input_amount),
        risk_hash: risk.risk_hash.clone(),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use types::{Exchange, Protocol};

    fn profitable_loop() -> TwoLegLoopInput {
        TwoLegLoopInput {
            block_context: BlockContext::Pending,
            settlement_token: "0xweth".to_string(),
            amount_in: 1_000_000_u128,
            first_pool: V2PoolState::new(
                "0xpool1",
                Protocol::UniswapV2,
                Some(Exchange::Aerodrome),
                "0xweth",
                "0xasset",
                1_000_000_000_u128,
                2_500_000_000_u128,
                30,
            )
            .unwrap(),
            second_pool: V2PoolState::new(
                "0xpool2",
                Protocol::UniswapV2,
                Some(Exchange::AlienBase),
                "0xasset",
                "0xweth",
                2_000_000_000_u128,
                1_200_000_000_u128,
                30,
            )
            .unwrap(),
            trigger_tx_hash: Some("0xtrigger".to_string()),
        }
    }

    fn decision_config() -> DecisionConfig {
        DecisionConfig {
            max_route_hops: 3,
            max_candidate_routes: 16,
            min_net_profit_wei: "10".to_string(),
            kill_switch_path: None,
            settlement_token: Some("0xweth".to_string()),
            scan_trade_amount_wei: Some("1000000".to_string()),
            v2_bootstrap_path: None,
            simulation_gas_estimate: 180_000,
            simulation_gas_cost_wei: "5".to_string(),
            simulation_l1_data_fee_wei: "2".to_string(),
        }
    }

    #[test]
    fn detects_profitable_two_leg_loop() {
        let opportunity = detect_two_leg_loop(profitable_loop()).unwrap();

        assert_eq!(opportunity.kind, OpportunityKind::TwoLegLoop);
        assert!(opportunity.expected_net_profit > 0);
        assert_eq!(opportunity.route.len(), 2);
    }

    #[test]
    fn rejects_non_profitable_loop() {
        let mut input = profitable_loop();
        input.second_pool.reserve1 = 700_000_000_u128;

        let opportunity = detect_two_leg_loop(input);

        assert!(opportunity.is_none());
    }

    #[test]
    fn rejects_zero_amount() {
        let mut input = profitable_loop();
        input.amount_in = 0;

        assert!(detect_two_leg_loop(input).is_none());
    }

    #[test]
    fn scans_pool_book_for_profitable_loops() {
        let mut book = PoolBook::new();
        book.insert_v2_pool(profitable_loop().first_pool);
        book.insert_v2_pool(profitable_loop().second_pool);

        let opportunities =
            detect_two_leg_loops_from_book(&book, BlockContext::Pending, "0xweth", 1_000_000, None);

        assert_eq!(opportunities.len(), 1);
        assert_eq!(opportunities[0].kind, OpportunityKind::TwoLegLoop);
        assert!(opportunities[0].expected_net_profit > 0);
    }

    #[test]
    fn sorts_opportunities_by_profit_descending() {
        let mut book = PoolBook::new();
        let base = profitable_loop();

        book.insert_v2_pool(base.first_pool.clone());
        book.insert_v2_pool(base.second_pool.clone());
        book.insert_v2_pool(
            V2PoolState::new(
                "0xpool3",
                Protocol::UniswapV2,
                Some(Exchange::BaseSwap),
                "0xasset",
                "0xweth",
                2_000_000_000_u128,
                1_400_000_000_u128,
                30,
            )
            .unwrap(),
        );

        let opportunities =
            detect_two_leg_loops_from_book(&book, BlockContext::Pending, "0xweth", 1_000_000, None);

        assert!(opportunities.len() >= 2);
        assert!(opportunities[0].expected_net_profit >= opportunities[1].expected_net_profit);
    }

    #[test]
    fn simulates_risks_and_builds_execution_request() {
        let opportunity = detect_two_leg_loop(profitable_loop()).unwrap();
        let simulation = simulate_two_leg_opportunity(
            &opportunity,
            SimulationConfig {
                gas_estimate: 180_000,
                gas_cost_wei: 5,
                l1_data_fee_wei: 2,
            },
        )
        .unwrap();
        let risk = assess_risk(&opportunity, &simulation, &decision_config()).unwrap();
        let request = build_execution_request(&opportunity, &risk).unwrap();

        assert!(simulation.expected_net_profit > 0);
        assert!(risk.accepted);
        assert_eq!(request.steps.len(), 2);
        assert_eq!(request.flashloan_amount, Some(opportunity.input_amount));
    }
}

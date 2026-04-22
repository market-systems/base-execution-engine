use crate::{Address, Amount, BlockContext, Exchange, Protocol};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityKind {
    TwoLegLoop,
    Triangle,
    Backrun,
    JitLiquidity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteLeg {
    pub venue: Option<Exchange>,
    pub protocol: Option<Protocol>,
    pub target: Address,
    pub pool_address: Option<Address>,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: Option<Amount>,
    pub min_amount_out: Option<Amount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Opportunity {
    pub id: String,
    pub kind: OpportunityKind,
    pub block_context: BlockContext,
    pub settlement_token: Address,
    pub input_amount: Amount,
    pub expected_output_amount: Amount,
    pub expected_net_profit: Amount,
    pub route: Vec<RouteLeg>,
    pub trigger_tx_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulationResult {
    pub opportunity_id: String,
    pub accepted: bool,
    pub expected_gross_profit: Amount,
    pub expected_net_profit: Amount,
    pub gas_estimate: u64,
    pub gas_cost_wei: Amount,
    pub l1_data_fee: Amount,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskDecision {
    pub opportunity_id: String,
    pub accepted: bool,
    pub reason: Option<String>,
    pub risk_hash: String,
    pub min_surplus: Amount,
}

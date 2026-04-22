use crate::{Address, Amount, BlockNumber, Exchange, Protocol};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Built,
    Submitted,
    Included,
    Reverted,
    Dropped,
    Replaced,
    ProfitRealized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStep {
    pub venue: Option<Exchange>,
    pub protocol: Option<Protocol>,
    pub target: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub pool_fee: Option<u32>,
    pub amount_in: Option<Amount>,
    pub min_amount_out: Amount,
    pub calldata_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRequest {
    pub opportunity_id: String,
    pub steps: Vec<ExecutionStep>,
    pub min_repay: Amount,
    pub min_surplus: Amount,
    pub flashloan_asset: Option<Address>,
    pub flashloan_amount: Option<Amount>,
    pub risk_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionAttempt {
    pub request_id: String,
    pub tx_hash: Option<String>,
    pub status: ExecutionStatus,
    pub execution_mode: String,
    pub submission_allowed: bool,
    pub blocked_reason: Option<String>,
    pub requested_notional: Amount,
    pub gas_limit: Option<u64>,
    pub max_fee_per_gas: Option<Amount>,
    pub max_priority_fee_per_gas: Option<Amount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionReceipt {
    pub request_id: String,
    pub tx_hash: String,
    pub block_number: Option<BlockNumber>,
    pub success: bool,
    pub gas_used: Option<u64>,
    pub effective_gas_price: Option<Amount>,
    pub l1_fee_paid: Option<Amount>,
    pub revert_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionOutcome {
    pub request_id: String,
    pub final_status: ExecutionStatus,
    pub tx_hash: String,
    pub block_number: Option<BlockNumber>,
    pub total_fee_paid: Option<Amount>,
    pub realized_surplus: Option<Amount>,
    pub reason: Option<String>,
}

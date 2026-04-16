#![allow(deprecated)]

use async_trait::async_trait;
use anyhow::{anyhow, Context, Result};
use common::{
    ContractCall, DispatchResult, DispatchStatus, ExecutionPlanStep, ExecutionRequest,
    ExecutionVenueKind, Mode, OpportunityCandidate, RoutePlan, RouterExecutionPlan,
    SimulationResult, VenueKind,
};
use ethers::{
    abi::{Abi, Function, Param, ParamType, StateMutability, Token as AbiToken},
    contract::BaseContract,
    types::{transaction::eip2718::TypedTransaction, Address, BlockNumber, TransactionRequest, U256},
    utils::keccak256,
};
use ingress::ChainClient;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
pub struct ExecutionBuilderConfig {
    pub execution_router: Address,
    pub min_output_bps: u16,
}

#[async_trait]
pub trait Broadcaster: Send + Sync {
    async fn dispatch(&self, request: &ExecutionRequest) -> Result<DispatchResult>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ShadowBroadcaster;

#[derive(Debug, Clone)]
pub struct LiveBroadcaster {
    chain: ChainClient,
}

#[derive(Debug, Clone)]
pub enum ExecutionBroadcaster {
    Shadow(ShadowBroadcaster),
    Live(LiveBroadcaster),
}

impl ExecutionBroadcaster {
    pub fn from_mode(
        mode: Mode,
        chain: ChainClient,
    ) -> Result<Self> {
        match mode {
            Mode::Live => Ok(Self::Live(LiveBroadcaster::new(chain))),
            Mode::Research | Mode::Shadow => Ok(Self::Shadow(ShadowBroadcaster)),
        }
    }
}

#[async_trait]
impl Broadcaster for ExecutionBroadcaster {
    async fn dispatch(&self, request: &ExecutionRequest) -> Result<DispatchResult> {
        match self {
            ExecutionBroadcaster::Shadow(broadcaster) => broadcaster.dispatch(request).await,
            ExecutionBroadcaster::Live(broadcaster) => broadcaster.dispatch(request).await,
        }
    }
}

#[async_trait]
impl Broadcaster for ShadowBroadcaster {
    async fn dispatch(&self, request: &ExecutionRequest) -> Result<DispatchResult> {
        Ok(DispatchResult {
            status: DispatchStatus::Suppressed,
            tx_hash: None,
            submitted_from: None,
            submitted_nonce: None,
            reason: format!(
                "broadcast suppressed for router execution request derived from observed tx {:?}",
                request.observed_tx_hash
            ),
        })
    }
}

impl LiveBroadcaster {
    pub fn new(chain: ChainClient) -> Self {
        Self { chain }
    }
}

#[async_trait]
impl Broadcaster for LiveBroadcaster {
    async fn dispatch(&self, request: &ExecutionRequest) -> Result<DispatchResult> {
        if request.calls.len() != 1 {
            return Err(anyhow!(
                "live broadcasting currently supports exactly one contract call, got {}",
                request.calls.len()
            ));
        }

        let call = &request.calls[0];
        let submitted_from = self
            .chain
            .signer_address()
            .ok_or_else(|| anyhow!("live chain client is not configured with a signer"))?;
        let submitted_nonce = self
            .chain
            .get_transaction_count(submitted_from, Some(BlockNumber::Pending.into()))
            .await
            .context("failed to resolve executor nonce for live submission")?;
        let tx: TypedTransaction = TransactionRequest::new()
            .to(call.target)
            .data(call.calldata.clone())
            .value(call.value)
            .from(submitted_from)
            .nonce(submitted_nonce)
            .into();

        let tx_hash = self
            .chain
            .send_transaction(tx)
            .await
            .context("failed to submit live transaction")?;

        Ok(DispatchResult {
            status: DispatchStatus::Submitted,
            tx_hash: Some(tx_hash),
            submitted_from: Some(submitted_from),
            submitted_nonce: Some(submitted_nonce.as_u64()),
            reason: format!(
                "submitted BaseExecutionRouter plan derived from observed tx {:?}",
                request.observed_tx_hash
            ),
        })
    }
}

pub fn build_execution_request(
    candidate: &OpportunityCandidate,
    plan: RoutePlan,
    simulation: SimulationResult,
    config: ExecutionBuilderConfig,
) -> Result<ExecutionRequest> {
    if plan.steps.is_empty() {
        return Err(anyhow!("execution route must contain at least one step"));
    }

    if !plan.is_executable() {
        return Err(anyhow!(
            "selected route contains at least one venue that is not executable in the current runtime"
        ));
    }

    if !plan.is_cycle() {
        return Err(anyhow!(
            "selected route does not settle back into the settlement token"
        ));
    }

    if config.min_output_bps == 0 || config.min_output_bps > 10_000 {
        return Err(anyhow!(
            "min output bps must be between 1 and 10000, got {}",
            config.min_output_bps
        ));
    }

    if simulation.gross_surplus <= ethers::types::I256::from(0) {
        return Err(anyhow!(
            "simulation gross surplus must be positive for settlement execution"
        ));
    }

    let min_surplus = simulation.gross_surplus.into_raw()
        .saturating_mul(U256::from(config.min_output_bps))
        / U256::from(10_000u64);
    let execution_steps = plan
        .steps
        .iter()
        .map(execution_plan_step)
        .collect::<Result<Vec<_>>>()?;
    let router_plan = RouterExecutionPlan {
        settlement_token: plan.source_token,
        funding_amount: plan.amount_in,
        min_repay_amount: plan.amount_in,
        min_surplus,
        steps: execution_steps,
        risk_hash: ethers::types::H256::zero(),
    };
    let router_plan = RouterExecutionPlan {
        risk_hash: build_risk_hash(candidate, &plan, &router_plan),
        ..router_plan
    };
    let router_call = ContractCall {
        target: config.execution_router,
        calldata: BaseContract::from(execution_router_abi())
            .encode("executePlan", (execution_plan_token(&router_plan),))
            .context("failed to encode BaseExecutionRouter.executePlan call")?
            .0
            .into(),
        value: U256::zero(),
    };

    Ok(ExecutionRequest {
        observed_tx_hash: candidate.intent.tx_hash,
        route_plan: plan,
        simulation,
        router: config.execution_router,
        plan: router_plan,
        calls: vec![router_call],
    })
}

pub fn build_provisional_execution_call(
    plan: &RoutePlan,
    execution_router: Address,
) -> Result<ContractCall> {
    if plan.steps.is_empty() {
        return Err(anyhow!("execution route must contain at least one step"));
    }

    if !plan.is_executable() || !plan.is_cycle() {
        return Err(anyhow!(
            "provisional execution call requires a live-executable cycle route"
        ));
    }

    let execution_steps = plan
        .steps
        .iter()
        .map(execution_plan_step)
        .collect::<Result<Vec<_>>>()?;
    let router_plan = RouterExecutionPlan {
        settlement_token: plan.source_token,
        funding_amount: plan.amount_in,
        min_repay_amount: U256::zero(),
        min_surplus: U256::zero(),
        steps: execution_steps,
        risk_hash: ethers::types::H256::zero(),
    };

    Ok(ContractCall {
        target: execution_router,
        calldata: BaseContract::from(execution_router_abi())
            .encode("executePlan", (execution_plan_token(&router_plan),))
            .context("failed to encode provisional BaseExecutionRouter.executePlan call")?
            .0
            .into(),
        value: U256::zero(),
    })
}

fn execution_plan_step(step: &common::RouteStep) -> Result<ExecutionPlanStep> {
    let venue_kind = match step.venue {
        VenueKind::UniswapV3 => ExecutionVenueKind::UniswapV3Single,
        other => {
            return Err(anyhow!(
                "BaseExecutionRouter does not support venue {:?} for step {}",
                other,
                step.name
            ))
        }
    };

    Ok(ExecutionPlanStep {
        venue_kind,
        target: step.router,
        token_in: step.token_in,
        token_out: step.token_out,
        fee_bps: step.fee_bps,
        extra_data: Default::default(),
    })
}

fn build_risk_hash(
    candidate: &OpportunityCandidate,
    plan: &RoutePlan,
    router_plan: &RouterExecutionPlan,
) -> ethers::types::H256 {
    let steps = router_plan
        .steps
        .iter()
        .map(|step| {
            AbiToken::Tuple(vec![
                AbiToken::Uint(U256::from(step.venue_kind as u8)),
                AbiToken::Address(step.target),
                AbiToken::Address(step.token_in),
                AbiToken::Address(step.token_out),
                AbiToken::Uint(U256::from(step.fee_bps)),
            ])
        })
        .collect::<Vec<_>>();

    ethers::types::H256::from(keccak256(ethers::abi::encode(&[
        AbiToken::FixedBytes(candidate.intent.tx_hash.as_bytes().to_vec()),
        AbiToken::Address(plan.source_token),
        AbiToken::Address(plan.target_token),
        AbiToken::Uint(plan.amount_in),
        AbiToken::Uint(router_plan.min_repay_amount),
        AbiToken::Uint(router_plan.min_surplus),
        AbiToken::Array(steps),
    ])))
}

fn execution_plan_token(plan: &RouterExecutionPlan) -> AbiToken {
    AbiToken::Tuple(vec![
        AbiToken::Address(plan.settlement_token),
        AbiToken::Uint(plan.funding_amount),
        AbiToken::Uint(plan.min_repay_amount),
        AbiToken::Uint(plan.min_surplus),
        AbiToken::Array(
            plan.steps
                .iter()
                .map(|step| {
                    AbiToken::Tuple(vec![
                        AbiToken::Uint(U256::from(step.venue_kind as u8)),
                        AbiToken::Address(step.target),
                        AbiToken::Address(step.token_in),
                        AbiToken::Address(step.token_out),
                        AbiToken::Uint(U256::from(step.fee_bps)),
                        AbiToken::Bytes(step.extra_data.to_vec()),
                    ])
                })
                .collect(),
        ),
        AbiToken::FixedBytes(plan.risk_hash.as_bytes().to_vec()),
    ])
}

fn execution_router_abi() -> Abi {
    Abi {
        constructor: None,
        functions: BTreeMap::from([(
            "executePlan".to_string(),
            vec![Function {
                name: "executePlan".to_string(),
                inputs: vec![Param {
                    name: "plan".to_string(),
                    kind: ParamType::Tuple(vec![
                        ParamType::Address,
                        ParamType::Uint(256),
                        ParamType::Uint(256),
                        ParamType::Uint(256),
                        ParamType::Array(Box::new(ParamType::Tuple(vec![
                            ParamType::Uint(8),
                            ParamType::Address,
                            ParamType::Address,
                            ParamType::Address,
                            ParamType::Uint(24),
                            ParamType::Bytes,
                        ]))),
                        ParamType::FixedBytes(32),
                    ]),
                    internal_type: None,
                }],
                outputs: vec![Param {
                    name: "amountOut".to_string(),
                    kind: ParamType::Uint(256),
                    internal_type: None,
                }],
                constant: None,
                state_mutability: StateMutability::Payable,
            }],
        )]),
        events: Default::default(),
        errors: Default::default(),
        receive: false,
        fallback: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        default_capabilities_for_venue, ActionKind, EventSource, ExecutionVenueKind,
        ObservedIntent, RouteStep, SimulationStatus, VenueKind,
    };
    use ethers::types::{Address, H256, U256};
    use serde_json::json;

    fn address(value: u64) -> Address {
        Address::from_low_u64_be(value)
    }

    #[test]
    fn builds_uniswap_v3_router_execution_request() {
        let candidate = OpportunityCandidate {
            intent: ObservedIntent {
                source: EventSource::Flashblocks,
                tx_hash: H256::from_low_u64_be(1),
                actor: address(99),
                router: address(10),
                venue: VenueKind::UniswapV3,
                action: ActionKind::Swap,
                token_in: Some(address(1)),
                token_out: Some(address(2)),
                amount_in: Some(U256::from(1_000u64)),
                pool_key: None,
                input: Default::default(),
                value: U256::zero(),
                raw_selector: None,
                metadata: json!({}),
            },
            settlement_token: address(1),
            target_token: address(2),
            amount_in: U256::from(1_000u64),
        };
        let plan = RoutePlan {
            source_token: address(1),
            target_token: address(1),
            amount_in: U256::from(1_000u64),
            steps: vec![
                RouteStep {
                    venue: VenueKind::UniswapV3,
                    name: "forward".to_string(),
                    capabilities: default_capabilities_for_venue(VenueKind::UniswapV3),
                    router: address(10),
                    quoter: Some(address(11)),
                    token_in: address(1),
                    token_out: address(2),
                    fee_bps: 500,
                    path: vec![address(1), address(2)],
                    stable: false,
                    pool_key: None,
                    estimated_gas: 100,
                },
                RouteStep {
                    venue: VenueKind::UniswapV3,
                    name: "return".to_string(),
                    capabilities: default_capabilities_for_venue(VenueKind::UniswapV3),
                    router: address(12),
                    quoter: Some(address(13)),
                    token_in: address(2),
                    token_out: address(1),
                    fee_bps: 500,
                    path: vec![address(2), address(1)],
                    stable: false,
                    pool_key: None,
                    estimated_gas: 100,
                },
            ],
            expected_amount_out: U256::from(2_000u64),
            estimated_gas: 200,
        };
        let simulation = SimulationResult {
            status: SimulationStatus::Success,
            expected_amount_out: U256::from(2_000u64),
            gas_used: 90,
            estimated_gas_cost: U256::from(10u64),
            gross_surplus: ethers::types::I256::from(1_000),
            net_surplus: ethers::types::I256::from(990),
            reason: "ok".to_string(),
            confidence_bps: 9_000,
        };

        let request = build_execution_request(
            &candidate,
            plan,
            simulation,
            ExecutionBuilderConfig {
                execution_router: address(77),
                min_output_bps: 9_500,
            },
        )
        .unwrap();

        assert_eq!(request.calls.len(), 1);
        assert_eq!(request.router, address(77));
        assert_eq!(request.plan.settlement_token, address(1));
        assert_eq!(request.plan.min_repay_amount, U256::from(1_000u64));
        assert_eq!(request.plan.min_surplus, U256::from(950u64));
        assert_eq!(request.plan.steps.len(), 2);
        assert_eq!(
            request.plan.steps[0].venue_kind,
            ExecutionVenueKind::UniswapV3Single
        );
        assert_eq!(request.plan.steps[0].target, address(10));
    }

    #[test]
    fn rejects_unsupported_router_venue() {
        let candidate = OpportunityCandidate {
            intent: ObservedIntent {
                source: EventSource::Flashblocks,
                tx_hash: H256::from_low_u64_be(1),
                actor: address(99),
                router: address(10),
                venue: VenueKind::UniswapV2,
                action: ActionKind::Swap,
                token_in: Some(address(1)),
                token_out: Some(address(2)),
                amount_in: Some(U256::from(1_000u64)),
                pool_key: None,
                input: Default::default(),
                value: U256::zero(),
                raw_selector: None,
                metadata: json!({}),
            },
            settlement_token: address(1),
            target_token: address(2),
            amount_in: U256::from(1_000u64),
        };
        let plan = RoutePlan {
            source_token: address(1),
            target_token: address(1),
            amount_in: U256::from(1_000u64),
            steps: vec![
                RouteStep {
                    venue: VenueKind::UniswapV2,
                    name: "unsupported".to_string(),
                    capabilities: default_capabilities_for_venue(VenueKind::UniswapV2),
                    router: address(10),
                    quoter: Some(address(11)),
                    token_in: address(1),
                    token_out: address(2),
                    fee_bps: 30,
                    path: vec![address(1), address(2)],
                    stable: false,
                    pool_key: None,
                    estimated_gas: 100,
                },
                RouteStep {
                    venue: VenueKind::UniswapV2,
                    name: "unsupported-return".to_string(),
                    capabilities: default_capabilities_for_venue(VenueKind::UniswapV2),
                    router: address(12),
                    quoter: Some(address(13)),
                    token_in: address(2),
                    token_out: address(1),
                    fee_bps: 30,
                    path: vec![address(2), address(1)],
                    stable: false,
                    pool_key: None,
                    estimated_gas: 100,
                },
            ],
            expected_amount_out: U256::from(2_000u64),
            estimated_gas: 200,
        };
        let simulation = SimulationResult {
            status: common::SimulationStatus::Success,
            expected_amount_out: U256::from(2_000u64),
            gas_used: 90,
            estimated_gas_cost: U256::from(10u64),
            gross_surplus: ethers::types::I256::from(1_000),
            net_surplus: ethers::types::I256::from(990),
            reason: "ok".to_string(),
            confidence_bps: 9_000,
        };

        let error = build_execution_request(
            &candidate,
            plan,
            simulation,
            ExecutionBuilderConfig {
                execution_router: address(77),
                min_output_bps: 9_500,
            },
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("not executable in the current runtime"));
    }
}

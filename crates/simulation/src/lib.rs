use anyhow::{anyhow, Context, Result};
use common::{ContractCall, OpportunityCandidate, RoutePlan, SimulationResult, SimulationStatus, WETH_BASE};
use ethers::abi::{decode, ParamType};
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::{I256, TransactionRequest, U256};
use ingress::ChainClient;
use strategy::{adapter_for_step, default_quote_steps};

#[derive(Clone)]
pub struct QuoteSimulator {
    chain: ChainClient,
}

impl QuoteSimulator {
    pub fn new(chain: ChainClient) -> Self {
        Self { chain }
    }

    pub async fn simulate_plan(&self, plan: &RoutePlan) -> Result<SimulationResult> {
        if plan.steps.is_empty() {
            return Ok(SimulationResult {
                status: SimulationStatus::Skipped,
                expected_amount_out: U256::zero(),
                gas_used: 0,
                estimated_gas_cost: U256::zero(),
                gross_surplus: I256::from(0),
                net_surplus: I256::from(0),
                post_trigger_checked: false,
                reason: "route plan has no steps".to_string(),
                confidence_bps: 0,
            });
        }

        if !plan.is_simulatable() {
            return Ok(SimulationResult {
                status: SimulationStatus::Skipped,
                expected_amount_out: U256::zero(),
                gas_used: 0,
                estimated_gas_cost: U256::zero(),
                gross_surplus: I256::from(0),
                net_surplus: I256::from(0),
                post_trigger_checked: false,
                reason: "route plan contains at least one non-simulatable venue".to_string(),
                confidence_bps: 0,
            });
        }

        let mut current_amount = plan.amount_in;
        let mut current_token = plan.source_token;
        let mut total_gas = 0u64;

        for step in &plan.steps {
            let adapter = adapter_for_step(step)?;
            let call = adapter
                .encode_quote(current_amount, current_token, step.token_out)
                .with_context(|| format!("failed to encode quote for {}", step.name))?;
            let tx: TypedTransaction = TransactionRequest::new()
                .to(call.target)
                .data(call.calldata)
                .value(call.value)
                .into();

            let output = self
                .chain
                .call(&tx)
                .await
                .with_context(|| format!("quote call failed for {}", step.name))?;
            current_amount = adapter
                .decode_quote(output)
                .with_context(|| format!("failed to decode quote for {}", step.name))?;
            current_token = step.token_out;
            total_gas = total_gas.saturating_add(step.estimated_gas);
        }

        if current_amount.is_zero() {
            return Err(anyhow!("simulation returned zero output"));
        }

        let estimated_gas_cost = self.estimate_gas_cost(plan.source_token, total_gas).await?;
        let gross_surplus = i256_from_diff(current_amount, plan.amount_in);
        let net_surplus = gross_surplus - I256::from_raw(estimated_gas_cost);

        Ok(SimulationResult {
            status: SimulationStatus::Success,
            expected_amount_out: current_amount,
            gas_used: total_gas,
            estimated_gas_cost,
            gross_surplus,
            net_surplus,
            post_trigger_checked: false,
            reason: "quote simulation completed".to_string(),
            confidence_bps: 8_500,
        })
    }

    pub async fn simulate_candidate_plan(
        &self,
        candidate: &OpportunityCandidate,
        plan: &RoutePlan,
        provisional_call: Option<&ContractCall>,
    ) -> Result<SimulationResult> {
        let trigger_viable = self.preflight_trigger(candidate).await?;
        if !trigger_viable {
            return Ok(SimulationResult {
                status: SimulationStatus::Skipped,
                expected_amount_out: U256::zero(),
                gas_used: 0,
                estimated_gas_cost: U256::zero(),
                gross_surplus: I256::from(0),
                net_surplus: I256::from(0),
                post_trigger_checked: false,
                reason:
                    "trigger preflight did not succeed on current state; route simulation skipped"
                        .to_string(),
                confidence_bps: 0,
            });
        }

        if let Some(call) = provisional_call {
            if let Ok(Some(post_trigger_result)) =
                self.simulate_post_trigger_plan(candidate, plan, call).await
            {
                return Ok(post_trigger_result);
            }
        }

        let mut result = self.simulate_plan(plan).await?;
        result.reason = format!(
            "trigger preflight passed, but post-trigger state replay was unavailable; {}",
            result.reason
        );
        result.confidence_bps = result.confidence_bps.saturating_sub(1_000);
        Ok(result)
    }

    async fn estimate_gas_cost(&self, settlement_token: ethers::types::Address, gas_used: u64) -> Result<U256> {
        if gas_used == 0 {
            return Ok(U256::zero());
        }

        let gas_price = self.chain.get_gas_price().await?;
        let gas_cost_in_weth = gas_price.saturating_mul(U256::from(gas_used));
        if settlement_token == *WETH_BASE {
            return Ok(gas_cost_in_weth);
        }

        let mut best: Option<U256> = None;
        for step in default_quote_steps(settlement_token, None) {
            let adapter = adapter_for_step(&step)?;
            let call = adapter
                .encode_quote(gas_cost_in_weth, *WETH_BASE, settlement_token)
                .with_context(|| format!("failed to encode gas conversion quote for {}", step.name))?;
            let tx: TypedTransaction = TransactionRequest::new()
                .to(call.target)
                .data(call.calldata)
                .value(call.value)
                .into();
            let output = self
                .chain
                .call(&tx)
                .await
                .with_context(|| format!("gas conversion quote failed for {}", step.name))?;
            let quoted = adapter
                .decode_quote(output)
                .with_context(|| format!("failed to decode gas conversion quote for {}", step.name))?;
            best = Some(best.map(|current| current.max(quoted)).unwrap_or(quoted));
        }

        best.ok_or_else(|| anyhow!("no conversion route available for settlement token gas estimate"))
    }

    async fn preflight_trigger(&self, candidate: &OpportunityCandidate) -> Result<bool> {
        let tx: TypedTransaction = TransactionRequest::new()
            .to(candidate.intent.router)
            .from(candidate.intent.actor)
            .data(candidate.intent.input.clone())
            .value(candidate.intent.value)
            .into();

        match self.chain.call(&tx).await {
            Ok(_) => Ok(true),
            Err(error) => {
                if candidate.intent.input.is_empty() {
                    Ok(false)
                } else {
                    Err(error).context("trigger preflight eth_call failed")
                }
            }
        }
    }

    async fn simulate_post_trigger_plan(
        &self,
        candidate: &OpportunityCandidate,
        plan: &RoutePlan,
        provisional_call: &ContractCall,
    ) -> Result<Option<SimulationResult>> {
        let trigger_tx: TypedTransaction = TransactionRequest::new()
            .to(candidate.intent.router)
            .from(candidate.intent.actor)
            .data(candidate.intent.input.clone())
            .value(candidate.intent.value)
            .into();
        let route_tx: TypedTransaction = TransactionRequest::new()
            .to(provisional_call.target)
            .from(candidate.intent.actor)
            .data(provisional_call.calldata.clone())
            .value(provisional_call.value)
            .into();
        let outputs = match self.chain.call_many(&[trigger_tx, route_tx]).await {
            Ok(outputs) => outputs,
            Err(_) => return Ok(None),
        };

        let Some(route_output) = outputs.last() else {
            return Ok(None);
        };
        let expected_amount_out = decode_amount_out(route_output)?;
        let estimated_gas_cost = self.estimate_gas_cost(plan.source_token, plan.estimated_gas).await?;
        let gross_surplus = i256_from_diff(expected_amount_out, plan.amount_in);
        let net_surplus = gross_surplus - I256::from_raw(estimated_gas_cost);

        Ok(Some(SimulationResult {
            status: SimulationStatus::Success,
            expected_amount_out,
            gas_used: plan.estimated_gas,
            estimated_gas_cost,
            gross_surplus,
            net_surplus,
            post_trigger_checked: true,
            reason: "post-trigger execution replay completed".to_string(),
            confidence_bps: 9_000,
        }))
    }
}

fn i256_from_diff(left: U256, right: U256) -> I256 {
    if left >= right {
        I256::from_raw(left - right)
    } else {
        -I256::from_raw(right - left)
    }
}

fn decode_amount_out(output: &ethers::types::Bytes) -> Result<U256> {
    let decoded = decode(&[ParamType::Uint(256)], output)?;
    decoded
        .first()
        .and_then(|token| token.clone().into_uint())
        .ok_or_else(|| anyhow!("router output did not contain a uint256 amountOut"))
}

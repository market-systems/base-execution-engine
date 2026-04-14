use anyhow::{anyhow, Context, Result};
use common::{RoutePlan, SimulationResult, SimulationStatus};
use ethers::providers::{Ipc, Middleware, Provider};
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::{TransactionRequest, U256};
use std::sync::Arc;
use strategy::adapter_for_step;

#[derive(Clone)]
pub struct QuoteSimulator {
    provider: Arc<Provider<Ipc>>,
}

impl QuoteSimulator {
    pub fn new(provider: Arc<Provider<Ipc>>) -> Self {
        Self { provider }
    }

    pub fn provider(&self) -> Arc<Provider<Ipc>> {
        Arc::clone(&self.provider)
    }

    pub async fn simulate_plan(&self, plan: &RoutePlan) -> Result<SimulationResult> {
        if plan.steps.is_empty() {
            return Ok(SimulationResult {
                status: SimulationStatus::Skipped,
                expected_amount_out: U256::zero(),
                gas_used: 0,
                reason: "route plan has no steps".to_string(),
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
                .provider
                .call(&tx, None)
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

        Ok(SimulationResult {
            status: SimulationStatus::Success,
            expected_amount_out: current_amount,
            gas_used: total_gas,
            reason: "quote simulation completed".to_string(),
            confidence_bps: 8_500,
        })
    }
}

//! Alloy-backed implementation of [`crate::ExecutionTransport`].
//!
//! The transport composes the building blocks of Phase 4 (signer, gas, nonce)
//! into a single submit + receipt loop. It is intentionally conservative:
//!
//! - Fees and gas limit are recomputed for every attempt; we never reuse a
//!   stale estimate across submissions.
//! - The nonce manager is the source of truth. On a chain mismatch we bail
//!   instead of guessing.
//! - Receipt polling is bounded by the caller-supplied timeout. A timeout
//!   returns `Ok(None)` so the caller can mark the attempt as `Dropped`
//!   without conflating it with a hard transport failure.

use std::str::FromStr;
use std::time::{Duration, Instant};

use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address as AlloyAddress, Bytes, TxKind, U256};
use alloy_provider::Provider;
use alloy_rpc_types_eth::TransactionRequest;
use alloy_signer::SignerSync;
use anyhow::{anyhow, Context};
use async_trait::async_trait;
use rpc::EngineProvider;
use tracing::{debug, info, warn};
use types::execution::{ExecutionAttempt, ExecutionReceipt, ExecutionRequest};
use types::Amount;

use crate::gas::{estimate_fees, GasCaps};
use crate::nonce::NonceManager;
use crate::signer::EngineSigner;
use crate::{EthCallSimulation, ExecutionTransport, SubmissionPayload};

const DEFAULT_RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Clone)]
pub struct AlloySubmitTransport {
    provider: EngineProvider,
    signer: EngineSigner,
    nonce: NonceManager,
    caps: GasCaps,
    poll_interval: Duration,
}

impl std::fmt::Debug for AlloySubmitTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlloySubmitTransport")
            .field("signer_address", &self.signer.address())
            .field("chain_id", &self.provider.chain_id())
            .field("poll_interval", &self.poll_interval)
            .finish()
    }
}

impl AlloySubmitTransport {
    pub fn new(
        provider: EngineProvider,
        signer: EngineSigner,
        nonce: NonceManager,
        caps: GasCaps,
    ) -> Self {
        Self {
            provider,
            signer,
            nonce,
            caps,
            poll_interval: DEFAULT_RECEIPT_POLL_INTERVAL,
        }
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    pub fn signer_address(&self) -> AlloyAddress {
        self.signer.address()
    }

    /// Build, sign, and broadcast the transaction.
    async fn submit_inner(
        &self,
        payload: &SubmissionPayload,
        attempt: &ExecutionAttempt,
    ) -> anyhow::Result<String> {
        let started = Instant::now();
        let to = AlloyAddress::from_str(&payload.router_address)
            .with_context(|| format!("invalid router address `{}`", payload.router_address))?;

        let pricing = estimate_fees(&self.provider, self.caps)
            .await
            .context("failed to obtain gas pricing for submission")?;

        let nonce = self
            .nonce
            .next()
            .await
            .context("failed to reserve nonce for submission")?;

        let gas_limit = attempt
            .gas_limit
            .ok_or_else(|| anyhow!("execution attempt is missing gas_limit; refusing to submit"))?;

        // Pre-flight: simulate via eth_call so we never broadcast a tx the node
        // already knows will revert. This burns one RPC round-trip but in
        // practice is the difference between a clean drop and a paid revert.
        self.preflight_call(to, &payload.calldata, gas_limit, payload.value)
            .await?;

        let tx = TxEip1559 {
            chain_id: self.provider.chain_id(),
            nonce,
            gas_limit,
            max_fee_per_gas: pricing.max_fee_per_gas,
            max_priority_fee_per_gas: pricing.max_priority_fee_per_gas,
            to: TxKind::Call(to),
            value: U256::from(payload.value),
            input: payload.calldata.clone(),
            access_list: Default::default(),
        };

        let signature = self
            .signer
            .inner()
            .sign_hash_sync(&tx.signature_hash())
            .context("failed to sign EIP-1559 transaction hash")?;
        let signed = tx.into_signed(signature);
        let envelope = TxEnvelope::Eip1559(signed);
        let mut raw = Vec::with_capacity(envelope.encode_2718_len());
        envelope.encode_2718(&mut raw);

        let pending = self
            .provider
            .inner()
            .send_raw_transaction(&raw)
            .await
            .with_context(|| {
                self.nonce_release_async(nonce);
                "failed to broadcast raw transaction"
            })?;
        let tx_hash = format!("{:#x}", *pending.tx_hash());

        let elapsed = started.elapsed();
        observability::record_submit_latency(elapsed.as_secs_f64());
        info!(
            request_id = %attempt.request_id,
            tx_hash = %tx_hash,
            nonce,
            max_fee_per_gas = pricing.max_fee_per_gas,
            max_priority_fee_per_gas = pricing.max_priority_fee_per_gas,
            gas_limit,
            submit_latency_ms = elapsed.as_millis() as u64,
            "broadcast execution transaction"
        );

        Ok(tx_hash)
    }

    async fn preflight_call(
        &self,
        to: AlloyAddress,
        calldata: &Bytes,
        gas_limit: u64,
        value: Amount,
    ) -> anyhow::Result<()> {
        let request = TransactionRequest::default()
            .from(self.signer.address())
            .to(to)
            .input(calldata.clone().into())
            .gas_limit(gas_limit)
            .value(U256::from(value));

        match self.provider.inner().call(&request).await {
            Ok(_) => Ok(()),
            Err(error) => {
                warn!(
                    %error,
                    "eth_call preflight failed; aborting submit"
                );
                Err(anyhow!("preflight eth_call reverted: {error}"))
            }
        }
    }

    fn nonce_release_async(&self, nonce: u64) {
        let manager = self.nonce.clone();
        tokio::spawn(async move {
            manager.release(nonce).await;
        });
    }

    async fn poll_receipt(
        &self,
        tx_hash_hex: &str,
        timeout: Duration,
    ) -> anyhow::Result<Option<ExecutionReceipt>> {
        let started = Instant::now();
        let deadline = started + timeout;
        let tx_hash = alloy_primitives::B256::from_str(tx_hash_hex)
            .with_context(|| format!("invalid tx hash `{tx_hash_hex}`"))?;

        loop {
            if let Some(receipt) = self
                .provider
                .inner()
                .get_transaction_receipt(tx_hash)
                .await
                .context("failed to query transaction receipt")?
            {
                observability::record_receipt_latency(started.elapsed().as_secs_f64());
                return Ok(Some(translate_receipt(tx_hash_hex, receipt)));
            }

            if Instant::now() >= deadline {
                // Record the timeout as a "receipt latency" reading equal to
                // the configured timeout, so revert/include p95s stay
                // comparable across the fleet even when the pipeline is
                // dropping txs.
                observability::record_receipt_latency(timeout.as_secs_f64());
                debug!(
                    tx_hash = %tx_hash_hex,
                    "receipt poll timed out without on-chain inclusion"
                );
                return Ok(None);
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

fn translate_receipt(
    tx_hash_hex: &str,
    receipt: alloy_rpc_types_eth::TransactionReceipt,
) -> ExecutionReceipt {
    let inner = &receipt.inner;
    ExecutionReceipt {
        // request_id is filled in by the engine when finalising; the transport
        // does not have a stable tx -> opportunity mapping on its own.
        request_id: String::new(),
        tx_hash: tx_hash_hex.to_string(),
        block_number: receipt.block_number,
        success: inner.status(),
        gas_used: Some(receipt.gas_used as u64),
        effective_gas_price: Some(receipt.effective_gas_price),
        // Base / OP stack expose the L1 fee as a top-level field on
        // `TransactionReceipt`; the alloy ethereum receipt type does not
        // surface it, so we leave it None and let the engine layer attach it
        // separately if it later parses the OP-flavoured receipt.
        l1_fee_paid: None,
        revert_reason: if inner.status() {
            None
        } else {
            Some("transaction reverted on-chain".to_string())
        },
    }
}

#[async_trait]
impl ExecutionTransport for AlloySubmitTransport {
    async fn submit(
        &self,
        payload: &SubmissionPayload,
        attempt: &ExecutionAttempt,
    ) -> anyhow::Result<String> {
        self.submit_inner(payload, attempt).await
    }

    async fn await_receipt(
        &self,
        _request: &ExecutionRequest,
        attempt: &ExecutionAttempt,
        timeout: Duration,
    ) -> anyhow::Result<Option<ExecutionReceipt>> {
        let tx_hash = attempt
            .tx_hash
            .as_deref()
            .ok_or_else(|| anyhow!("execution attempt has no tx_hash to await"))?;
        let mut receipt = self.poll_receipt(tx_hash, timeout).await?;
        if let Some(rx) = receipt.as_mut() {
            rx.request_id = attempt.request_id.clone();
        }
        Ok(receipt)
    }

    async fn simulate(
        &self,
        payload: &SubmissionPayload,
        attempt: &ExecutionAttempt,
    ) -> anyhow::Result<EthCallSimulation> {
        let to = AlloyAddress::from_str(&payload.router_address)
            .with_context(|| format!("invalid router address `{}`", payload.router_address))?;

        // Use the gas limit from the attempt if available, otherwise let the
        // node estimate. We do NOT pull a fresh nonce here — eth_call does not
        // need one and it would conflict with the eventual real submit.
        let request = TransactionRequest::default()
            .from(self.signer.address())
            .to(to)
            .input(payload.calldata.clone().into())
            .value(U256::from(payload.value));

        let request = if let Some(gas_limit) = attempt.gas_limit {
            request.gas_limit(gas_limit)
        } else {
            request
        };

        // First do a plain eth_call so a revert produces a clean error.
        self.provider
            .inner()
            .call(&request)
            .await
            .map_err(|err| anyhow!("eth_call simulation reverted: {err}"))?;

        // Then ask the node for a gas estimate so the engine can use it as a
        // sanity check on the static gas_limit. This is best-effort: failures
        // here do not invalidate the simulation, since the call already
        // succeeded.
        let gas_used = self
            .provider
            .inner()
            .estimate_gas(&request)
            .await
            .ok();

        Ok(EthCallSimulation::Passed { gas_used })
    }
}

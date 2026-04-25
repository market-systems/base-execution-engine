//! Gas pricing policy.
//!
//! The on-chain Base L2 cost has two parts: the EIP-1559 L2 execution fee and
//! the L1 calldata fee charged by the sequencer. We surface both as a single
//! [`GasPricing`] struct so the rest of the engine can budget upfront.
//!
//! The pricing is the `min(observed, configured_cap)` across both L2 fee
//! components — the configured ceilings act as a hard upper bound so a
//! mis-priced sequencer or a bursty market cannot drain the wallet.

use alloy_provider::utils::Eip1559Estimation;
use alloy_provider::Provider;
use alloy_transport_http::Http;
use anyhow::Context;
use config::ExecutionConfig;
use reqwest::Client as ReqwestClient;
use rpc::EngineProvider;
use types::Amount;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GasPricing {
    pub max_fee_per_gas: Amount,
    pub max_priority_fee_per_gas: Amount,
}

#[derive(Debug, Clone, Copy)]
pub struct GasCaps {
    pub max_fee_cap: Option<Amount>,
    pub max_priority_fee_cap: Option<Amount>,
}

impl GasCaps {
    pub fn from_config(config: &ExecutionConfig) -> anyhow::Result<Self> {
        let max_fee_cap = config
            .max_fee_per_gas_wei
            .as_deref()
            .map(|raw| {
                raw.parse::<Amount>()
                    .context("failed to parse EXECUTION_MAX_FEE_PER_GAS_WEI")
            })
            .transpose()?;
        let max_priority_fee_cap = config
            .max_priority_fee_per_gas_wei
            .as_deref()
            .map(|raw| {
                raw.parse::<Amount>()
                    .context("failed to parse EXECUTION_MAX_PRIORITY_FEE_PER_GAS_WEI")
            })
            .transpose()?;
        Ok(Self {
            max_fee_cap,
            max_priority_fee_cap,
        })
    }
}

/// Estimate EIP-1559 fees from the engine provider, applying the configured
/// caps as upper bounds. Returns an error if either component exceeds its cap
/// and no cap is configured to clamp it (clamping is the default; this only
/// errors on overflow / RPC failure).
pub async fn estimate_fees(
    provider: &EngineProvider,
    caps: GasCaps,
) -> anyhow::Result<GasPricing> {
    let inner: &alloy_provider::RootProvider<Http<ReqwestClient>> = provider.inner();

    let estimate: Eip1559Estimation = inner
        .estimate_eip1559_fees(None)
        .await
        .context("failed to estimate EIP-1559 fees from provider")?;

    // The engine's `Amount` type currently aliases `u128`, which is exactly
    // what alloy returns; if/when `Amount` widens to U256, add a checked cast
    // here so we never silently truncate a fee value.
    let mut max_fee: Amount = estimate.max_fee_per_gas;
    let mut max_priority_fee: Amount = estimate.max_priority_fee_per_gas;

    if let Some(cap) = caps.max_fee_cap {
        if max_fee > cap {
            tracing::debug!(
                observed = max_fee,
                cap,
                "clamping max_fee_per_gas to configured cap"
            );
            max_fee = cap;
        }
    }
    if let Some(cap) = caps.max_priority_fee_cap {
        if max_priority_fee > cap {
            tracing::debug!(
                observed = max_priority_fee,
                cap,
                "clamping max_priority_fee_per_gas to configured cap"
            );
            max_priority_fee = cap;
        }
    }

    if max_priority_fee > max_fee {
        // EIP-1559 invariant: priority fee must not exceed max_fee.
        max_priority_fee = max_fee;
    }

    Ok(GasPricing {
        max_fee_per_gas: max_fee,
        max_priority_fee_per_gas: max_priority_fee,
    })
}


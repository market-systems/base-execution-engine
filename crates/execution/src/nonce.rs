//! Nonce manager.
//!
//! The manager keeps a single monotonically-increasing counter per signer
//! address. It is initialised from `eth_getTransactionCount(addr, "pending")`
//! at startup (via [`NonceManager::sync_from_chain`]) and then advanced
//! locally for every successful submission. A helper to "rewind" on
//! `nonce too low` errors is exposed because the engine prefers to refresh
//! and retry rather than silently drop a tx.

use std::sync::Arc;

use alloy_primitives::Address as AlloyAddress;
use alloy_provider::Provider;
use alloy_rpc_types_eth::BlockNumberOrTag;
use anyhow::Context;
use rpc::EngineProvider;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
pub struct NonceManager {
    state: Arc<Mutex<NonceState>>,
}

#[derive(Debug, Default)]
struct NonceState {
    next: u64,
    initialised: bool,
}

impl NonceManager {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(NonceState::default())),
        }
    }

    /// Refresh the local counter from the chain's pending nonce. Always
    /// overwrites the local view; safe to call after a `nonce too low` revert
    /// or after a planned wallet rotation.
    pub async fn sync_from_chain(
        &self,
        provider: &EngineProvider,
        signer_address: AlloyAddress,
    ) -> anyhow::Result<u64> {
        let pending = provider
            .inner()
            .get_transaction_count(signer_address)
            .block_id(BlockNumberOrTag::Pending.into())
            .await
            .context("failed to fetch pending transaction count from provider")?;

        let mut state = self.state.lock().await;
        state.next = pending;
        state.initialised = true;
        Ok(pending)
    }

    /// Reserve the next nonce. Caller MUST submit using exactly this value;
    /// drop the call site if you cannot follow through, or call
    /// [`NonceManager::release`] to roll back.
    pub async fn next(&self) -> anyhow::Result<u64> {
        let mut state = self.state.lock().await;
        if !state.initialised {
            anyhow::bail!("nonce manager has not been initialised; call sync_from_chain first");
        }
        let nonce = state.next;
        state.next = state.next.checked_add(1).context("nonce counter overflow")?;
        Ok(nonce)
    }

    /// Roll back the last reserved nonce. Use sparingly: only safe if the tx
    /// was never broadcast.
    pub async fn release(&self, expected: u64) {
        let mut state = self.state.lock().await;
        if state.next == expected.saturating_add(1) {
            state.next = expected;
        }
    }
}

impl Default for NonceManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn next_fails_until_initialised() {
        let manager = NonceManager::new();
        let error = manager.next().await.unwrap_err();
        assert!(error.to_string().contains("not been initialised"));
    }

    #[tokio::test]
    async fn release_rewinds_only_when_state_matches() {
        let manager = NonceManager::new();
        {
            let mut state = manager.state.lock().await;
            state.next = 7;
            state.initialised = true;
        }
        let reserved = manager.next().await.unwrap();
        assert_eq!(reserved, 7);
        manager.release(7).await;
        let reserved_again = manager.next().await.unwrap();
        assert_eq!(reserved_again, 7);
    }
}

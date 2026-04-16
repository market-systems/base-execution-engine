use anyhow::Result;
use common::{AttemptStatus, ExecutionAttempt};
use ethers::types::{Address, BlockId, H256, U64};
use ingress::ChainClient;
use state::StateStore;
use std::{
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Clone)]
pub struct ReceiptReconciler {
    chain: ChainClient,
    submission_timeout_micros: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingOutcome {
    StillPending,
    Replaced,
    Dropped,
    TimedOut,
}

impl ReceiptReconciler {
    pub fn new(chain: ChainClient, submission_timeout_secs: u64) -> Self {
        Self {
            chain,
            submission_timeout_micros: submission_timeout_secs.saturating_mul(1_000_000),
        }
    }

    pub async fn reconcile_once(&self, store: &StateStore) -> Result<usize> {
        let attempts = store.latest_attempts()?;
        let mut updates = 0usize;

        for attempt in attempts {
            if attempt.status != AttemptStatus::BroadcastSubmitted {
                continue;
            }

            let Some(tx_hash) = attempt.dispatch.as_ref().and_then(|dispatch| dispatch.tx_hash) else {
                continue;
            };

            if let Some(receipt) = self.chain.get_transaction_receipt(tx_hash).await? {
                let mut next_attempt = attempt.clone();
                next_attempt.receipt_block_number = receipt.block_number.map(|value| value.as_u64());
                next_attempt.receipt_gas_used = receipt.gas_used.map(|value| value.as_u64());

                if receipt.status.unwrap_or_else(U64::zero).as_u64() == 1 {
                    next_attempt.status = AttemptStatus::Included;
                } else {
                    next_attempt.status = AttemptStatus::OnchainReverted;
                    if next_attempt.fail_reason.is_none() {
                        next_attempt.fail_reason =
                            Some("transaction was included but reverted onchain".to_string());
                    }
                }

                store.append_attempt(next_attempt.bump_version())?;
                updates = updates.saturating_add(1);
                continue;
            }

            let next_status = self.classify_pending_attempt(&attempt, tx_hash).await?;
            let Some(next_status) = next_status else {
                continue;
            };

            let mut next_attempt = attempt.clone();
            next_attempt.status = next_status;
            if next_attempt.fail_reason.is_none() {
                next_attempt.fail_reason = Some(match next_status {
                    AttemptStatus::Replaced => {
                        "transaction nonce has advanced and the original submission appears replaced"
                            .to_string()
                    }
                    AttemptStatus::Dropped => {
                        "transaction is no longer visible and appears dropped before inclusion"
                            .to_string()
                    }
                    AttemptStatus::TimedOut => {
                        "transaction is still pending beyond the configured submission timeout"
                            .to_string()
                    }
                    _ => next_attempt.decision.reason.clone(),
                });
            }
            store.append_attempt(next_attempt.bump_version())?;
            updates = updates.saturating_add(1);
        }

        Ok(updates)
    }

    async fn classify_pending_attempt(
        &self,
        attempt: &ExecutionAttempt,
        tx_hash: H256,
    ) -> Result<Option<AttemptStatus>> {
        let tx_present = self.chain.get_transaction(tx_hash).await?.is_some();
        let pending_outcome = self.pending_outcome(attempt, tx_present).await?;

        Ok(match pending_outcome {
            PendingOutcome::StillPending => None,
            PendingOutcome::Replaced => Some(AttemptStatus::Replaced),
            PendingOutcome::Dropped => Some(AttemptStatus::Dropped),
            PendingOutcome::TimedOut => Some(AttemptStatus::TimedOut),
        })
    }

    async fn pending_outcome(
        &self,
        attempt: &ExecutionAttempt,
        tx_present: bool,
    ) -> Result<PendingOutcome> {
        let Some(dispatch) = attempt.dispatch.as_ref() else {
            return Ok(PendingOutcome::StillPending);
        };

        let aged_out = now_micros()?
            .saturating_sub(attempt.updated_at_micros)
            >= self.submission_timeout_micros;

        let account_nonce_advanced = match (dispatch.submitted_from, dispatch.submitted_nonce) {
            (Some(from), Some(submitted_nonce)) => {
                self.account_nonce_advanced(from, submitted_nonce).await?
            }
            _ => false,
        };

        Ok(classify_pending_state(
            aged_out,
            tx_present,
            account_nonce_advanced,
        ))
    }

    async fn account_nonce_advanced(&self, from: Address, submitted_nonce: u64) -> Result<bool> {
        let latest_nonce = self
            .chain
            .get_transaction_count(from, Some(BlockId::Number(ethers::types::BlockNumber::Latest)))
            .await?;
        Ok(latest_nonce.as_u64() > submitted_nonce)
    }
}

fn now_micros() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_micros()
        .min(u64::MAX as u128) as u64)
}

fn classify_pending_state(
    aged_out: bool,
    tx_present: bool,
    account_nonce_advanced: bool,
) -> PendingOutcome {
    if account_nonce_advanced && !tx_present {
        return PendingOutcome::Replaced;
    }

    if !aged_out {
        return PendingOutcome::StillPending;
    }

    if tx_present {
        PendingOutcome::TimedOut
    } else {
        PendingOutcome::Dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_missing_old_transaction_as_dropped() {
        assert_eq!(
            classify_pending_state(true, false, false),
            PendingOutcome::Dropped
        );
    }

    #[test]
    fn keeps_recent_visible_transaction_pending() {
        assert_eq!(
            classify_pending_state(false, true, false),
            PendingOutcome::StillPending
        );
    }

    #[test]
    fn marks_missing_nonce_advanced_transaction_as_replaced() {
        assert_eq!(
            classify_pending_state(false, false, true),
            PendingOutcome::Replaced
        );
    }

    #[test]
    fn marks_visible_old_transaction_as_timed_out() {
        assert_eq!(
            classify_pending_state(true, true, false),
            PendingOutcome::TimedOut
        );
    }
}

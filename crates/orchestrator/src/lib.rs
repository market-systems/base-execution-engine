use anyhow::Result;
use common::{AttemptStatus, DecisionStatus, ExecutionAttempt};
use state::StateStore;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrchestrationOutcome {
    pub advanced: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Orchestrator;

impl Orchestrator {
    pub fn new() -> Self {
        Self
    }

    pub fn advance_attempt(
        &self,
        store: &StateStore,
        attempt: &ExecutionAttempt,
    ) -> Result<OrchestrationOutcome> {
        if attempt.status != AttemptStatus::PolicyChecked {
            return Ok(OrchestrationOutcome { advanced: false });
        }

        let next_attempt = if attempt.decision.status == DecisionStatus::Approved {
            let mut next_attempt = attempt.clone();
            next_attempt.status = if next_attempt.decision.plan.is_some() {
                AttemptStatus::ReadyToSubmit
            } else {
                AttemptStatus::Rejected
            };
            if next_attempt.decision.plan.is_none() {
                next_attempt.fail_reason = Some(
                    "approved attempt is missing a route plan and cannot become submit-ready"
                        .to_string(),
                );
            }
            next_attempt
        } else {
            let mut next_attempt = attempt.clone();
            next_attempt.status = AttemptStatus::Rejected;
            if next_attempt.fail_reason.is_none() {
                next_attempt.fail_reason = Some(next_attempt.decision.reason.clone());
            }
            next_attempt
        };

        store.append_attempt(next_attempt.bump_version())?;
        Ok(OrchestrationOutcome { advanced: true })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        DecisionStatus, EvaluationStatus, ExecutionDecision, PipelineTimings,
    };
    use ethers::types::H256;

    fn sample_attempt() -> ExecutionAttempt {
        ExecutionAttempt {
            id: "attempt-1".to_string(),
            intent_id: "intent-1".to_string(),
            routes_considered: 1,
            candidate: None,
            decision: ExecutionDecision {
                status: DecisionStatus::Approved,
                reason: "looks good".to_string(),
                plan: Some(common::RoutePlan {
                    source_token: Default::default(),
                    target_token: Default::default(),
                    amount_in: Default::default(),
                    steps: Vec::new(),
                    expected_amount_out: Default::default(),
                    estimated_gas: 123,
                }),
                simulation: None,
            },
            request: None,
            dispatch: None,
            evaluation_status: EvaluationStatus::Approved,
            rejection_reason: None,
            timings: PipelineTimings::default(),
            status: AttemptStatus::PolicyChecked,
            fail_reason: None,
            receipt_block_number: None,
            receipt_gas_used: None,
            created_at_micros: 1,
            updated_at_micros: 1,
            version: 1,
        }
    }

    #[test]
    fn advances_policy_checked_attempt_to_ready_to_submit() {
        let root = std::env::temp_dir().join(format!("execution-engine-orchestrator-{}", H256::from_low_u64_be(3)));
        let store = StateStore::new(&root).unwrap();
        let attempt = sample_attempt();

        let outcome = Orchestrator::new().advance_attempt(&store, &attempt).unwrap();
        assert!(outcome.advanced);

        let latest = store.latest_attempts().unwrap();
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].status, AttemptStatus::ReadyToSubmit);

        let _ = std::fs::remove_dir_all(root);
    }
}

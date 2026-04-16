use anyhow::{Context, Result};
use common::{
    AttemptStatus, DispatchResult, DispatchStatus, EvaluationStatus, ExecutionAttempt,
    ExecutionEvaluation, ExecutionIntent, ExecutionRequest, IntentStatus,
};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const INTENTS_LOG: &str = "execution_intents.jsonl";
const ATTEMPTS_LOG: &str = "execution_attempts.jsonl";

#[derive(Clone, Debug)]
pub struct StateStore {
    root_dir: PathBuf,
}

impl StateStore {
    pub fn new(root_dir: impl AsRef<Path>) -> Result<Self> {
        let root_dir = root_dir.as_ref().to_path_buf();
        fs::create_dir_all(&root_dir)
            .with_context(|| format!("failed to create state directory {}", root_dir.display()))?;
        Ok(Self { root_dir })
    }

    pub fn persist_execution_result(
        &self,
        record: &ExecutionEvaluation,
        request: Option<ExecutionRequest>,
        dispatch: Option<DispatchResult>,
    ) -> Result<(ExecutionIntent, ExecutionAttempt)> {
        let now_micros = now_micros()?;
        let tx_hash = format!("{:?}", record.tx_hash);

        let intent = ExecutionIntent {
            id: format!("intent-{tx_hash}-{now_micros}"),
            observed_tx_hash: record.tx_hash,
            actor: record.candidate.as_ref().map(|candidate| candidate.intent.actor),
            source: record.candidate.as_ref().map(|candidate| candidate.intent.source),
            trigger: record.candidate.as_ref().map(|candidate| candidate.intent.clone()),
            status: intent_status(record.status),
            created_at_micros: now_micros,
            updated_at_micros: now_micros,
        };

        let attempt = ExecutionAttempt {
            id: format!("attempt-{tx_hash}-{now_micros}"),
            intent_id: intent.id.clone(),
            routes_considered: record.routes_considered,
            candidate: record.candidate.clone(),
            decision: record.decision.clone(),
            request,
            dispatch: dispatch.clone(),
            evaluation_status: record.status,
            rejection_reason: record.rejection_reason,
            timings: record.timings,
            status: attempt_status(record.status, dispatch.as_ref()),
            fail_reason: fail_reason(record, dispatch.as_ref()),
            receipt_block_number: None,
            receipt_gas_used: None,
            created_at_micros: now_micros,
            updated_at_micros: now_micros,
            version: 1,
        };

        self.append_json_line(INTENTS_LOG, &intent)?;
        self.append_attempt(attempt.clone())?;

        Ok((intent, attempt))
    }

    pub fn append_attempt(&self, attempt: ExecutionAttempt) -> Result<()> {
        self.append_json_line(ATTEMPTS_LOG, &attempt)
    }

    pub fn latest_attempts(&self) -> Result<Vec<ExecutionAttempt>> {
        let path = self.root_dir.join(ATTEMPTS_LOG);
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = fs::File::open(&path)
            .with_context(|| format!("failed to open attempt log {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut attempts = HashMap::<String, ExecutionAttempt>::new();

        for line in reader.lines() {
            let line = line.with_context(|| format!("failed to read line from {}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }

            let attempt: ExecutionAttempt = serde_json::from_str(&line)
                .with_context(|| format!("failed to parse attempt record in {}", path.display()))?;
            match attempts.get(&attempt.id) {
                Some(existing) if existing.version >= attempt.version => {}
                _ => {
                    attempts.insert(attempt.id.clone(), attempt);
                }
            }
        }

        let mut latest_attempts = attempts.into_values().collect::<Vec<_>>();
        latest_attempts.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(latest_attempts)
    }

    fn append_json_line<T: serde::Serialize>(&self, file_name: &str, value: &T) -> Result<()> {
        let path = self.root_dir.join(file_name);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open state log {}", path.display()))?;
        serde_json::to_writer(&mut file, value)
            .with_context(|| format!("failed to serialize state record to {}", path.display()))?;
        file.write_all(b"\n")
            .with_context(|| format!("failed to finalize state log line {}", path.display()))?;
        file.flush()
            .with_context(|| format!("failed to flush state log {}", path.display()))?;
        Ok(())
    }
}

fn now_micros() -> Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before unix epoch")?;
    Ok(duration.as_micros().min(u64::MAX as u128) as u64)
}

fn intent_status(status: EvaluationStatus) -> IntentStatus {
    match status {
        EvaluationStatus::Approved => IntentStatus::Qualified,
        EvaluationStatus::Rejected => IntentStatus::Rejected,
        EvaluationStatus::Skipped => IntentStatus::FilteredOut,
    }
}

fn attempt_status(status: EvaluationStatus, dispatch: Option<&DispatchResult>) -> AttemptStatus {
    match (status, dispatch.map(|dispatch| dispatch.status)) {
        (EvaluationStatus::Approved, Some(DispatchStatus::Suppressed)) => AttemptStatus::BroadcastSuppressed,
        (EvaluationStatus::Approved, Some(DispatchStatus::Submitted)) => AttemptStatus::BroadcastSubmitted,
        (EvaluationStatus::Approved, Some(DispatchStatus::Failed)) => AttemptStatus::BroadcastFailed,
        (EvaluationStatus::Approved, None) => AttemptStatus::PolicyChecked,
        (EvaluationStatus::Rejected, _) => AttemptStatus::Rejected,
        (EvaluationStatus::Skipped, _) => AttemptStatus::Skipped,
    }
}

fn fail_reason(record: &ExecutionEvaluation, dispatch: Option<&DispatchResult>) -> Option<String> {
    match (record.status, dispatch) {
        (EvaluationStatus::Approved, Some(dispatch)) if dispatch.status == DispatchStatus::Failed => {
            Some(dispatch.reason.clone())
        }
        (EvaluationStatus::Approved, Some(_)) => None,
        (EvaluationStatus::Approved, None) => None,
        _ => Some(record.decision.reason.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{
        DecisionStatus, EvaluationRejectionReason, EvaluationStatus, ExecutionDecision,
        ExecutionEvaluation, PipelineTimings,
    };
    use ethers::types::H256;

    #[test]
    fn persists_evaluations_as_intent_and_attempt() {
        let root = std::env::temp_dir().join(format!("execution-engine-state-{}", now_micros().unwrap()));
        let store = StateStore::new(&root).unwrap();
        let record = ExecutionEvaluation {
            tx_hash: H256::from_low_u64_be(7),
            candidate: None,
            routes_considered: 2,
            decision: ExecutionDecision {
                status: DecisionStatus::Denied,
                reason: "policy denied".to_string(),
                plan: None,
                simulation: None,
            },
            status: EvaluationStatus::Rejected,
            rejection_reason: Some(EvaluationRejectionReason::PolicyDenied),
            timings: PipelineTimings::default(),
        };

        let (_intent, attempt) = store.persist_execution_result(&record, None, None).unwrap();
        assert_eq!(attempt.status, AttemptStatus::Rejected);
        assert!(root.join(INTENTS_LOG).exists());
        assert!(root.join(ATTEMPTS_LOG).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keeps_only_latest_attempt_version_per_attempt_id() {
        let root = std::env::temp_dir().join(format!("execution-engine-state-latest-{}", now_micros().unwrap()));
        let store = StateStore::new(&root).unwrap();
        let attempt_v1 = ExecutionAttempt {
            id: "attempt-1".to_string(),
            intent_id: "intent-1".to_string(),
            routes_considered: 1,
            candidate: None,
            decision: ExecutionDecision {
                status: DecisionStatus::Approved,
                reason: "ok".to_string(),
                plan: None,
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
        };
        let mut attempt_v2 = attempt_v1.clone();
        attempt_v2.status = AttemptStatus::ReadyToSubmit;
        attempt_v2.version = 2;
        attempt_v2.updated_at_micros = 2;

        store.append_attempt(attempt_v1).unwrap();
        store.append_attempt(attempt_v2).unwrap();

        let attempts = store.latest_attempts().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].status, AttemptStatus::ReadyToSubmit);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persists_dispatch_status_after_broadcast() {
        let root = std::env::temp_dir().join(format!("execution-engine-state-dispatch-{}", now_micros().unwrap()));
        let store = StateStore::new(&root).unwrap();
        let record = ExecutionEvaluation {
            tx_hash: H256::from_low_u64_be(9),
            candidate: None,
            routes_considered: 1,
            decision: ExecutionDecision {
                status: DecisionStatus::Approved,
                reason: "approved".to_string(),
                plan: None,
                simulation: None,
            },
            status: EvaluationStatus::Approved,
            rejection_reason: None,
            timings: PipelineTimings::default(),
        };

        let dispatch = DispatchResult {
            status: DispatchStatus::Suppressed,
            tx_hash: None,
            submitted_from: None,
            submitted_nonce: None,
            reason: "shadow".to_string(),
        };

        let (_, attempt) = store
            .persist_execution_result(&record, None, Some(dispatch))
            .unwrap();
        assert_eq!(attempt.status, AttemptStatus::BroadcastSuppressed);

        let _ = fs::remove_dir_all(root);
    }
}

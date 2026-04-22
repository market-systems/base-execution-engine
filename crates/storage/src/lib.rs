#![forbid(unsafe_code)]

//! Postgres storage bootstrap and production pipeline persistence.

use anyhow::{anyhow, Context};
use config::StorageConfig;
use sha2::Digest;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;
use types::decision::{Opportunity, RiskDecision, SimulationResult};
use types::execution::{ExecutionAttempt, ExecutionOutcome, ExecutionReceipt, ExecutionRequest};
use types::ingest::Log as IngestLog;

const INIT_SCHEMA_SQL: &str = include_str!("../../../migration/00000000-00-init.sql");
const PRIVILEGES_SQL: &str = include_str!("../../../migration/~privileges.sql");

pub struct Storage {
    pool: Option<PgPool>,
}

#[derive(Debug, Clone)]
pub struct DecisionRunRecord {
    pub observed_log_id: Option<i64>,
    pub trigger_tx_hash: Option<String>,
    pub opportunity: Opportunity,
    pub simulation: SimulationResult,
    pub risk: RiskDecision,
    pub execution_request: Option<ExecutionRequest>,
}

impl Storage {
    pub async fn connect(config: &StorageConfig) -> anyhow::Result<Self> {
        let Some(database_url) = &config.database_url else {
            tracing::info!("storage disabled; no database url configured");
            return Ok(Self { pool: None });
        };

        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(Duration::from_secs(config.connect_timeout_secs))
            .connect(database_url)
            .await
            .with_context(|| "failed to connect to postgres")?;

        if config.auto_migrate {
            sqlx::raw_sql(INIT_SCHEMA_SQL)
                .execute(&pool)
                .await
                .with_context(|| "failed to apply init schema")?;
            sqlx::raw_sql(PRIVILEGES_SQL)
                .execute(&pool)
                .await
                .with_context(|| "failed to apply privileges schema")?;
            tracing::info!("storage migrations applied");
        }

        tracing::info!(
            max_connections = config.max_connections,
            auto_migrate = config.auto_migrate,
            "postgres storage connected"
        );

        Ok(Self { pool: Some(pool) })
    }

    pub fn is_enabled(&self) -> bool {
        self.pool.is_some()
    }

    pub fn pool(&self) -> Option<&PgPool> {
        self.pool.as_ref()
    }

    pub async fn persist_observed_log(&self, log: &IngestLog) -> anyhow::Result<Option<i64>> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };

        let fingerprint = observed_log_fingerprint(log)?;
        let log_json = serde_json::to_value(log).context("failed to serialize observed log")?;
        let row_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO observed_logs (
                fingerprint,
                tx_hash,
                block_context,
                chain_id,
                address,
                event_signature,
                log_index,
                removed,
                log_json
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (fingerprint)
            DO UPDATE SET
                observed_at = NOW(),
                tx_hash = EXCLUDED.tx_hash,
                block_context = EXCLUDED.block_context,
                chain_id = EXCLUDED.chain_id,
                address = EXCLUDED.address,
                event_signature = EXCLUDED.event_signature,
                log_index = EXCLUDED.log_index,
                removed = EXCLUDED.removed,
                log_json = EXCLUDED.log_json
            RETURNING id
            "#,
        )
        .bind(fingerprint)
        .bind(&log.metadata.tx_hash)
        .bind(serde_json::to_string(&log.metadata.block_context)?)
        .bind(i64::try_from(log.metadata.chain_id).context("chain_id does not fit into i64")?)
        .bind(&log.address)
        .bind(&log.event_signature)
        .bind(log.log_index.and_then(|value| i64::try_from(value).ok()))
        .bind(log.removed)
        .bind(log_json)
        .fetch_one(pool)
        .await
        .with_context(|| "failed to persist observed log")?;

        Ok(Some(row_id))
    }

    pub async fn persist_decision_run(
        &self,
        record: &DecisionRunRecord,
    ) -> anyhow::Result<Option<i64>> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };

        let execution_request = serde_json::to_value(&record.execution_request)
            .context("failed to serialize execution request")?;
        let opportunity =
            serde_json::to_value(&record.opportunity).context("failed to serialize opportunity")?;
        let simulation =
            serde_json::to_value(&record.simulation).context("failed to serialize simulation")?;
        let risk = serde_json::to_value(&record.risk).context("failed to serialize risk")?;

        let row_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO decision_runs (
                observed_log_id,
                opportunity_id,
                trigger_tx_hash,
                block_context,
                settlement_token,
                input_amount_wei,
                expected_output_amount_wei,
                expected_net_profit_wei,
                simulated_gross_profit_wei,
                simulated_net_profit_wei,
                gas_estimate,
                gas_cost_wei,
                l1_data_fee_wei,
                risk_accepted,
                risk_hash,
                min_surplus_wei,
                opportunity_json,
                simulation_json,
                risk_json,
                execution_request_json
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
            )
            RETURNING id
            "#,
        )
        .bind(record.observed_log_id)
        .bind(&record.opportunity.id)
        .bind(&record.trigger_tx_hash)
        .bind(serde_json::to_string(&record.opportunity.block_context)?)
        .bind(&record.opportunity.settlement_token)
        .bind(record.opportunity.input_amount.to_string())
        .bind(record.opportunity.expected_output_amount.to_string())
        .bind(record.opportunity.expected_net_profit.to_string())
        .bind(record.simulation.expected_gross_profit.to_string())
        .bind(record.simulation.expected_net_profit.to_string())
        .bind(
            i64::try_from(record.simulation.gas_estimate)
                .context("gas estimate does not fit into i64")?,
        )
        .bind(record.simulation.gas_cost_wei.to_string())
        .bind(record.simulation.l1_data_fee.to_string())
        .bind(record.risk.accepted)
        .bind(&record.risk.risk_hash)
        .bind(record.risk.min_surplus.to_string())
        .bind(opportunity)
        .bind(simulation)
        .bind(risk)
        .bind(execution_request)
        .fetch_one(pool)
        .await
        .with_context(|| "failed to persist decision run")?;

        Ok(Some(row_id))
    }

    pub async fn persist_execution_attempt(
        &self,
        decision_run_id: Option<i64>,
        attempt: &ExecutionAttempt,
        request: &ExecutionRequest,
    ) -> anyhow::Result<Option<i64>> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };

        let attempt_json =
            serde_json::to_value(attempt).context("failed to serialize execution attempt")?;
        let request_json =
            serde_json::to_value(request).context("failed to serialize execution request")?;

        let row_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO execution_attempts (
                decision_run_id,
                request_id,
                status,
                execution_mode,
                submission_allowed,
                blocked_reason,
                requested_notional_wei,
                tx_hash,
                submitted_at,
                finalized_at,
                inclusion_block_number,
                gas_limit,
                gas_used,
                max_fee_per_gas_wei,
                max_priority_fee_per_gas_wei,
                effective_gas_price_wei,
                l1_fee_paid_wei,
                total_fee_paid_wei,
                realized_surplus_wei,
                outcome_reason,
                last_error_stage,
                last_error,
                execution_request_json,
                attempt_json,
                receipt_json,
                outcome_json
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, NULL, NULL, NULL,
                $9, NULL, $10, $11, NULL, NULL, NULL, NULL, NULL, NULL, NULL, $12, $13, NULL, NULL
            )
            RETURNING id
            "#,
        )
        .bind(decision_run_id)
        .bind(&attempt.request_id)
        .bind(serde_json::to_string(&attempt.status)?)
        .bind(&attempt.execution_mode)
        .bind(attempt.submission_allowed)
        .bind(&attempt.blocked_reason)
        .bind(attempt.requested_notional.to_string())
        .bind(&attempt.tx_hash)
        .bind(
            attempt
                .gas_limit
                .and_then(|value| i64::try_from(value).ok()),
        )
        .bind(attempt.max_fee_per_gas.map(|value| value.to_string()))
        .bind(
            attempt
                .max_priority_fee_per_gas
                .map(|value| value.to_string()),
        )
        .bind(request_json)
        .bind(attempt_json)
        .fetch_one(pool)
        .await
        .with_context(|| "failed to persist execution attempt")?;

        Ok(Some(row_id))
    }

    pub async fn mark_execution_submitted(
        &self,
        execution_attempt_id: i64,
        attempt: &ExecutionAttempt,
    ) -> anyhow::Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let attempt_json =
            serde_json::to_value(attempt).context("failed to serialize submitted attempt")?;

        let result = sqlx::query(
            r#"
            UPDATE execution_attempts
            SET status = $2,
                tx_hash = $3,
                submitted_at = NOW(),
                attempt_json = $4
            WHERE id = $1
            "#,
        )
        .bind(execution_attempt_id)
        .bind(serde_json::to_string(&attempt.status)?)
        .bind(&attempt.tx_hash)
        .bind(attempt_json)
        .execute(pool)
        .await
        .with_context(|| "failed to mark execution attempt as submitted")?;

        if result.rows_affected() == 0 {
            return Err(anyhow!(
                "no execution attempt row found for submission update"
            ));
        }

        Ok(())
    }

    pub async fn finalize_execution_attempt(
        &self,
        execution_attempt_id: i64,
        attempt: &ExecutionAttempt,
        receipt: &ExecutionReceipt,
        outcome: &ExecutionOutcome,
    ) -> anyhow::Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let attempt_json =
            serde_json::to_value(attempt).context("failed to serialize finalized attempt")?;
        let receipt_json =
            serde_json::to_value(receipt).context("failed to serialize execution receipt")?;
        let outcome_json =
            serde_json::to_value(outcome).context("failed to serialize execution outcome")?;

        let result = sqlx::query(
            r#"
            UPDATE execution_attempts
            SET status = $2,
                tx_hash = $3,
                finalized_at = NOW(),
                inclusion_block_number = $4,
                gas_used = $5,
                effective_gas_price_wei = $6,
                l1_fee_paid_wei = $7,
                total_fee_paid_wei = $8,
                realized_surplus_wei = $9,
                outcome_reason = $10,
                last_error_stage = NULL,
                last_error = NULL,
                attempt_json = $11,
                receipt_json = $12,
                outcome_json = $13
            WHERE id = $1
            "#,
        )
        .bind(execution_attempt_id)
        .bind(serde_json::to_string(&attempt.status)?)
        .bind(&attempt.tx_hash)
        .bind(
            receipt
                .block_number
                .and_then(|value| i64::try_from(value).ok()),
        )
        .bind(receipt.gas_used.and_then(|value| i64::try_from(value).ok()))
        .bind(receipt.effective_gas_price.map(|value| value.to_string()))
        .bind(receipt.l1_fee_paid.map(|value| value.to_string()))
        .bind(outcome.total_fee_paid.map(|value| value.to_string()))
        .bind(outcome.realized_surplus.map(|value| value.to_string()))
        .bind(&outcome.reason)
        .bind(attempt_json)
        .bind(receipt_json)
        .bind(outcome_json)
        .execute(pool)
        .await
        .with_context(|| "failed to finalize execution attempt")?;

        if result.rows_affected() == 0 {
            return Err(anyhow!(
                "no execution attempt row found for finalization update"
            ));
        }

        Ok(())
    }

    pub async fn finalize_execution_attempt_without_receipt(
        &self,
        execution_attempt_id: i64,
        attempt: &ExecutionAttempt,
        outcome: &ExecutionOutcome,
    ) -> anyhow::Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let attempt_json = serde_json::to_value(attempt)
            .context("failed to serialize finalized attempt without receipt")?;
        let outcome_json =
            serde_json::to_value(outcome).context("failed to serialize execution outcome")?;

        let result = sqlx::query(
            r#"
            UPDATE execution_attempts
            SET status = $2,
                tx_hash = $3,
                finalized_at = NOW(),
                total_fee_paid_wei = $4,
                realized_surplus_wei = $5,
                outcome_reason = $6,
                last_error_stage = NULL,
                last_error = NULL,
                attempt_json = $7,
                outcome_json = $8
            WHERE id = $1
            "#,
        )
        .bind(execution_attempt_id)
        .bind(serde_json::to_string(&attempt.status)?)
        .bind(&attempt.tx_hash)
        .bind(outcome.total_fee_paid.map(|value| value.to_string()))
        .bind(outcome.realized_surplus.map(|value| value.to_string()))
        .bind(&outcome.reason)
        .bind(attempt_json)
        .bind(outcome_json)
        .execute(pool)
        .await
        .with_context(|| "failed to finalize execution attempt without receipt")?;

        if result.rows_affected() == 0 {
            return Err(anyhow!(
                "no execution attempt row found for finalization update without receipt"
            ));
        }

        Ok(())
    }

    pub async fn record_execution_attempt_error(
        &self,
        execution_attempt_id: i64,
        stage: &str,
        error: &str,
    ) -> anyhow::Result<()> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let result = sqlx::query(
            r#"
            UPDATE execution_attempts
            SET last_error_stage = $2,
                last_error = $3
            WHERE id = $1
            "#,
        )
        .bind(execution_attempt_id)
        .bind(stage)
        .bind(error)
        .execute(pool)
        .await
        .with_context(|| "failed to record execution attempt error")?;

        if result.rows_affected() == 0 {
            return Err(anyhow!("no execution attempt row found for error update"));
        }

        Ok(())
    }

    pub async fn todays_submittable_notional_wei(&self) -> anyhow::Result<types::Amount> {
        let Some(pool) = &self.pool else {
            return Ok(0);
        };

        let total: String = sqlx::query_scalar(
            r#"
            SELECT COALESCE(SUM((requested_notional_wei)::numeric), 0)::text
            FROM execution_attempts
            WHERE submission_allowed = TRUE
              AND observed_at >= (date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC')
            "#,
        )
        .fetch_one(pool)
        .await
        .with_context(|| "failed to load current daily requested notional")?;

        total.parse::<types::Amount>().with_context(|| {
            format!("failed to parse current daily requested notional `{total}` into u128")
        })
    }
}

fn observed_log_fingerprint(log: &IngestLog) -> anyhow::Result<String> {
    let mut hasher = sha2::Sha256::new();
    hasher.update(serde_json::to_vec(&log.metadata.block_context)?);
    hasher.update(log.metadata.chain_id.to_le_bytes());
    hasher.update(log.metadata.observed_at_ms.to_le_bytes());
    if let Some(tx_hash) = &log.metadata.tx_hash {
        hasher.update(tx_hash.as_bytes());
    }
    if let Some(address) = &log.address {
        hasher.update(address.as_bytes());
    }
    if let Some(event_signature) = &log.event_signature {
        hasher.update(event_signature.as_bytes());
    }
    if let Some(log_index) = log.log_index {
        hasher.update(log_index.to_le_bytes());
    }
    if let Some(removed) = log.removed {
        hasher.update([removed as u8]);
    }
    hasher.update(serde_json::to_vec(log)?);
    Ok(format!("{:x}", hasher.finalize()))
}

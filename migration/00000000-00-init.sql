CREATE TABLE IF NOT EXISTS observed_logs (
    id BIGSERIAL PRIMARY KEY,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    fingerprint TEXT NOT NULL UNIQUE,
    tx_hash TEXT,
    block_context TEXT NOT NULL,
    chain_id BIGINT NOT NULL,
    address TEXT,
    event_signature TEXT,
    log_index BIGINT,
    removed BOOLEAN,
    log_json JSONB NOT NULL
);

CREATE INDEX IF NOT EXISTS observed_logs_observed_at_idx
    ON observed_logs (observed_at DESC);

CREATE INDEX IF NOT EXISTS observed_logs_tx_hash_idx
    ON observed_logs (tx_hash);

CREATE TABLE IF NOT EXISTS decision_runs (
    id BIGSERIAL PRIMARY KEY,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    observed_log_id BIGINT REFERENCES observed_logs(id),
    opportunity_id TEXT NOT NULL,
    trigger_tx_hash TEXT,
    block_context TEXT NOT NULL,
    settlement_token TEXT NOT NULL,
    input_amount_wei TEXT NOT NULL,
    expected_output_amount_wei TEXT NOT NULL,
    expected_net_profit_wei TEXT NOT NULL,
    simulated_gross_profit_wei TEXT NOT NULL,
    simulated_net_profit_wei TEXT NOT NULL,
    gas_estimate BIGINT NOT NULL,
    gas_cost_wei TEXT NOT NULL,
    l1_data_fee_wei TEXT NOT NULL,
    risk_accepted BOOLEAN NOT NULL,
    risk_hash TEXT NOT NULL,
    min_surplus_wei TEXT NOT NULL,
    opportunity_json JSONB NOT NULL,
    simulation_json JSONB NOT NULL,
    risk_json JSONB NOT NULL,
    execution_request_json JSONB
);

CREATE INDEX IF NOT EXISTS decision_runs_observed_at_idx
    ON decision_runs (observed_at DESC);

CREATE INDEX IF NOT EXISTS decision_runs_observed_log_id_idx
    ON decision_runs (observed_log_id);

CREATE INDEX IF NOT EXISTS decision_runs_opportunity_id_idx
    ON decision_runs (opportunity_id);

CREATE INDEX IF NOT EXISTS decision_runs_risk_hash_idx
    ON decision_runs (risk_hash);

CREATE TABLE IF NOT EXISTS execution_attempts (
    id BIGSERIAL PRIMARY KEY,
    observed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    decision_run_id BIGINT REFERENCES decision_runs(id),
    request_id TEXT NOT NULL,
    status TEXT NOT NULL,
    execution_mode TEXT NOT NULL,
    submission_allowed BOOLEAN NOT NULL,
    blocked_reason TEXT,
    requested_notional_wei TEXT NOT NULL,
    tx_hash TEXT,
    submitted_at TIMESTAMPTZ,
    finalized_at TIMESTAMPTZ,
    inclusion_block_number BIGINT,
    gas_limit BIGINT,
    gas_used BIGINT,
    max_fee_per_gas_wei TEXT,
    max_priority_fee_per_gas_wei TEXT,
    effective_gas_price_wei TEXT,
    l1_fee_paid_wei TEXT,
    total_fee_paid_wei TEXT,
    realized_surplus_wei TEXT,
    outcome_reason TEXT,
    last_error_stage TEXT,
    last_error TEXT,
    execution_request_json JSONB NOT NULL,
    attempt_json JSONB NOT NULL,
    receipt_json JSONB,
    outcome_json JSONB
);

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS execution_mode TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS submission_allowed BOOLEAN;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS blocked_reason TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS requested_notional_wei TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS submitted_at TIMESTAMPTZ;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS finalized_at TIMESTAMPTZ;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS inclusion_block_number BIGINT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS gas_used BIGINT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS effective_gas_price_wei TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS l1_fee_paid_wei TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS total_fee_paid_wei TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS realized_surplus_wei TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS outcome_reason TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS last_error_stage TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS last_error TEXT;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS receipt_json JSONB;

ALTER TABLE execution_attempts
    ADD COLUMN IF NOT EXISTS outcome_json JSONB;

UPDATE execution_attempts
SET execution_mode = COALESCE(execution_mode, 'shadow'),
    submission_allowed = COALESCE(submission_allowed, FALSE),
    requested_notional_wei = COALESCE(requested_notional_wei, '0')
WHERE execution_mode IS NULL
   OR submission_allowed IS NULL
   OR requested_notional_wei IS NULL;

ALTER TABLE execution_attempts
    ALTER COLUMN execution_mode SET NOT NULL;

ALTER TABLE execution_attempts
    ALTER COLUMN submission_allowed SET NOT NULL;

ALTER TABLE execution_attempts
    ALTER COLUMN requested_notional_wei SET NOT NULL;

CREATE INDEX IF NOT EXISTS execution_attempts_observed_at_idx
    ON execution_attempts (observed_at DESC);

CREATE INDEX IF NOT EXISTS execution_attempts_decision_run_id_idx
    ON execution_attempts (decision_run_id);

CREATE INDEX IF NOT EXISTS execution_attempts_request_id_idx
    ON execution_attempts (request_id);

CREATE INDEX IF NOT EXISTS execution_attempts_submission_allowed_idx
    ON execution_attempts (submission_allowed, observed_at DESC);

CREATE INDEX IF NOT EXISTS execution_attempts_status_idx
    ON execution_attempts (status, observed_at DESC);

CREATE INDEX IF NOT EXISTS execution_attempts_tx_hash_idx
    ON execution_attempts (tx_hash);

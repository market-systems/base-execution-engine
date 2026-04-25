//! Business metric definitions and typed emit helpers.
//!
//! All metric names live here so callsites can stay free of stringly-typed
//! constants and Prometheus stays well-described (every metric gets a `# HELP`
//! line via `describe_*` at startup).
//!
//! Naming convention: `<subsystem>_<thing>_<unit>`. Counter names get the
//! `_total` suffix Prometheus tooling expects. Histograms use base units
//! (seconds, wei) so Grafana queries don't need division.
//!
//! Labels are kept short and bounded — never include free-form text like
//! revert reasons (those go in tracing instead). Cardinality budget per
//! metric is documented inline.

use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};

// ---- metric names ----

/// Total normalised events delivered by the ingest layer.
/// Labels: `stream` (log|block|transaction|flashblock), `channel`
/// (ws|ipc|flashblocks_ws), `decode_status` (decoded|partial|unsupported|failed).
pub const INGEST_EVENTS_TOTAL: &str = "ingest_events_total";

/// Wall-clock seconds between an ingest event being observed at the
/// connector and being picked up by the main event loop.
/// Labels: `stream`.
pub const INGEST_DECISION_LAG_SECONDS: &str = "ingest_decision_lag_seconds";

/// Two-leg loop opportunities surfaced by the decision pipeline.
/// Labels: `outcome` (detected|simulated|submitted|skipped).
pub const DECISION_OPPORTUNITIES_TOTAL: &str = "decision_opportunities_total";

/// Local quote-math simulator wall-clock duration. Histogram unit: seconds.
pub const SIMULATOR_LOCAL_SECONDS: &str = "simulator_local_seconds";

/// Slow-path `eth_call` simulation wall-clock duration. Histogram unit: seconds.
pub const SIMULATOR_ETHCALL_SECONDS: &str = "simulator_ethcall_seconds";

/// Outcomes of every simulator pass.
/// Labels: `kind` (local|ethcall), `outcome` (ok|reverted|timeout|error).
pub const SIMULATOR_OUTCOMES_TOTAL: &str = "simulator_outcomes_total";

/// Submit-side wall-clock from `submit_inner` start to `eth_sendRawTransaction`
/// resolving (i.e. the node accepted the tx). Excludes receipt wait.
pub const EXECUTION_SUBMIT_SECONDS: &str = "execution_submit_seconds";

/// Time from submit-accepted to a receipt being seen on chain. Capped by the
/// configured receipt timeout; timeouts are recorded as the timeout value.
pub const EXECUTION_RECEIPT_SECONDS: &str = "execution_receipt_seconds";

/// Final outcomes of every executed attempt.
/// Labels: `status` (built|submitted|included|reverted|dropped|replaced|profit_realized).
pub const EXECUTION_OUTCOMES_TOTAL: &str = "execution_outcomes_total";

/// Realized surplus of every settled attempt, in wei. Histogram so Grafana
/// can show p50/p95 ticket size alongside cumulative PnL.
pub const EXECUTION_REALIZED_SURPLUS_WEI: &str = "execution_realized_surplus_wei";

/// Cumulative realized surplus across the process lifetime, in wei. Gauge
/// (not counter) because we want the absolute "where are we right now"
/// reading rather than a rate.
pub const EXECUTION_REALIZED_SURPLUS_TOTAL_WEI: &str = "execution_realized_surplus_total_wei";

/// Cumulative gas + L1 fee paid, in wei. Always increases.
pub const EXECUTION_FEE_PAID_TOTAL_WEI: &str = "execution_fee_paid_total_wei";

// ---- registration ----

/// Register descriptions for every business metric. Idempotent: the metrics
/// crate dedupes by name so callers can run this multiple times without
/// emitting duplicate `# HELP` lines.
pub fn register_business_metrics() {
    describe_counter!(
        INGEST_EVENTS_TOTAL,
        "Normalised ingest events delivered to the main loop, partitioned by stream/channel/decode status."
    );
    describe_histogram!(
        INGEST_DECISION_LAG_SECONDS,
        metrics::Unit::Seconds,
        "Seconds between an event being observed at the connector and being consumed by the decision loop."
    );
    describe_counter!(
        DECISION_OPPORTUNITIES_TOTAL,
        "Two-leg arbitrage opportunities observed during the decision pipeline."
    );
    describe_histogram!(
        SIMULATOR_LOCAL_SECONDS,
        metrics::Unit::Seconds,
        "Wall-clock seconds spent in the local quote-math simulator."
    );
    describe_histogram!(
        SIMULATOR_ETHCALL_SECONDS,
        metrics::Unit::Seconds,
        "Wall-clock seconds spent in the slow-path eth_call simulation."
    );
    describe_counter!(
        SIMULATOR_OUTCOMES_TOTAL,
        "Simulator passes by kind and outcome."
    );
    describe_histogram!(
        EXECUTION_SUBMIT_SECONDS,
        metrics::Unit::Seconds,
        "Seconds from submit_inner start to eth_sendRawTransaction acceptance."
    );
    describe_histogram!(
        EXECUTION_RECEIPT_SECONDS,
        metrics::Unit::Seconds,
        "Seconds from submission acceptance to on-chain receipt observation."
    );
    describe_counter!(
        EXECUTION_OUTCOMES_TOTAL,
        "Final execution outcomes by status."
    );
    describe_histogram!(
        EXECUTION_REALIZED_SURPLUS_WEI,
        metrics::Unit::Count,
        "Realized surplus per settled attempt, in wei."
    );
    describe_gauge!(
        EXECUTION_REALIZED_SURPLUS_TOTAL_WEI,
        metrics::Unit::Count,
        "Cumulative realized surplus across the process lifetime, in wei."
    );
    describe_counter!(
        EXECUTION_FEE_PAID_TOTAL_WEI,
        metrics::Unit::Count,
        "Cumulative gas + L1 fee paid by execution attempts, in wei."
    );
}

// ---- typed emit helpers ----
//
// Helpers exist for two reasons:
// 1. Centralise the label key names so a typo in a callsite doesn't silently
//    create a parallel time series.
// 2. Saturating-cast every numeric to f64 once. The `metrics` crate stores
//    every histogram observation as f64 internally; doing the cast here
//    keeps callsites legible.

pub fn record_ingest_event(stream: &'static str, channel: &str, decode_status: &str) {
    counter!(
        INGEST_EVENTS_TOTAL,
        "stream" => stream,
        "channel" => channel.to_string(),
        "decode_status" => decode_status.to_string()
    )
    .increment(1);
}

pub fn record_decision_lag(stream: &'static str, lag_secs: f64) {
    if !lag_secs.is_finite() || lag_secs < 0.0 {
        return;
    }
    histogram!(INGEST_DECISION_LAG_SECONDS, "stream" => stream).record(lag_secs);
}

pub fn record_opportunity_outcome(outcome: &'static str) {
    counter!(DECISION_OPPORTUNITIES_TOTAL, "outcome" => outcome).increment(1);
}

pub fn record_simulator_local(duration_secs: f64, outcome: &'static str) {
    histogram!(SIMULATOR_LOCAL_SECONDS).record(duration_secs);
    counter!(SIMULATOR_OUTCOMES_TOTAL, "kind" => "local", "outcome" => outcome).increment(1);
}

pub fn record_simulator_ethcall(duration_secs: f64, outcome: &'static str) {
    histogram!(SIMULATOR_ETHCALL_SECONDS).record(duration_secs);
    counter!(SIMULATOR_OUTCOMES_TOTAL, "kind" => "ethcall", "outcome" => outcome).increment(1);
}

pub fn record_submit_latency(duration_secs: f64) {
    histogram!(EXECUTION_SUBMIT_SECONDS).record(duration_secs);
}

pub fn record_receipt_latency(duration_secs: f64) {
    histogram!(EXECUTION_RECEIPT_SECONDS).record(duration_secs);
}

pub fn record_execution_outcome(status: &'static str) {
    counter!(EXECUTION_OUTCOMES_TOTAL, "status" => status).increment(1);
}

/// Record a settled attempt's realized PnL. `surplus_wei` may be negative
/// (revert with consumed gas, dropped tx, ...). The cumulative gauge tracks
/// the running net, which Grafana will plot directly.
///
/// Numbers are converted to `f64` for Prometheus emission. For wei-scale
/// values up to ~9e15 ETH this stays within f64 precision (53 bits ≈ 9e15);
/// beyond that the high-order bits round, which is acceptable for monitoring
/// — the canonical PnL ledger lives in storage.
pub fn record_realized_pnl(surplus_wei: i128, fee_paid_wei: u128) {
    let surplus_f64 = surplus_wei as f64;
    histogram!(EXECUTION_REALIZED_SURPLUS_WEI).record(surplus_f64);

    // The `gauge!` macro's `.increment(f64)` accepts negative deltas, so the
    // cumulative gauge mirrors the integer running total even on revert.
    gauge!(EXECUTION_REALIZED_SURPLUS_TOTAL_WEI).increment(surplus_f64);

    // Saturating cast: realistic Base mainnet fees stay well under u64::MAX
    // (~1.8e19 wei, ~18 ETH per single tx). Saturate just in case so a
    // bogus receipt can't crash the recorder.
    let fee_u64 = u64::try_from(fee_paid_wei).unwrap_or(u64::MAX);
    counter!(EXECUTION_FEE_PAID_TOTAL_WEI).increment(fee_u64);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: emitting before a recorder is installed must not panic.
    /// The metrics crate's default recorder is a no-op, so this also
    /// validates we are not reaching into any global state in helpers.
    #[test]
    fn emit_helpers_are_no_op_safe() {
        register_business_metrics();
        record_ingest_event("log_stream", "ws", "decoded");
        record_decision_lag("log_stream", 0.05);
        record_opportunity_outcome("simulated");
        record_simulator_local(0.001, "ok");
        record_simulator_ethcall(0.012, "reverted");
        record_submit_latency(0.21);
        record_receipt_latency(2.5);
        record_execution_outcome("included");
        record_realized_pnl(123_456, 7_890);
        // Negative surplus path (revert with paid gas).
        record_realized_pnl(-9_000, 9_000);
    }

    #[test]
    fn negative_or_nan_lag_is_dropped() {
        record_decision_lag("log_stream", -1.0);
        record_decision_lag("log_stream", f64::NAN);
        record_decision_lag("log_stream", f64::INFINITY);
    }
}

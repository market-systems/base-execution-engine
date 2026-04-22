use anyhow::Context;
use config::{AppConfig, DecisionConfig};
use decision::{
    assess_risk, build_execution_request, detect_two_leg_loops_from_book,
    simulate_two_leg_opportunity, SimulationConfig,
};
use execution::{
    build_submission_payload, finalize_from_receipt, mark_dropped, mark_submitted,
    prepare_execution, ExecutionTransport, UnavailableTransport,
};
use ingest::IngestPipeline;
use markets::{MarketEventOutcome, PoolBook, V2PoolState};
use observability::ObservabilityRuntime;
use rpc::RpcTopology;
use serde::Deserialize;
use storage::{DecisionRunRecord, Storage};
use types::ingest::Event;
use types::{Amount, Exchange, Protocol};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct BootstrapV2Pool {
    address: String,
    protocol: Protocol,
    exchange: Option<Exchange>,
    token0: String,
    token1: String,
    reserve0: Amount,
    reserve1: Amount,
    fee_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RuntimeScanConfig {
    settlement_token: String,
    amount_in: Amount,
    simulation: SimulationConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = AppConfig::from_env().context("failed to load application config")?;
    let observability = ObservabilityRuntime::init(&config.metrics)
        .context("failed to initialize observability")?;

    let rpc_topology = RpcTopology::from_config(&config.rpc);
    tracing::info!(
        chain_id = rpc_topology.chain_id,
        has_primary_endpoint = rpc_topology.primary.is_some(),
        fallback_count = rpc_topology.fallbacks.len(),
        execution_mode = ?config.execution.mode,
        "engine runtime configured"
    );
    let execution_transport = build_execution_transport();

    let storage = Storage::connect(&config.storage)
        .await
        .context("failed to initialize storage")?;
    tracing::info!(storage_enabled = storage.is_enabled(), "storage ready");

    let mut pool_book =
        load_pool_book(&config.decision).context("failed to bootstrap pool book")?;
    tracing::info!(pool_count = pool_book.v2_pools().count(), "pool book ready");

    let runtime_scan_config = runtime_scan_config(&config.decision)?;
    if let Some(scan) = &runtime_scan_config {
        tracing::info!(
            settlement_token = %scan.settlement_token,
            amount_in = %scan.amount_in,
            gas_estimate = scan.simulation.gas_estimate,
            "event-driven two-leg scanning enabled"
        );
    } else {
        tracing::info!("event-driven two-leg scanning disabled");
    }

    let runtime = IngestPipeline::from_config(&config.ingest)
        .context("failed to build ingest pipeline")?
        .spawn();
    let (mut event_receiver, mut runtime_event_receiver, stream_tasks) = runtime.into_parts();

    let exit_reason = loop {
        tokio::select! {
            event = event_receiver.recv() => {
                match event {
                    Some(event) => {
                        let decision_output =
                            handle_event(&mut pool_book, runtime_scan_config.as_ref(), &event, &storage)
                                .await
                                .context("failed to handle ingest event")?;
                        maybe_log_decision_output(
                            decision_output,
                            &config,
                            &storage,
                            execution_transport.as_ref(),
                        )
                        .await
                        .context("failed to persist decision output")?;
                        tracing::debug!(event = %serde_json::to_string(&event)?, "ingest event");
                    }
                    None => break "event channel closed",
                }
            }
            runtime_event = runtime_event_receiver.recv() => {
                match runtime_event {
                    Some(event) => tracing::info!(runtime_event = %serde_json::to_string(&event)?, "runtime event"),
                    None => break "runtime event channel closed",
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to listen for ctrl-c")?;
                for task in &stream_tasks {
                    task.abort();
                }
                break "received ctrl-c";
            }
        }
    };

    tracing::info!(exit_reason, "shutting down engine");
    observability.shutdown().await?;
    Ok(())
}

async fn handle_event(
    pool_book: &mut PoolBook,
    runtime_scan_config: Option<&RuntimeScanConfig>,
    event: &Event,
    storage: &Storage,
) -> anyhow::Result<Option<DecisionPipelineOutput>> {
    let Event::Log(log) = event else {
        return Ok(None);
    };
    let observed_log_id = storage
        .persist_observed_log(log)
        .await
        .context("failed to persist observed log")?;

    let outcome = pool_book.apply_log(log)?;
    match outcome {
        MarketEventOutcome::Applied | MarketEventOutcome::Reverted => {
            tracing::debug!(
                pool_address = log.address.as_deref().unwrap_or("<unknown>"),
                removed = log.removed.unwrap_or(false),
                outcome = ?outcome,
                "applied market log to pool book"
            );

            if let Some(scan) = runtime_scan_config {
                let opportunities = detect_two_leg_loops_from_book(
                    pool_book,
                    log.metadata.block_context.clone(),
                    &scan.settlement_token,
                    scan.amount_in,
                    log.metadata.tx_hash.clone(),
                );

                if let Some(best) = opportunities.first() {
                    tracing::info!(
                        opportunity_id = %best.id,
                        opportunity_count = opportunities.len(),
                        expected_net_profit = %best.expected_net_profit,
                        expected_output_amount = %best.expected_output_amount,
                        "detected two-leg opportunities after market update"
                    );

                    let simulation = simulate_two_leg_opportunity(best, scan.simulation)
                        .context("failed to simulate two-leg opportunity")?;
                    return Ok(Some(DecisionPipelineOutput {
                        observed_log_id,
                        opportunity: best.clone(),
                        simulation,
                    }));
                }
            }
        }
        MarketEventOutcome::Ignored => {}
    }

    Ok(None)
}

fn load_pool_book(config: &DecisionConfig) -> anyhow::Result<PoolBook> {
    let mut book = PoolBook::new();

    let Some(path) = &config.v2_bootstrap_path else {
        return Ok(book);
    };

    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read bootstrap pool file `{path}`"))?;
    let entries: Vec<BootstrapV2Pool> = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse bootstrap pool file `{path}`"))?;

    for entry in entries {
        let pool = V2PoolState::new(
            entry.address,
            entry.protocol,
            entry.exchange,
            entry.token0,
            entry.token1,
            entry.reserve0,
            entry.reserve1,
            entry.fee_bps,
        )?;
        book.insert_v2_pool(pool);
    }

    Ok(book)
}

fn runtime_scan_config(config: &DecisionConfig) -> anyhow::Result<Option<RuntimeScanConfig>> {
    let (Some(settlement_token), Some(amount)) =
        (&config.settlement_token, &config.scan_trade_amount_wei)
    else {
        return Ok(None);
    };

    let amount_in = amount
        .parse::<Amount>()
        .with_context(|| "failed to parse DECISION_SCAN_TRADE_AMOUNT_WEI")?;

    Ok(Some(RuntimeScanConfig {
        settlement_token: settlement_token.clone(),
        amount_in,
        simulation: SimulationConfig::from_config(config)?,
    }))
}

#[derive(Debug, Clone)]
struct DecisionPipelineOutput {
    observed_log_id: Option<i64>,
    opportunity: types::decision::Opportunity,
    simulation: types::decision::SimulationResult,
}

async fn maybe_log_decision_output(
    output: Option<DecisionPipelineOutput>,
    app_config: &AppConfig,
    storage: &Storage,
    execution_transport: &dyn ExecutionTransport,
) -> anyhow::Result<()> {
    let Some(output) = output else {
        return Ok(());
    };

    let risk = assess_risk(
        &output.opportunity,
        &output.simulation,
        &app_config.decision,
    )
    .context("failed to assess risk for opportunity")?;

    tracing::info!(
        opportunity_id = %output.opportunity.id,
        simulated_net_profit = %output.simulation.expected_net_profit,
        gas_cost_wei = %output.simulation.gas_cost_wei,
        l1_data_fee = %output.simulation.l1_data_fee,
        risk_accepted = risk.accepted,
        "completed decision simulation and risk assessment"
    );

    let execution_request = build_execution_request(&output.opportunity, &risk);
    let prepared_execution = if let Some(request) = &execution_request {
        let current_daily_requested_wei = storage
            .todays_submittable_notional_wei()
            .await
            .context("failed to load current daily execution exposure")?;
        Some(
            prepare_execution(
                &app_config.execution,
                request,
                &output.simulation,
                current_daily_requested_wei,
            )
            .context("failed to prepare execution attempt")?,
        )
    } else {
        None
    };

    let decision_run_id = storage
        .persist_decision_run(&DecisionRunRecord {
            observed_log_id: output.observed_log_id,
            trigger_tx_hash: output.opportunity.trigger_tx_hash.clone(),
            opportunity: output.opportunity.clone(),
            simulation: output.simulation.clone(),
            risk: risk.clone(),
            execution_request: execution_request.clone(),
        })
        .await?;

    if let (Some(request), Some(prepared_execution)) = (&execution_request, prepared_execution) {
        tracing::info!(
            opportunity_id = %request.opportunity_id,
            step_count = request.steps.len(),
            min_repay = %request.min_repay,
            min_surplus = %request.min_surplus,
            risk_hash = %request.risk_hash,
            execution_mode = %prepared_execution.attempt.execution_mode,
            submission_allowed = prepared_execution.attempt.submission_allowed,
            blocked_reason = prepared_execution.attempt.blocked_reason.as_deref().unwrap_or(""),
            requested_notional = %prepared_execution.attempt.requested_notional,
            gas_limit = prepared_execution.attempt.gas_limit.unwrap_or_default(),
            "prepared execution attempt in decision pipeline"
        );

        let execution_attempt_id = storage
            .persist_execution_attempt(decision_run_id, &prepared_execution.attempt, request)
            .await?;

        if prepared_execution.should_submit {
            drive_execution_attempt(
                execution_transport,
                request,
                &prepared_execution.attempt,
                app_config,
                storage,
                execution_attempt_id,
            )
            .await
            .context("failed to drive execution attempt lifecycle")?;
        }
    }

    Ok(())
}

fn build_execution_transport() -> Box<dyn ExecutionTransport> {
    Box::new(UnavailableTransport::new(
        "execution submitter is not configured yet; signer, calldata builder, and RPC submit path are still pending",
    ))
}

async fn drive_execution_attempt(
    execution_transport: &dyn ExecutionTransport,
    request: &types::execution::ExecutionRequest,
    built_attempt: &types::execution::ExecutionAttempt,
    app_config: &AppConfig,
    storage: &Storage,
    execution_attempt_id: Option<i64>,
) -> anyhow::Result<()> {
    let submission_payload = match build_submission_payload(&app_config.execution, request) {
        Ok(payload) => payload,
        Err(error) => {
            tracing::error!(
                request_id = %built_attempt.request_id,
                error = %error,
                "execution build failed"
            );
            if let Some(execution_attempt_id) = execution_attempt_id {
                storage
                    .record_execution_attempt_error(
                        execution_attempt_id,
                        "build",
                        &error.to_string(),
                    )
                    .await?;
            }
            return Ok(());
        }
    };

    let submitted_tx_hash = match tokio::time::timeout(
        std::time::Duration::from_secs(app_config.execution.submit_timeout_secs),
        execution_transport.submit(&submission_payload, built_attempt),
    )
    .await
    {
        Ok(Ok(tx_hash)) => tx_hash,
        Ok(Err(error)) => {
            tracing::error!(
                request_id = %built_attempt.request_id,
                error = %error,
                "execution submission failed"
            );
            if let Some(execution_attempt_id) = execution_attempt_id {
                storage
                    .record_execution_attempt_error(
                        execution_attempt_id,
                        "submit",
                        &error.to_string(),
                    )
                    .await?;
            }
            return Ok(());
        }
        Err(error) => {
            tracing::error!(
                request_id = %built_attempt.request_id,
                error = %error,
                "execution submission timed out"
            );
            if let Some(execution_attempt_id) = execution_attempt_id {
                storage
                    .record_execution_attempt_error(
                        execution_attempt_id,
                        "submit_timeout",
                        &error.to_string(),
                    )
                    .await?;
            }
            return Ok(());
        }
    };

    let submitted_attempt =
        mark_submitted(built_attempt, submitted_tx_hash).context("invalid submitted attempt")?;
    if let Some(execution_attempt_id) = execution_attempt_id {
        storage
            .mark_execution_submitted(execution_attempt_id, &submitted_attempt)
            .await?;
    }

    let maybe_receipt = execution_transport
        .await_receipt(
            request,
            &submitted_attempt,
            std::time::Duration::from_secs(app_config.execution.receipt_timeout_secs),
        )
        .await;

    match maybe_receipt {
        Ok(Some(receipt)) => {
            let finalized = finalize_from_receipt(
                &submitted_attempt,
                receipt.clone(),
                None,
                request.min_surplus,
            )
            .context("failed to finalize execution receipt")?;

            tracing::info!(
                request_id = %request.opportunity_id,
                tx_hash = %finalized.outcome.tx_hash,
                final_status = ?finalized.outcome.final_status,
                "execution finalized from receipt"
            );

            if let Some(execution_attempt_id) = execution_attempt_id {
                storage
                    .finalize_execution_attempt(
                        execution_attempt_id,
                        &finalized.attempt,
                        &receipt,
                        &finalized.outcome,
                    )
                    .await?;
            }
        }
        Ok(None) => {
            let dropped = mark_dropped(
                &submitted_attempt,
                "timed out while waiting for execution receipt",
            )
            .context("failed to mark execution attempt as dropped")?;
            tracing::warn!(
                request_id = %request.opportunity_id,
                tx_hash = dropped.outcome.tx_hash,
                "execution receipt did not arrive before timeout; marked dropped"
            );
            if let Some(execution_attempt_id) = execution_attempt_id {
                storage
                    .finalize_execution_attempt_without_receipt(
                        execution_attempt_id,
                        &dropped.attempt,
                        &dropped.outcome,
                    )
                    .await?;
            }
        }
        Err(error) => {
            tracing::warn!(
                request_id = %request.opportunity_id,
                tx_hash = submitted_attempt.tx_hash.as_deref().unwrap_or(""),
                error = %error,
                "execution receipt lookup failed; attempt remains submitted pending"
            );
            if let Some(execution_attempt_id) = execution_attempt_id {
                storage
                    .record_execution_attempt_error(
                        execution_attempt_id,
                        "receipt_wait",
                        &error.to_string(),
                    )
                    .await?;
            }
        }
    }

    Ok(())
}

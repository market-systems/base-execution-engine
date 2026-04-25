use anyhow::Context;
use config::{AppConfig, DecisionConfig, DiscoveryConfig};
use decision::{
    build_execution_request, detect_two_leg_loops_from_book, simulate_two_leg_opportunity,
    RiskEngine, RiskPolicy, SimulationConfig,
};
use config::ExecutionMode;
use discovery::{AlloyFactoryReader, DiscoveryService, FactoryConfig};
use execution::gas::GasCaps;
use execution::nonce::NonceManager;
use execution::signer::load_signer;
use execution::submit::AlloySubmitTransport;
use execution::{
    build_submission_payload, finalize_from_receipt, mark_dropped, mark_submitted,
    prepare_execution, ExecutionTransport, UnavailableTransport,
};
use ingest::IngestPipeline;
use markets::{MarketEventOutcome, PoolBook, V2PoolState};
use observability::ObservabilityRuntime;
use rpc::{EngineProvider, RpcTopology};
use serde::Deserialize;
use std::sync::Arc;
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
    let execution_transport = build_execution_transport(&rpc_topology, &config)
        .await
        .context("failed to build execution transport")?;

    let risk_engine = std::sync::Arc::new(RiskEngine::new(
        RiskPolicy::from_config(&config.risk).context("failed to build risk policy")?,
    ));
    tracing::info!(
        min_net_profit_wei = %config.risk.min_net_profit_wei,
        max_trade_notional = config.risk.max_trade_notional_wei.as_deref().unwrap_or("unbounded"),
        max_daily_notional = config.risk.max_daily_notional_wei.as_deref().unwrap_or("unbounded"),
        max_consecutive_reverts = config.risk.max_consecutive_reverts,
        "risk engine initialised"
    );

    let storage = Storage::connect(&config.storage)
        .await
        .context("failed to initialize storage")?;
    tracing::info!(storage_enabled = storage.is_enabled(), "storage ready");

    let mut pool_book =
        load_pool_book(&config.decision).context("failed to bootstrap pool book")?;
    if config.discovery.enabled {
        run_pool_discovery(&config.discovery, &rpc_topology, &mut pool_book)
            .await
            .context("failed to run pool discovery")?;
    }
    tracing::info!(
        v2_pool_count = pool_book.v2_pools().count(),
        v3_pool_count = pool_book.v3_pools().count(),
        "pool book ready"
    );

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
                            risk_engine.as_ref(),
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

    // Decision-loop ingest lag = wall-clock from the connector observing
    // this event to it being picked up here. Captures backpressure on the
    // mpsc channel feeding the main loop and downstream blocking work.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(log.metadata.observed_at_ms);
    let lag_ms = now_ms.saturating_sub(log.metadata.observed_at_ms);
    observability::record_decision_lag("log_stream", lag_ms as f64 / 1000.0);

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
                    observability::record_opportunity_outcome("detected");
                    tracing::info!(
                        opportunity_id = %best.id,
                        opportunity_count = opportunities.len(),
                        expected_net_profit = %best.expected_net_profit,
                        expected_output_amount = %best.expected_output_amount,
                        "detected two-leg opportunities after market update"
                    );

                    let sim_started = std::time::Instant::now();
                    let simulation_opt = simulate_two_leg_opportunity(best, scan.simulation);
                    let sim_secs = sim_started.elapsed().as_secs_f64();
                    let Some(simulation) = simulation_opt else {
                        // Local simulator returned `None` (e.g. step rejected
                        // by quote math, slippage out of band). Treat as a
                        // skipped opportunity and bail out — this is the hot
                        // path so we don't want to log loudly.
                        observability::record_simulator_local(sim_secs, "rejected");
                        observability::record_opportunity_outcome("skipped");
                        return Ok(None);
                    };
                    observability::record_simulator_local(sim_secs, "ok");
                    observability::record_opportunity_outcome("simulated");
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

/// Run a one-shot factory scan + multicall hydration pass and inject the
/// discovered pools into `book`. Errors are propagated to the caller; this
/// function is invoked only when `DISCOVERY_ENABLED=true`.
async fn run_pool_discovery(
    config: &DiscoveryConfig,
    rpc_topology: &RpcTopology,
    book: &mut PoolBook,
) -> anyhow::Result<()> {
    let factories_path = config
        .factories_path
        .as_ref()
        .context("DISCOVERY_FACTORIES_PATH must be set when discovery is enabled")?;
    let multicall3 = config
        .multicall3_address
        .as_ref()
        .context("DISCOVERY_MULTICALL3_ADDRESS must be set when discovery is enabled")?;

    let raw = std::fs::read_to_string(factories_path)
        .with_context(|| format!("failed to read factories file `{factories_path}`"))?;
    let factories: Vec<FactoryConfig> = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse factories file `{factories_path}`"))?;

    let provider = EngineProvider::connect_http(rpc_topology)
        .context("failed to connect HTTP rpc provider for discovery")?;
    provider
        .verify_chain_id()
        .await
        .context("rpc chain_id mismatch during discovery bootstrap")?;
    let reader = Arc::new(AlloyFactoryReader::new(provider));
    let mut service = DiscoveryService::new(reader, multicall3)
        .context("failed to construct DiscoveryService")?;
    if let Some(head) = config.pinned_head_block {
        service = service.with_pinned_head(head);
    }

    for factory in &factories {
        match service.run_factory(factory, book).await {
            Ok(report) => tracing::info!(
                factory_address = %report.factory_address,
                kind = ?report.kind,
                from_block = report.from_block,
                to_block = report.to_block,
                pools_scanned = report.pools_scanned,
                pools_inserted = report.pools_inserted,
                "factory discovery complete"
            ),
            Err(err) => tracing::error!(
                factory_address = %factory.address,
                error = %err,
                "factory discovery failed; continuing with remaining factories"
            ),
        }
    }
    Ok(())
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
    risk_engine: &RiskEngine,
) -> anyhow::Result<()> {
    let Some(output) = output else {
        return Ok(());
    };

    let risk = risk_engine.assess(&output.opportunity, &output.simulation);

    tracing::info!(
        opportunity_id = %output.opportunity.id,
        simulated_net_profit = %output.simulation.expected_net_profit,
        gas_cost_wei = %output.simulation.gas_cost_wei,
        l1_data_fee = %output.simulation.l1_data_fee,
        risk_accepted = risk.accepted,
        risk_reason = risk.reason.as_deref().unwrap_or(""),
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
            // Slow-path eth_call validation: replay the same calldata against
            // the live router. Cheap local sims miss things like router
            // pause, allowance drift, or tax/honeypot tokens that only show
            // up at execution time.
            let payload_for_sim = build_submission_payload(&app_config.execution, request)
                .context("failed to build submission payload for eth_call simulation")?;
            let sim_outcome = run_ethcall_simulation(
                execution_transport,
                &payload_for_sim,
                &prepared_execution.attempt,
                app_config,
            )
            .await;

            match sim_outcome {
                Ok(simulation) => {
                    tracing::info!(
                        request_id = %request.opportunity_id,
                        outcome = ?simulation,
                        "eth_call slow-path simulation completed"
                    );
                }
                Err(error) if app_config.execution.ethcall_preflight_required => {
                    tracing::warn!(
                        request_id = %request.opportunity_id,
                        error = %error,
                        "eth_call slow-path simulation failed; aborting submission"
                    );
                    risk_engine.record_outcome(false);
                    if let Some(execution_attempt_id) = execution_attempt_id {
                        storage
                            .record_execution_attempt_error(
                                execution_attempt_id,
                                "ethcall_simulate",
                                &error.to_string(),
                            )
                            .await?;
                    }
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(
                        request_id = %request.opportunity_id,
                        error = %error,
                        "eth_call slow-path simulation failed but is configured as advisory; submitting anyway"
                    );
                }
            }

            drive_execution_attempt(
                execution_transport,
                request,
                &prepared_execution.attempt,
                app_config,
                storage,
                execution_attempt_id,
                risk_engine,
            )
            .await
            .context("failed to drive execution attempt lifecycle")?;
        }
    }

    Ok(())
}

/// Pick the execution transport for the configured mode.
///
/// - `Shadow`: returns the placeholder transport so the engine can run a
///   passive observation loop without holding a signer or RPC handle.
/// - `Canary` / `Live`: builds the alloy-backed transport; requires a working
///   HTTP RPC, a signer source, and a deployed router address. Bails fast on
///   any of those being absent.
async fn build_execution_transport(
    topology: &RpcTopology,
    config: &AppConfig,
) -> anyhow::Result<Box<dyn ExecutionTransport>> {
    if config.execution.mode == ExecutionMode::Shadow {
        return Ok(Box::new(UnavailableTransport::new(
            "shadow mode: submission disabled by configuration",
        )));
    }

    if topology.first_http().is_none() {
        anyhow::bail!(
            "execution mode `{:?}` requires a HTTP RPC endpoint; set INGEST_HTTP_URL or its alias",
            config.execution.mode
        );
    }

    let provider = EngineProvider::connect_http(topology)
        .context("failed to connect HTTP rpc provider")?;
    provider
        .verify_chain_id()
        .await
        .context("rpc chain id verification failed during bootstrap")?;

    let signer = load_signer(&config.signer, topology.chain_id)
        .context("failed to load signer for execution transport")?;
    tracing::info!(
        signer_address = ?signer.address(),
        chain_id = topology.chain_id,
        "engine signer loaded"
    );

    let nonce = NonceManager::new();
    let initial_nonce = nonce
        .sync_from_chain(&provider, signer.address())
        .await
        .context("failed to initialise signer nonce from chain")?;
    tracing::info!(initial_nonce, "nonce manager primed from chain");

    let caps = GasCaps::from_config(&config.execution)
        .context("invalid gas cap configuration")?;

    let transport = AlloySubmitTransport::new(provider, signer, nonce, caps);
    Ok(Box::new(transport))
}

/// Run the slow-path `eth_call` simulation with a hard timeout. Wrapping in
/// `tokio::time::timeout` keeps a misbehaving RPC node from stalling the
/// pipeline indefinitely; if the timeout fires we treat it as a simulation
/// failure so the caller can apply the configured strict/advisory policy.
async fn run_ethcall_simulation(
    transport: &dyn ExecutionTransport,
    payload: &execution::SubmissionPayload,
    attempt: &types::execution::ExecutionAttempt,
    app_config: &AppConfig,
) -> anyhow::Result<execution::EthCallSimulation> {
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(app_config.execution.ethcall_timeout_secs),
        transport.simulate(payload, attempt),
    )
    .await;
    let elapsed = started.elapsed().as_secs_f64();

    match result {
        Ok(Ok(outcome)) => {
            observability::record_simulator_ethcall(elapsed, "ok");
            Ok(outcome)
        }
        Ok(Err(error)) => {
            observability::record_simulator_ethcall(elapsed, "reverted");
            Err(error)
        }
        Err(_) => {
            observability::record_simulator_ethcall(elapsed, "timeout");
            Err(anyhow::anyhow!(
                "eth_call simulation timed out after {}s",
                app_config.execution.ethcall_timeout_secs
            ))
        }
    }
}

async fn drive_execution_attempt(
    execution_transport: &dyn ExecutionTransport,
    request: &types::execution::ExecutionRequest,
    built_attempt: &types::execution::ExecutionAttempt,
    app_config: &AppConfig,
    storage: &Storage,
    execution_attempt_id: Option<i64>,
    risk_engine: &RiskEngine,
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
            observability::record_execution_outcome("submit_failed");
            // Submission failures count as reverts for the circuit breaker:
            // they consumed a decision slot and possibly a nonce, and the most
            // common cause (RPC reject, preflight revert) usually repeats on
            // the next attempt.
            risk_engine.record_outcome(false);
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
            observability::record_execution_outcome("submit_timeout");
            risk_engine.record_outcome(false);
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
    observability::record_execution_outcome("submitted");
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

            observability::record_execution_outcome(execution_status_label(
                finalized.outcome.final_status,
            ));

            // PnL emit: surplus may be missing (e.g. revert) — fall back to
            // -fee so the cumulative gauge captures the real net cost.
            let fee_paid_wei = finalized.outcome.total_fee_paid.unwrap_or(0);
            let surplus_wei: i128 = match finalized.outcome.realized_surplus {
                Some(value) => value as i128,
                None => -(fee_paid_wei as i128),
            };
            observability::record_realized_pnl(surplus_wei, fee_paid_wei);

            let outcome_success = matches!(
                finalized.outcome.final_status,
                types::execution::ExecutionStatus::Included
                    | types::execution::ExecutionStatus::ProfitRealized
            );
            risk_engine.record_outcome(outcome_success);

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
            observability::record_execution_outcome("dropped");
            // Dropped tx ≠ revert, but it still uses up a nonce window and is
            // a strong signal something is wrong (mempool eviction, sequencer
            // outage). Treat it as a soft revert for breaker purposes.
            risk_engine.record_outcome(false);
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
            // Receipt lookup failure leaves the tx in an unknown state. Bucket
            // it under a distinct label so dashboards can distinguish RPC
            // flakiness from real on-chain reverts.
            observability::record_execution_outcome("receipt_unknown");
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

/// Map an [`ExecutionStatus`] onto its bounded Prometheus label. We intentionally
/// keep this list closed: a new variant requires adding a label here, which
/// keeps the cardinality of `execution_outcomes_total` predictable.
fn execution_status_label(status: types::execution::ExecutionStatus) -> &'static str {
    match status {
        types::execution::ExecutionStatus::Built => "built",
        types::execution::ExecutionStatus::Submitted => "submitted",
        types::execution::ExecutionStatus::Included => "included",
        types::execution::ExecutionStatus::Reverted => "reverted",
        types::execution::ExecutionStatus::Dropped => "dropped",
        types::execution::ExecutionStatus::Replaced => "replaced",
        types::execution::ExecutionStatus::ProfitRealized => "profit_realized",
    }
}

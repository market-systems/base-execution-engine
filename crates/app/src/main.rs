use anyhow::Result;
use common::{
    default_capabilities_for_venue, ActionKind, DecisionStatus, EvaluationRejectionReason,
    EvaluationStatus, ExecutionDecision, ExecutionEvaluation, ExecutionRequest, IngressEvent,
    Mode, OpportunityCandidate, PipelineTimings, PoolEdge, PoolKey, WETH_BASE,
};
use config::EngineConfig;
use decode::IntentDecoder;
use execution::{
    build_execution_request, build_provisional_execution_call, Broadcaster,
    ExecutionBroadcaster, ExecutionBuilderConfig,
};
use ethers::types::{Address, H256};
use graph::{load_seed_graph, LiquidityGraph};
use ingress::{ChainClient, FlashblocksClient};
use policy::ViabilityPolicy;
use reconcile::ReceiptReconciler;
use simulation::QuoteSimulator;
use state::StateStore;
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let config = EngineConfig::from_env()?;
    init_tracing(&config.log_level);

    let chain = ChainClient::connect(&config.base_network).await?;
    let chain = match config.executor_private_key.as_deref() {
        Some(private_key) if !private_key.trim().is_empty() => {
            chain.with_signer(private_key, config.base_chain_id)?
        }
        _ => chain,
    };
    let latest_block = chain.health_check().await?;
    let liquidity_graph = load_runtime_graph(&config);
    let viability_policy = ViabilityPolicy::new(
        config.mode,
        config.min_simulation_confidence_bps,
        config.max_route_gas,
        config.min_net_surplus,
        !matches!(config.mode, Mode::Research) && config.post_trigger_only,
    );
    let broadcaster = ExecutionBroadcaster::from_mode(config.mode, chain.clone())?;
    let state_store = StateStore::new(&config.state_dir)?;
    let reconciler = ReceiptReconciler::new(chain.clone(), config.submission_timeout_secs);
    info!(
        mode = ?config.mode,
        transport = ?chain.transport(),
        latest_block,
        state_dir = %config.state_dir,
        "Base Execution Engine started"
    );

    let reconcile_store = state_store.clone();
    let reconcile_interval_secs = config.reconcile_interval_secs;
    let reconcile_handle = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(reconcile_interval_secs)).await;
            match reconciler.reconcile_once(&reconcile_store).await {
                Ok(updated) if updated > 0 => {
                    debug!(updated_attempts = updated, "reconciled submitted execution attempts");
                }
                Ok(_) => {}
                Err(error) => {
                    warn!(?error, "receipt reconciliation pass failed");
                }
            }
        }
    });

    let simulator = QuoteSimulator::new(chain);
    let intent_decoder = IntentDecoder::default();
    let (sender, mut receiver) = mpsc::channel::<IngressEvent>(4_096);
    let flashblocks_handle = FlashblocksClient::new(config.flashblocks.clone()).spawn(sender);

    loop {
        tokio::select! {
            maybe_event = receiver.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };
                match event {
                    IngressEvent::Transaction(envelope) => {
                        let started_at = Instant::now();
                        if let Some(intent) = intent_decoder.decode(&envelope) {
                            let evaluation = evaluate_execution(
                                &config,
                                &liquidity_graph,
                                &simulator,
                                &viability_policy,
                                intent,
                                started_at,
                            )
                            .await;
                            let (request, dispatch) =
                                dispatch_if_approved(&config, &broadcaster, &evaluation).await;
                            log_execution_evaluation(&evaluation, dispatch.as_ref());
                            if let Err(error) =
                                persist_execution_result(&state_store, &evaluation, request, dispatch)
                            {
                                warn!(tx_hash = ?evaluation.tx_hash, ?error, "failed to persist execution state");
                            }
                        } else {
                            debug!(tx_hash = ?envelope.tx_hash, "ignored undecodable transaction");
                        }
                    }
                    IngressEvent::Log { source, .. } => {
                        debug!(?source, "received pre-confirmation log payload");
                    }
                    IngressEvent::Heartbeat { source } => {
                        debug!(?source, "received ingress heartbeat");
                    }
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                warn!("received termination signal");
                break;
            }
        }
    }

    match flashblocks_handle.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(?error, "Flashblocks task exited with an error"),
        Err(error) => warn!(?error, "Flashblocks task join failed"),
    }
    reconcile_handle.abort();

    Ok(())
}

fn init_tracing(log_level: &str) {
    let filter = EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn load_runtime_graph(config: &EngineConfig) -> LiquidityGraph {
    match load_seed_graph(&config.liquidity_seeds_path) {
        Ok(graph) => graph,
        Err(error) => {
            warn!(
                path = %config.liquidity_seeds_path,
                ?error,
                "failed to load liquidity seed file; using built-in fallback routes"
            );
            fallback_graph(Address::from_low_u64_be(42))
        }
    }
}

fn fallback_graph(token: Address) -> LiquidityGraph {
    let mut graph = LiquidityGraph::default();
    for step in strategy::default_quote_steps(token, None) {
        graph.add_bidirectional_edge(PoolEdge {
            name: step.name,
            venue: step.venue,
            capabilities: default_capabilities_for_venue(step.venue),
            router: step.router,
            quoter: step.quoter,
            token_in: step.token_in,
            token_out: step.token_out,
            fee_bps: step.fee_bps,
            liquidity_score: 10_000,
            path: step.path,
            stable: step.stable,
            pool_key: step.pool_key,
            estimated_gas: step.estimated_gas,
        });
    }
    graph
}

async fn evaluate_execution(
    config: &EngineConfig,
    base_graph: &LiquidityGraph,
    simulator: &QuoteSimulator,
    viability_policy: &ViabilityPolicy,
    intent: common::ObservedIntent,
    started_at: Instant,
) -> ExecutionEvaluation {
    let candidates = match build_candidates(config, intent) {
        Ok(candidates) => candidates,
        Err((tx_hash, rejection_reason, reason)) => {
            return terminal_record(
                tx_hash,
                None,
                0,
                ExecutionDecision {
                    status: DecisionStatus::Denied,
                    reason,
                    plan: None,
                    simulation: None,
                },
                Some(rejection_reason),
                started_at.elapsed(),
                PipelineTimings::default(),
            );
        }
    };

    let planning_started = Instant::now();
    let planned = plan_candidates(config, base_graph, &candidates);
    let planning_elapsed = planning_started.elapsed();

    let Some((candidate, planned)) = planned else {
        return terminal_record(
            candidates[0].intent.tx_hash,
            Some(candidates[0].clone()),
            0,
            ExecutionDecision {
                status: DecisionStatus::Denied,
                reason: "no settlement cycles available for the observed trigger".to_string(),
                plan: None,
                simulation: None,
            },
            Some(EvaluationRejectionReason::NoCandidateRoutes),
            started_at.elapsed(),
            PipelineTimings {
                planning_micros: duration_micros(planning_elapsed),
                ..PipelineTimings::default()
            },
        );
    };

    let simulation_started = Instant::now();
    let selected = select_best_simulation(config, simulator, &planned).await;
    let simulation_elapsed = simulation_started.elapsed();

    let Some((best_plan, best_simulation)) = selected else {
        return terminal_record(
            candidate.intent.tx_hash,
            Some(candidate),
            planned.routes.len(),
            ExecutionDecision {
                status: DecisionStatus::Denied,
                reason: "failed to simulate any planned route".to_string(),
                plan: None,
                simulation: None,
            },
            Some(EvaluationRejectionReason::SimulationFailed),
            started_at.elapsed(),
            PipelineTimings {
                planning_micros: duration_micros(planning_elapsed),
                simulation_micros: duration_micros(simulation_elapsed),
                total_micros: duration_micros(started_at.elapsed()),
                ..PipelineTimings::default()
            },
        );
    };

    let policy_started = Instant::now();
    let decision = viability_policy.evaluate(best_plan, best_simulation);
    let policy_elapsed = policy_started.elapsed();

    let rejection_reason = if decision.status == DecisionStatus::Approved {
        None
    } else {
        Some(EvaluationRejectionReason::PolicyDenied)
    };

    terminal_record(
        candidate.intent.tx_hash,
        Some(candidate),
        planned.routes.len(),
        decision,
        rejection_reason,
        started_at.elapsed(),
        PipelineTimings {
            planning_micros: duration_micros(planning_elapsed),
            simulation_micros: duration_micros(simulation_elapsed),
            policy_micros: duration_micros(policy_elapsed),
            total_micros: duration_micros(started_at.elapsed()),
        },
    )
}

fn build_candidates(
    config: &EngineConfig,
    intent: common::ObservedIntent,
) -> std::result::Result<Vec<OpportunityCandidate>, (H256, EvaluationRejectionReason, String)> {
    match intent.action {
        ActionKind::BuyLike | ActionKind::SellLike | ActionKind::Swap => {}
        _ => {
            return Err((
                intent.tx_hash,
                EvaluationRejectionReason::UnsupportedAction,
                format!("action {:?} is not yet eligible for execution planning", intent.action),
            ))
        }
    }

    let target_token = intent.token_out.or(intent.token_in).ok_or_else(|| {
        (
            intent.tx_hash,
            EvaluationRejectionReason::MissingTokenOut,
            "decoded intent is missing token_in/token_out hints".to_string(),
        )
    })?;
    let amount_in = intent.amount_in.filter(|amount| !amount.is_zero()).ok_or_else(|| {
        (
            intent.tx_hash,
            EvaluationRejectionReason::MissingAmountIn,
            "decoded intent is missing a non-zero amount_in".to_string(),
        )
    })?;

    let candidates = config
        .settlement_tokens
        .iter()
        .copied()
        .map(|settlement_token| OpportunityCandidate {
            intent: intent.clone(),
            settlement_token,
            target_token,
            amount_in,
        })
        .collect::<Vec<_>>();

    Ok(candidates)
}

fn plan_candidates(
    config: &EngineConfig,
    base_graph: &LiquidityGraph,
    candidates: &[OpportunityCandidate],
) -> Option<(OpportunityCandidate, common::PlannedOpportunity)> {
    let mut best = None;

    for candidate in candidates {
        let graph = build_candidate_graph(base_graph, candidate);
        let mut routes = graph.plan_cycles(
            candidate.settlement_token,
            candidate.amount_in,
            config.max_route_hops,
            config.max_candidate_routes,
        );
        routes.retain(|route| {
            route.steps.iter().any(|step| {
                step.token_in == candidate.target_token || step.token_out == candidate.target_token
            })
        });

        if routes.is_empty() {
            continue;
        }

        let planned = common::PlannedOpportunity {
            candidate: candidate.clone(),
            routes,
        };

        let replace = best
            .as_ref()
            .map(|(_, current): &(OpportunityCandidate, common::PlannedOpportunity)| {
                planned.routes[0].expected_amount_out > current.routes[0].expected_amount_out
            })
            .unwrap_or(true);

        if replace {
            best = Some((candidate.clone(), planned));
        }
    }

    best
}

fn build_candidate_graph(
    base_graph: &LiquidityGraph,
    candidate: &OpportunityCandidate,
) -> LiquidityGraph {
    let mut graph = base_graph.clone();
    append_default_steps(&mut graph, candidate.settlement_token, None);
    if let Some(token_in) = candidate.intent.token_in {
        append_default_steps(&mut graph, token_in, None);
    }
    if let Some(token_out) = candidate.intent.token_out {
        append_default_steps(&mut graph, token_out, target_pool_key(candidate, token_out));
    }
    append_default_steps(
        &mut graph,
        candidate.target_token,
        target_pool_key(candidate, candidate.target_token),
    );

    graph
}

fn append_default_steps(graph: &mut LiquidityGraph, token: Address, pool_key: Option<PoolKey>) {
    if token == *WETH_BASE {
        return;
    }

    for step in strategy::default_quote_steps(token, pool_key.clone()) {
        graph.add_bidirectional_edge(PoolEdge {
            name: step.name,
            venue: step.venue,
            capabilities: default_capabilities_for_venue(step.venue),
            router: step.router,
            quoter: step.quoter,
            token_in: step.token_in,
            token_out: step.token_out,
            fee_bps: step.fee_bps,
            liquidity_score: 10_000,
            path: step.path,
            stable: step.stable,
            pool_key: step.pool_key,
            estimated_gas: step.estimated_gas,
        });
    }
}

fn target_pool_key(candidate: &OpportunityCandidate, token: Address) -> Option<PoolKey> {
    let pool_key = candidate.intent.pool_key.clone()?;

    if token == pool_key.currency0 || token == pool_key.currency1 {
        Some(pool_key)
    } else {
        None
    }
}

async fn select_best_simulation(
    config: &EngineConfig,
    simulator: &QuoteSimulator,
    planned: &common::PlannedOpportunity,
) -> Option<(common::RoutePlan, common::SimulationResult)> {
    let mut best: Option<(common::RoutePlan, common::SimulationResult)> = None;

    for route in &planned.routes {
        let provisional_call = config
            .execution_router_address
            .and_then(|router| build_provisional_execution_call(route, router).ok());
        match simulator
            .simulate_candidate_plan(&planned.candidate, route, provisional_call.as_ref())
            .await
        {
            Ok(result) if result.status == common::SimulationStatus::Success => {
                let should_replace = best
                    .as_ref()
                    .map(|(_, current)| {
                        result.net_surplus > current.net_surplus
                            || (result.net_surplus == current.net_surplus
                                && route.estimated_gas
                                    < best.as_ref().map(|(plan, _)| plan.estimated_gas).unwrap_or(u64::MAX))
                    })
                    .unwrap_or(true);

                if should_replace {
                    best = Some((route.clone(), result));
                }
            }
            Ok(result) => {
                debug!(
                    tx_hash = ?planned.candidate.intent.tx_hash,
                    route = %route.token_path().len(),
                    status = ?result.status,
                    reason = %result.reason,
                    "ignored non-successful simulated route"
                );
            }
            Err(error) => {
                debug!(
                    tx_hash = ?planned.candidate.intent.tx_hash,
                    route = %route.token_path().len(),
                    ?error,
                    "route simulation failed"
                );
            }
        }
    }

    best
}

fn terminal_record(
    tx_hash: H256,
    candidate: Option<OpportunityCandidate>,
    routes_considered: usize,
    decision: ExecutionDecision,
    rejection_reason: Option<EvaluationRejectionReason>,
    total_elapsed: std::time::Duration,
    mut timings: PipelineTimings,
) -> ExecutionEvaluation {
    if timings.total_micros == 0 {
        timings.total_micros = duration_micros(total_elapsed);
    }

    let status = match rejection_reason {
        None => EvaluationStatus::Approved,
        Some(
            EvaluationRejectionReason::UnsupportedAction
            | EvaluationRejectionReason::MissingTokenIn
            | EvaluationRejectionReason::MissingTokenOut
            | EvaluationRejectionReason::MissingAmountIn,
        ) => EvaluationStatus::Skipped,
        Some(_) => EvaluationStatus::Rejected,
    };

    ExecutionEvaluation {
        tx_hash,
        candidate,
        routes_considered,
        decision,
        status,
        rejection_reason,
        timings,
    }
}

fn log_execution_evaluation(
    record: &ExecutionEvaluation,
    dispatch: Option<&common::DispatchResult>,
) {
    match record.status {
        EvaluationStatus::Approved | EvaluationStatus::Rejected => {
            info!(
                tx_hash = ?record.tx_hash,
                evaluation_status = ?record.status,
                decision_status = ?record.decision.status,
                rejection_reason = ?record.rejection_reason,
                dispatch_status = ?dispatch.map(|dispatch| dispatch.status),
                routes_considered = record.routes_considered,
                planning_micros = record.timings.planning_micros,
                simulation_micros = record.timings.simulation_micros,
                policy_micros = record.timings.policy_micros,
                total_micros = record.timings.total_micros,
                reason = %record.decision.reason,
                "execution evaluation completed"
            );
            debug!(?record, ?dispatch, "execution record");
        }
        EvaluationStatus::Skipped => {
            debug!(
                tx_hash = ?record.tx_hash,
                evaluation_status = ?record.status,
                rejection_reason = ?record.rejection_reason,
                reason = %record.decision.reason,
                total_micros = record.timings.total_micros,
                "decoded transaction was skipped by the execution pipeline"
            );
        }
    }
}

async fn dispatch_if_approved(
    config: &EngineConfig,
    broadcaster: &impl Broadcaster,
    record: &ExecutionEvaluation,
) -> (Option<ExecutionRequest>, Option<common::DispatchResult>) {
    if record.status != EvaluationStatus::Approved {
        return (None, None);
    }

    if matches!(config.mode, Mode::Research) {
        return (None, None);
    }

    let (candidate, plan, simulation) = match (
        record.candidate.as_ref(),
        record.decision.plan.clone(),
        record.decision.simulation.clone(),
    ) {
        (Some(candidate), Some(plan), Some(simulation)) => (candidate, plan, simulation),
        _ => {
            return (
                None,
                Some(common::DispatchResult {
                    status: common::DispatchStatus::Failed,
                    tx_hash: None,
                    submitted_from: None,
                    submitted_nonce: None,
                    reason: "approved evaluation is missing candidate, plan, or simulation".to_string(),
                }),
            );
        }
    };

    let request = match build_execution_request(
        candidate,
            plan,
            simulation,
            ExecutionBuilderConfig {
                execution_router: config
                    .execution_router_address
                    .expect("validated execution router address for dispatch-capable modes"),
                min_output_bps: config.min_output_bps,
            },
        ) {
        Ok(request) => request,
        Err(error) => {
            return (
                None,
                Some(common::DispatchResult {
                    status: common::DispatchStatus::Failed,
                    tx_hash: None,
                    submitted_from: None,
                    submitted_nonce: None,
                    reason: error.to_string(),
                }),
            );
        }
    };

    match broadcaster.dispatch(&request).await {
        Ok(dispatch) => (Some(request), Some(dispatch)),
        Err(error) => (
            Some(request),
            Some(common::DispatchResult {
                status: common::DispatchStatus::Failed,
                tx_hash: None,
                submitted_from: None,
                submitted_nonce: None,
                reason: error.to_string(),
            }),
        ),
    }
}

fn persist_execution_result(
    store: &StateStore,
    record: &ExecutionEvaluation,
    request: Option<ExecutionRequest>,
    dispatch: Option<common::DispatchResult>,
) -> Result<()> {
    let (intent, attempt) = store.persist_execution_result(record, request, dispatch)?;
    debug!(
        tx_hash = ?record.tx_hash,
        intent_id = %intent.id,
        attempt_id = %attempt.id,
        attempt_status = ?attempt.status,
        "persisted execution state"
    );
    Ok(())
}

fn duration_micros(duration: std::time::Duration) -> u64 {
    duration.as_micros().min(u64::MAX as u128) as u64
}

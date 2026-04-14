use anyhow::Result;
use common::{IngressEvent, Mode};
use config::EngineConfig;
use decode::IntentDecoder;
use ethers::types::{Address, U256};
use graph::{load_seed_graph, LiquidityGraph};
use ingress::{ChainClient, FlashblocksClient};
use simulation::QuoteSimulator;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let config = EngineConfig::from_env()?;
    init_tracing(&config.log_level);

    let chain = ChainClient::connect(&config.base_ipc_path).await?;
    let latest_block = chain.health_check().await?;
    info!(
        mode = ?config.mode,
        latest_block,
        "Base Execution Engine started"
    );

    let simulator = QuoteSimulator::new(chain.provider());
    let intent_decoder = IntentDecoder::default();
    let (sender, mut receiver) = mpsc::channel::<IngressEvent>(4_096);
    let flashblocks_handle = FlashblocksClient::new(config.flashblocks.clone()).spawn(sender);

    if matches!(config.mode, Mode::Research | Mode::Shadow) {
        bootstrap_graph(&config, &simulator).await;
    }

    loop {
        tokio::select! {
            maybe_event = receiver.recv() => {
                let Some(event) = maybe_event else {
                    break;
                };
                match event {
                    IngressEvent::Transaction(envelope) => {
                        if let Some(intent) = intent_decoder.decode(&envelope) {
                            info!(
                                tx_hash = ?intent.tx_hash,
                                actor = ?intent.actor,
                                venue = ?intent.venue,
                                action = ?intent.action,
                                selector = intent.raw_selector.as_deref().unwrap_or("unknown"),
                                "decoded pre-confirmation intent"
                            );
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

    Ok(())
}

fn init_tracing(log_level: &str) {
    let filter = EnvFilter::try_new(log_level)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

async fn bootstrap_graph(config: &EngineConfig, simulator: &QuoteSimulator) {
    let token = Address::from_low_u64_be(42);
    let graph = match load_seed_graph(&config.liquidity_seeds_path) {
        Ok(graph) => graph,
        Err(error) => {
            warn!(
                path = %config.liquidity_seeds_path,
                ?error,
                "failed to load liquidity seed file; using built-in fallback routes"
            );
            fallback_graph(token)
        }
    };

    let plans = graph.plan_routes(*common::WETH_BASE, token, U256::exp10(16), 2, 4);
    if let Some(plan) = plans.first() {
        match simulator.simulate_plan(plan).await {
            Ok(result) => debug!(?result, "bootstrap route simulation completed"),
            Err(error) => debug!(?error, "bootstrap route simulation skipped"),
        }
    }
}

fn fallback_graph(token: Address) -> LiquidityGraph {
    let mut graph = LiquidityGraph::default();
    for step in strategy::default_quote_steps(token, None) {
        graph.add_edge(common::PoolEdge {
            name: step.name,
            venue: step.venue,
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

use anyhow::{anyhow, Result};
use common::{Mode, BASE_CHAIN_ID};
use dotenv::dotenv;
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashblocksConfig {
    pub ws_url: String,
    pub subscribe_transactions: bool,
    pub subscribe_pending_logs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub mode: Mode,
    pub base_chain_id: u64,
    pub base_ipc_path: String,
    pub liquidity_seeds_path: String,
    pub flashblocks: FlashblocksConfig,
    pub max_route_hops: usize,
    pub max_candidate_routes: usize,
    pub log_level: String,
}

impl EngineConfig {
    pub fn from_env() -> Result<Self> {
        dotenv().ok();

        let mode = parse_mode(
            &env::var("ENGINE_MODE").unwrap_or_else(|_| "research".to_string()),
        )?;
        let base_ipc_path = env::var("BASE_IPC_PATH")
            .unwrap_or_else(|_| "/var/lib/reth/reth.ipc".to_string());

        let config = Self {
            mode,
            base_chain_id: get_u64("BASE_CHAIN_ID", BASE_CHAIN_ID),
            base_ipc_path,
            liquidity_seeds_path: env::var("LIQUIDITY_SEEDS_PATH")
                .unwrap_or_else(|_| "config/base-liquidity-seeds.example.json".to_string()),
            flashblocks: FlashblocksConfig {
                ws_url: env::var("FLASHBLOCKS_WS_URL")
                    .unwrap_or_else(|_| "wss://mainnet-preconf.base.org".to_string()),
                subscribe_transactions: get_bool("FLASHBLOCKS_SUBSCRIBE_TRANSACTIONS", true),
                subscribe_pending_logs: get_bool("FLASHBLOCKS_SUBSCRIBE_PENDING_LOGS", true),
            },
            max_route_hops: get_usize("MAX_ROUTE_HOPS", 3),
            max_candidate_routes: get_usize("MAX_CANDIDATE_ROUTES", 16),
            log_level: env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.base_chain_id != BASE_CHAIN_ID {
            return Err(anyhow!(
                "Base Execution Engine currently supports chain id {} only",
                BASE_CHAIN_ID
            ));
        }

        if self.max_route_hops == 0 {
            return Err(anyhow!("MAX_ROUTE_HOPS must be greater than zero"));
        }

        if self.max_candidate_routes == 0 {
            return Err(anyhow!(
                "MAX_CANDIDATE_ROUTES must be greater than zero"
            ));
        }

        if self.liquidity_seeds_path.trim().is_empty() {
            return Err(anyhow!("LIQUIDITY_SEEDS_PATH must not be empty"));
        }

        Ok(())
    }
}

fn get_bool(key: &str, default: bool) -> bool {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or(default)
}

fn get_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn get_usize(key: &str, default: usize) -> usize {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_mode(value: &str) -> Result<Mode> {
    match value.to_ascii_lowercase().as_str() {
        "research" => Ok(Mode::Research),
        "shadow" => Ok(Mode::Shadow),
        "live" => Ok(Mode::Live),
        _ => Err(anyhow!("unsupported ENGINE_MODE: {value}")),
    }
}

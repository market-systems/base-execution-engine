use anyhow::{anyhow, Context, Result};
use common::{Mode, USDC_BASE, WETH_BASE, BASE_CHAIN_ID};
use dotenv::dotenv;
use ethers::types::Address;
use serde::{Deserialize, Serialize};
use std::env;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChainTransportKind {
    Auto,
    Ipc,
    Http,
    Ws,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaseNetworkConfig {
    pub transport: ChainTransportKind,
    pub ipc_path: Option<String>,
    pub rpc_url: Option<String>,
    pub ws_url: Option<String>,
}

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
    pub base_network: BaseNetworkConfig,
    pub liquidity_seeds_path: String,
    pub state_dir: String,
    pub flashblocks: FlashblocksConfig,
    pub max_route_hops: usize,
    pub max_candidate_routes: usize,
    pub min_simulation_confidence_bps: u16,
    pub max_route_gas: u64,
    pub min_net_surplus: u64,
    pub min_output_bps: u16,
    pub post_trigger_only: bool,
    pub settlement_tokens: Vec<Address>,
    pub execution_router_address: Option<Address>,
    pub executor_private_key: Option<String>,
    pub reconcile_interval_secs: u64,
    pub submission_timeout_secs: u64,
    pub log_level: String,
}

impl EngineConfig {
    pub fn from_env() -> Result<Self> {
        dotenv().ok();

        let mode = parse_mode(
            &env::var("ENGINE_MODE").unwrap_or_else(|_| "research".to_string()),
        )?;
        let base_transport = parse_chain_transport(
            &env::var("BASE_TRANSPORT").unwrap_or_else(|_| "ipc".to_string()),
        )?;
        let base_network = BaseNetworkConfig {
            transport: base_transport,
            ipc_path: env::var("BASE_IPC_PATH").ok().or_else(|| {
                if base_transport == ChainTransportKind::Ipc {
                    Some("/var/lib/reth/reth.ipc".to_string())
                } else {
                    None
                }
            }),
            rpc_url: env::var("BASE_RPC_URL").ok(),
            ws_url: env::var("BASE_WS_URL").ok(),
        };

        let config = Self {
            mode,
            base_chain_id: get_u64("BASE_CHAIN_ID", BASE_CHAIN_ID),
            base_network,
            liquidity_seeds_path: env::var("LIQUIDITY_SEEDS_PATH")
                .unwrap_or_else(|_| "config/base-liquidity-seeds.example.json".to_string()),
            state_dir: env::var("STATE_DIR").unwrap_or_else(|_| "state".to_string()),
            flashblocks: FlashblocksConfig {
                ws_url: env::var("FLASHBLOCKS_WS_URL")
                    .unwrap_or_else(|_| "wss://mainnet-preconf.base.org".to_string()),
                subscribe_transactions: get_bool("FLASHBLOCKS_SUBSCRIBE_TRANSACTIONS", true),
                subscribe_pending_logs: get_bool("FLASHBLOCKS_SUBSCRIBE_PENDING_LOGS", true),
            },
            max_route_hops: get_usize("MAX_ROUTE_HOPS", 3),
            max_candidate_routes: get_usize("MAX_CANDIDATE_ROUTES", 16),
            min_simulation_confidence_bps: get_u16("MIN_SIMULATION_CONFIDENCE_BPS", 8_000),
            max_route_gas: get_u64("MAX_ROUTE_GAS", 500_000),
            min_net_surplus: get_u64("MIN_NET_SURPLUS", 1),
            min_output_bps: get_u16("MIN_OUTPUT_BPS", 9_500),
            post_trigger_only: get_bool("POST_TRIGGER_ONLY", true),
            settlement_tokens: env::var("SETTLEMENT_TOKENS")
                .ok()
                .map(|value| parse_address_list("SETTLEMENT_TOKENS", &value))
                .transpose()?
                .unwrap_or_else(|| vec![*USDC_BASE, *WETH_BASE]),
            execution_router_address: env::var("EXECUTION_ROUTER_ADDRESS")
                .ok()
                .map(|value| parse_address("EXECUTION_ROUTER_ADDRESS", &value))
                .transpose()?,
            executor_private_key: env::var("EXECUTOR_PRIVATE_KEY").ok(),
            reconcile_interval_secs: get_u64("RECONCILE_INTERVAL_SECS", 5),
            submission_timeout_secs: get_u64("SUBMISSION_TIMEOUT_SECS", 60),
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

        self.base_network.validate()?;

        if self.max_route_hops == 0 {
            return Err(anyhow!("MAX_ROUTE_HOPS must be greater than zero"));
        }

        if self.max_candidate_routes == 0 {
            return Err(anyhow!(
                "MAX_CANDIDATE_ROUTES must be greater than zero"
            ));
        }

        if self.min_simulation_confidence_bps > 10_000 {
            return Err(anyhow!(
                "MIN_SIMULATION_CONFIDENCE_BPS must be between 0 and 10000"
            ));
        }

        if self.max_route_gas == 0 {
            return Err(anyhow!("MAX_ROUTE_GAS must be greater than zero"));
        }

        if self.min_net_surplus == 0 {
            return Err(anyhow!("MIN_NET_SURPLUS must be greater than zero"));
        }

        if self.min_output_bps == 0 || self.min_output_bps > 10_000 {
            return Err(anyhow!("MIN_OUTPUT_BPS must be between 1 and 10000"));
        }

        if self.settlement_tokens.is_empty() {
            return Err(anyhow!("SETTLEMENT_TOKENS must contain at least one address"));
        }

        if self.reconcile_interval_secs == 0 {
            return Err(anyhow!("RECONCILE_INTERVAL_SECS must be greater than zero"));
        }

        if self.submission_timeout_secs == 0 {
            return Err(anyhow!("SUBMISSION_TIMEOUT_SECS must be greater than zero"));
        }

        if matches!(self.mode, Mode::Live)
            && self
                .executor_private_key
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            return Err(anyhow!(
                "EXECUTOR_PRIVATE_KEY must be set when ENGINE_MODE=live"
            ));
        }

        if matches!(self.mode, Mode::Shadow | Mode::Live)
            && self.execution_router_address.is_none()
        {
            return Err(anyhow!(
                "EXECUTION_ROUTER_ADDRESS must be set when ENGINE_MODE is shadow or live"
            ));
        }

        if self.liquidity_seeds_path.trim().is_empty() {
            return Err(anyhow!("LIQUIDITY_SEEDS_PATH must not be empty"));
        }

        if self.state_dir.trim().is_empty() {
            return Err(anyhow!("STATE_DIR must not be empty"));
        }

        Ok(())
    }
}

impl BaseNetworkConfig {
    pub fn validate(&self) -> Result<()> {
        match self.transport {
            ChainTransportKind::Ipc => validate_present("BASE_IPC_PATH", self.ipc_path.as_deref()),
            ChainTransportKind::Http => validate_present("BASE_RPC_URL", self.rpc_url.as_deref()),
            ChainTransportKind::Ws => validate_present("BASE_WS_URL", self.ws_url.as_deref()),
            ChainTransportKind::Auto => {
                if self
                    .ipc_path
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .is_none()
                    && self
                        .rpc_url
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .is_none()
                    && self
                        .ws_url
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .is_none()
                {
                    Err(anyhow!(
                        "BASE_TRANSPORT=auto requires at least one of BASE_IPC_PATH, BASE_RPC_URL, or BASE_WS_URL"
                    ))
                } else {
                    Ok(())
                }
            }
        }
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

fn get_u16(key: &str, default: u16) -> u16 {
    env::var(key)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
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

fn parse_chain_transport(value: &str) -> Result<ChainTransportKind> {
    match value.to_ascii_lowercase().as_str() {
        "auto" => Ok(ChainTransportKind::Auto),
        "ipc" => Ok(ChainTransportKind::Ipc),
        "http" => Ok(ChainTransportKind::Http),
        "ws" => Ok(ChainTransportKind::Ws),
        _ => Err(anyhow!("unsupported BASE_TRANSPORT: {value}")),
    }
}

fn parse_address(key: &str, value: &str) -> Result<Address> {
    value
        .parse::<Address>()
        .with_context(|| format!("failed to parse {key} as an EVM address"))
}

fn parse_address_list(key: &str, value: &str) -> Result<Vec<Address>> {
    let mut addresses = Vec::new();
    for raw in value.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        addresses.push(parse_address(key, trimmed)?);
    }

    if addresses.is_empty() {
        return Err(anyhow!("{key} must contain at least one address"));
    }

    Ok(addresses)
}

fn validate_present(key: &str, value: Option<&str>) -> Result<()> {
    if value.filter(|candidate| !candidate.trim().is_empty()).is_some() {
        Ok(())
    } else {
        Err(anyhow!("{key} must be set for the selected BASE_TRANSPORT"))
    }
}

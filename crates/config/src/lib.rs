#![forbid(unsafe_code)]

//! Runtime configuration for the engine.
//!
//! The configuration layer accepts both the new crate-scoped keys and the
//! older `BASE_*` / `FLASHBLOCKS_*` compose variables so we can migrate the
//! runtime without breaking local environments.

use serde::{Deserialize, Serialize};
use std::env;
use std::net::SocketAddr;
use thiserror::Error;
use types::ingest::Channel;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    pub ingest: IngestConfig,
    pub rpc: RpcConfig,
    pub signer: SignerConfig,
    pub decision: DecisionConfig,
    pub execution: ExecutionConfig,
    pub storage: StorageConfig,
    pub metrics: MetricsConfig,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let config = Self {
            ingest: IngestConfig::from_env()?,
            rpc: RpcConfig::from_env()?,
            signer: SignerConfig::from_env()?,
            decision: DecisionConfig::from_env()?,
            execution: ExecutionConfig::from_env()?,
            storage: StorageConfig::from_env()?,
            metrics: MetricsConfig::from_env()?,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        self.ingest.validate()?;
        self.rpc.validate()?;
        self.signer.validate(&self.execution)?;
        self.decision.validate()?;
        self.execution.validate()?;
        self.storage.validate()?;
        self.metrics.validate()?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestConfig {
    pub chain_id: u64,
    pub channel: Channel,
    pub ipc_file_path: Option<String>,
    pub ws_url: Option<String>,
    pub flashblocks_ws_url: Option<String>,
    pub subscribe_transactions: bool,
    pub subscribe_logs: bool,
    pub subscribe_blocks: bool,
    pub subscribe_flashblocks: bool,
    pub reconnect_initial_ms: u64,
    pub reconnect_max_ms: u64,
    pub heartbeat_timeout_secs: u64,
    pub dedup_cache_size: usize,
    pub event_channel_capacity: usize,
    pub runtime_channel_capacity: usize,
}

impl IngestConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let channel = parse_channel(required_var_any(&["INGEST_CHANNEL", "BASE_TRANSPORT"])?)?;
        let config = Self {
            chain_id: u64_var_any(&["INGEST_CHAIN_ID", "BASE_CHAIN_ID"], 8_453)?,
            channel,
            ipc_file_path: optional_non_empty_any(&["INGEST_IPC_FILE_PATH", "BASE_IPC_PATH"]),
            ws_url: optional_non_empty_any(&["INGEST_WS_URL", "BASE_WS_URL"]),
            flashblocks_ws_url: optional_non_empty_any(&[
                "INGEST_FLASHBLOCKS_WS_URL",
                "FLASHBLOCKS_WS_URL",
            ]),
            subscribe_transactions: bool_var_any(
                &[
                    "INGEST_SUBSCRIBE_TRANSACTIONS",
                    "FLASHBLOCKS_SUBSCRIBE_TRANSACTIONS",
                ],
                true,
            )?,
            subscribe_logs: bool_var_any(
                &[
                    "INGEST_SUBSCRIBE_LOGS",
                    "FLASHBLOCKS_SUBSCRIBE_PENDING_LOGS",
                ],
                true,
            )?,
            subscribe_blocks: bool_var_any(&["INGEST_SUBSCRIBE_BLOCKS"], true)?,
            subscribe_flashblocks: bool_var_any(&["INGEST_SUBSCRIBE_FLASHBLOCKS"], false)?,
            reconnect_initial_ms: u64_var_any(&["INGEST_RECONNECT_INITIAL_MS"], 500)?,
            reconnect_max_ms: u64_var_any(&["INGEST_RECONNECT_MAX_MS"], 10_000)?,
            heartbeat_timeout_secs: u64_var_any(&["INGEST_HEARTBEAT_TIMEOUT_SECS"], 15)?,
            dedup_cache_size: usize_var_any(&["INGEST_DEDUP_CACHE_SIZE"], 50_000)?,
            event_channel_capacity: usize_var_any(&["INGEST_EVENT_CHANNEL_CAPACITY"], 4_096)?,
            runtime_channel_capacity: usize_var_any(&["INGEST_RUNTIME_CHANNEL_CAPACITY"], 512)?,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.chain_id == 0 {
            return Err(ConfigError::Validation(
                "INGEST_CHAIN_ID / BASE_CHAIN_ID must be greater than zero",
            ));
        }

        match self.channel {
            Channel::Ipc if self.ipc_file_path.is_none() => {
                return Err(ConfigError::MissingRequired {
                    key: "INGEST_IPC_FILE_PATH",
                    context: "INGEST_CHANNEL=ipc requires an IPC file path",
                });
            }
            Channel::Ws if self.ws_url.is_none() => {
                return Err(ConfigError::MissingRequired {
                    key: "INGEST_WS_URL",
                    context: "INGEST_CHANNEL=ws requires a websocket URL",
                });
            }
            _ => {}
        }

        if !self.subscribe_transactions && !self.subscribe_logs && !self.subscribe_blocks {
            return Err(ConfigError::Validation(
                "at least one ingest subscription must be enabled",
            ));
        }

        if self.subscribe_flashblocks && self.flashblocks_ws_url.is_none() {
            return Err(ConfigError::MissingRequired {
                key: "INGEST_FLASHBLOCKS_WS_URL",
                context: "INGEST_SUBSCRIBE_FLASHBLOCKS=true requires a flashblocks websocket URL",
            });
        }

        validate_backoff(
            self.reconnect_initial_ms,
            self.reconnect_max_ms,
            "INGEST_RECONNECT_INITIAL_MS",
            "INGEST_RECONNECT_MAX_MS",
        )?;
        validate_positive_u64(
            self.heartbeat_timeout_secs,
            "INGEST_HEARTBEAT_TIMEOUT_SECS must be greater than zero",
        )?;
        validate_positive_usize(
            self.dedup_cache_size,
            "INGEST_DEDUP_CACHE_SIZE must be greater than zero",
        )?;
        validate_positive_usize(
            self.event_channel_capacity,
            "INGEST_EVENT_CHANNEL_CAPACITY must be greater than zero",
        )?;
        validate_positive_usize(
            self.runtime_channel_capacity,
            "INGEST_RUNTIME_CHANNEL_CAPACITY must be greater than zero",
        )?;

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcConfig {
    pub chain_id: u64,
    pub http_url: Option<String>,
    pub ws_url: Option<String>,
    pub ipc_file_path: Option<String>,
    pub flashblocks_ws_url: Option<String>,
    pub multicall3_address: Option<String>,
    pub retry_initial_ms: u64,
    pub retry_max_ms: u64,
    pub request_timeout_ms: u64,
}

impl RpcConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let config = Self {
            chain_id: u64_var_any(&["RPC_CHAIN_ID", "BASE_CHAIN_ID"], 8_453)?,
            http_url: optional_non_empty_any(&["RPC_HTTP_URL", "BASE_RPC_URL"]),
            ws_url: optional_non_empty_any(&["RPC_WS_URL", "BASE_WS_URL"]),
            ipc_file_path: optional_non_empty_any(&["RPC_IPC_FILE_PATH", "BASE_IPC_PATH"]),
            flashblocks_ws_url: optional_non_empty_any(&[
                "RPC_FLASHBLOCKS_WS_URL",
                "FLASHBLOCKS_WS_URL",
            ]),
            multicall3_address: optional_non_empty_any(&["RPC_MULTICALL3_ADDRESS"]),
            retry_initial_ms: u64_var_any(&["RPC_RETRY_INITIAL_MS"], 250)?,
            retry_max_ms: u64_var_any(&["RPC_RETRY_MAX_MS"], 5_000)?,
            request_timeout_ms: u64_var_any(&["RPC_REQUEST_TIMEOUT_MS"], 5_000)?,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.chain_id == 0 {
            return Err(ConfigError::Validation(
                "RPC_CHAIN_ID / BASE_CHAIN_ID must be greater than zero",
            ));
        }

        if self.http_url.is_none() && self.ws_url.is_none() && self.ipc_file_path.is_none() {
            return Err(ConfigError::Validation(
                "at least one of RPC_HTTP_URL, RPC_WS_URL, or RPC_IPC_FILE_PATH must be configured",
            ));
        }

        validate_backoff(
            self.retry_initial_ms,
            self.retry_max_ms,
            "RPC_RETRY_INITIAL_MS",
            "RPC_RETRY_MAX_MS",
        )?;
        validate_positive_u64(
            self.request_timeout_ms,
            "RPC_REQUEST_TIMEOUT_MS must be greater than zero",
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignerConfig {
    pub private_key: Option<String>,
    pub keystore_path: Option<String>,
    pub aws_kms_key_id: Option<String>,
}

impl SignerConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            private_key: optional_non_empty_any(&["SIGNER_PRIVATE_KEY", "EXECUTOR_PRIVATE_KEY"]),
            keystore_path: optional_non_empty_any(&["SIGNER_KEYSTORE_PATH"]),
            aws_kms_key_id: optional_non_empty_any(&["SIGNER_AWS_KMS_KEY_ID"]),
        })
    }

    pub fn validate(&self, execution: &ExecutionConfig) -> Result<(), ConfigError> {
        if execution.mode == ExecutionMode::Shadow {
            return Ok(());
        }

        if self.private_key.is_none()
            && self.keystore_path.is_none()
            && self.aws_kms_key_id.is_none()
        {
            return Err(ConfigError::Validation(
                "live/canary execution requires a signer source",
            ));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionConfig {
    pub max_route_hops: usize,
    pub max_candidate_routes: usize,
    pub min_net_profit_wei: String,
    pub kill_switch_path: Option<String>,
    pub settlement_token: Option<String>,
    pub scan_trade_amount_wei: Option<String>,
    pub v2_bootstrap_path: Option<String>,
    pub simulation_gas_estimate: u64,
    pub simulation_gas_cost_wei: String,
    pub simulation_l1_data_fee_wei: String,
}

impl DecisionConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let config = Self {
            max_route_hops: usize_var_any(&["DECISION_MAX_ROUTE_HOPS", "MAX_ROUTE_HOPS"], 3)?,
            max_candidate_routes: usize_var_any(
                &["DECISION_MAX_CANDIDATE_ROUTES", "MAX_CANDIDATE_ROUTES"],
                16,
            )?,
            min_net_profit_wei: string_var_any(&["DECISION_MIN_NET_PROFIT_WEI"], "0")?,
            kill_switch_path: optional_non_empty_any(&["DECISION_KILL_SWITCH_PATH"]),
            settlement_token: optional_non_empty_any(&["DECISION_SETTLEMENT_TOKEN"]),
            scan_trade_amount_wei: optional_non_empty_any(&["DECISION_SCAN_TRADE_AMOUNT_WEI"]),
            v2_bootstrap_path: optional_non_empty_any(&["DECISION_V2_BOOTSTRAP_PATH"]),
            simulation_gas_estimate: u64_var_any(&["DECISION_SIMULATION_GAS_ESTIMATE"], 180_000)?,
            simulation_gas_cost_wei: string_var_any(&["DECISION_SIMULATION_GAS_COST_WEI"], "0")?,
            simulation_l1_data_fee_wei: string_var_any(
                &["DECISION_SIMULATION_L1_DATA_FEE_WEI"],
                "0",
            )?,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_positive_usize(
            self.max_route_hops,
            "DECISION_MAX_ROUTE_HOPS / MAX_ROUTE_HOPS must be greater than zero",
        )?;
        validate_positive_usize(
            self.max_candidate_routes,
            "DECISION_MAX_CANDIDATE_ROUTES / MAX_CANDIDATE_ROUTES must be greater than zero",
        )?;
        parse_u128("DECISION_MIN_NET_PROFIT_WEI", &self.min_net_profit_wei)?;
        validate_positive_u64(
            self.simulation_gas_estimate,
            "DECISION_SIMULATION_GAS_ESTIMATE must be greater than zero",
        )?;
        parse_u128(
            "DECISION_SIMULATION_GAS_COST_WEI",
            &self.simulation_gas_cost_wei,
        )?;
        parse_u128(
            "DECISION_SIMULATION_L1_DATA_FEE_WEI",
            &self.simulation_l1_data_fee_wei,
        )?;

        match (&self.settlement_token, &self.scan_trade_amount_wei) {
            (Some(_), Some(amount)) => {
                parse_u128("DECISION_SCAN_TRADE_AMOUNT_WEI", amount)?;
            }
            (None, None) => {}
            (Some(_), None) => {
                return Err(ConfigError::MissingRequired {
                    key: "DECISION_SCAN_TRADE_AMOUNT_WEI",
                    context: "decision runtime scanning requires a trade amount when a settlement token is configured",
                });
            }
            (None, Some(_)) => {
                return Err(ConfigError::MissingRequired {
                    key: "DECISION_SETTLEMENT_TOKEN",
                    context: "decision runtime scanning requires a settlement token when a trade amount is configured",
                });
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Shadow,
    Canary,
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionConfig {
    pub mode: ExecutionMode,
    pub submit_timeout_secs: u64,
    pub receipt_timeout_secs: u64,
    pub router_address: Option<String>,
    pub canary_max_trade_wei: Option<String>,
    pub canary_max_daily_wei: Option<String>,
    pub gas_limit_multiplier_bps: u32,
    pub max_fee_per_gas_wei: Option<String>,
    pub max_priority_fee_per_gas_wei: Option<String>,
}

impl ExecutionConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let config = Self {
            mode: parse_execution_mode(string_var_any(
                &["EXECUTION_MODE", "ENGINE_MODE"],
                "shadow",
            )?)?,
            submit_timeout_secs: u64_var_any(&["EXECUTION_SUBMIT_TIMEOUT_SECS"], 30)?,
            receipt_timeout_secs: u64_var_any(&["EXECUTION_RECEIPT_TIMEOUT_SECS"], 60)?,
            router_address: optional_non_empty_any(&["EXECUTION_ROUTER_ADDRESS"]),
            canary_max_trade_wei: optional_non_empty_any(&["EXECUTION_CANARY_MAX_TRADE_WEI"]),
            canary_max_daily_wei: optional_non_empty_any(&["EXECUTION_CANARY_MAX_DAILY_WEI"]),
            gas_limit_multiplier_bps: u32_var_any(&["EXECUTION_GAS_LIMIT_MULTIPLIER_BPS"], 12_000)?,
            max_fee_per_gas_wei: optional_non_empty_any(&["EXECUTION_MAX_FEE_PER_GAS_WEI"]),
            max_priority_fee_per_gas_wei: optional_non_empty_any(&[
                "EXECUTION_MAX_PRIORITY_FEE_PER_GAS_WEI",
            ]),
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        validate_positive_u64(
            self.submit_timeout_secs,
            "EXECUTION_SUBMIT_TIMEOUT_SECS must be greater than zero",
        )?;
        validate_positive_u64(
            self.receipt_timeout_secs,
            "EXECUTION_RECEIPT_TIMEOUT_SECS must be greater than zero",
        )?;
        if self.gas_limit_multiplier_bps < 10_000 {
            return Err(ConfigError::Validation(
                "EXECUTION_GAS_LIMIT_MULTIPLIER_BPS must be at least 10000",
            ));
        }

        if self.mode != ExecutionMode::Shadow && self.router_address.is_none() {
            return Err(ConfigError::MissingRequired {
                key: "EXECUTION_ROUTER_ADDRESS",
                context: "live/canary execution requires a deployed execution router address",
            });
        }

        if let Some(max_fee) = &self.max_fee_per_gas_wei {
            parse_u128("EXECUTION_MAX_FEE_PER_GAS_WEI", max_fee)?;
        }

        if let Some(max_priority_fee) = &self.max_priority_fee_per_gas_wei {
            parse_u128("EXECUTION_MAX_PRIORITY_FEE_PER_GAS_WEI", max_priority_fee)?;
        }

        if self.mode == ExecutionMode::Canary {
            let Some(max_trade) = &self.canary_max_trade_wei else {
                return Err(ConfigError::MissingRequired {
                    key: "EXECUTION_CANARY_MAX_TRADE_WEI",
                    context: "canary mode requires a per-trade limit",
                });
            };
            let Some(max_daily) = &self.canary_max_daily_wei else {
                return Err(ConfigError::MissingRequired {
                    key: "EXECUTION_CANARY_MAX_DAILY_WEI",
                    context: "canary mode requires a daily limit",
                });
            };

            parse_u128("EXECUTION_CANARY_MAX_TRADE_WEI", max_trade)?;
            parse_u128("EXECUTION_CANARY_MAX_DAILY_WEI", max_daily)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageConfig {
    pub database_url: Option<String>,
    pub max_connections: u32,
    pub connect_timeout_secs: u64,
    pub auto_migrate: bool,
}

impl StorageConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let config = Self {
            database_url: optional_non_empty_any(&["STORAGE_DATABASE_URL", "DATABASE_URL"]),
            max_connections: u32_var_any(&["STORAGE_MAX_CONNECTIONS"], 10)?,
            connect_timeout_secs: u64_var_any(&["STORAGE_CONNECT_TIMEOUT_SECS"], 5)?,
            auto_migrate: bool_var_any(&["STORAGE_AUTO_MIGRATE"], true)?,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.database_url.is_none() {
            return Ok(());
        }

        if self.max_connections == 0 {
            return Err(ConfigError::Validation(
                "STORAGE_MAX_CONNECTIONS must be greater than zero",
            ));
        }

        validate_positive_u64(
            self.connect_timeout_secs,
            "STORAGE_CONNECT_TIMEOUT_SECS must be greater than zero",
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub log_level: String,
    pub healthz_bind_addr: SocketAddr,
    pub prometheus_bind_addr: Option<SocketAddr>,
}

impl MetricsConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let enabled = bool_var_any(&["METRICS_ENABLED"], true)?;
        let prometheus_bind_addr = optional_non_empty_any(&["METRICS_PROMETHEUS_BIND_ADDR"])
            .map(|value| parse_socket_addr("METRICS_PROMETHEUS_BIND_ADDR", &value))
            .transpose()?;

        let config = Self {
            enabled,
            log_level: string_var_any(&["LOG_LEVEL"], "info")?,
            healthz_bind_addr: socket_addr_var_any(
                &["METRICS_HEALTHZ_BIND_ADDR"],
                "127.0.0.1:9000",
            )?,
            prometheus_bind_addr,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.log_level.trim().is_empty() {
            return Err(ConfigError::Validation("LOG_LEVEL must not be empty"));
        }

        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing required configuration `{key}`: {context}")]
    MissingRequired {
        key: &'static str,
        context: &'static str,
    },
    #[error("invalid value for `{key}`: `{value}` ({context})")]
    InvalidValue {
        key: &'static str,
        value: String,
        context: &'static str,
    },
    #[error("{0}")]
    Validation(&'static str),
}

fn required_var_any(keys: &[&'static str]) -> Result<String, ConfigError> {
    keys.iter()
        .find_map(|key| optional_non_empty_any(&[*key]))
        .ok_or(ConfigError::MissingRequired {
            key: keys[0],
            context: "value is required",
        })
}

fn string_var_any(keys: &[&'static str], default: &'static str) -> Result<String, ConfigError> {
    Ok(optional_non_empty_any(keys).unwrap_or_else(|| default.to_string()))
}

fn optional_non_empty_any(keys: &[&'static str]) -> Option<String> {
    keys.iter().find_map(|key| {
        env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn bool_var_any(keys: &[&'static str], default: bool) -> Result<bool, ConfigError> {
    let Some(value) = optional_non_empty_any(keys) else {
        return Ok(default);
    };

    match value.to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            key: keys[0],
            value,
            context: "expected a boolean value",
        }),
    }
}

fn u64_var_any(keys: &[&'static str], default: u64) -> Result<u64, ConfigError> {
    let Some(value) = optional_non_empty_any(keys) else {
        return Ok(default);
    };

    value.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
        key: keys[0],
        value,
        context: "expected an unsigned integer",
    })
}

fn u32_var_any(keys: &[&'static str], default: u32) -> Result<u32, ConfigError> {
    let Some(value) = optional_non_empty_any(keys) else {
        return Ok(default);
    };

    value.parse::<u32>().map_err(|_| ConfigError::InvalidValue {
        key: keys[0],
        value,
        context: "expected an unsigned integer",
    })
}

fn usize_var_any(keys: &[&'static str], default: usize) -> Result<usize, ConfigError> {
    let Some(value) = optional_non_empty_any(keys) else {
        return Ok(default);
    };

    value
        .parse::<usize>()
        .map_err(|_| ConfigError::InvalidValue {
            key: keys[0],
            value,
            context: "expected a positive integer",
        })
}

fn socket_addr_var_any(
    keys: &[&'static str],
    default: &'static str,
) -> Result<SocketAddr, ConfigError> {
    let value = optional_non_empty_any(keys).unwrap_or_else(|| default.to_string());
    parse_socket_addr(keys[0], &value)
}

fn parse_socket_addr(key: &'static str, value: &str) -> Result<SocketAddr, ConfigError> {
    value
        .parse::<SocketAddr>()
        .map_err(|_| ConfigError::InvalidValue {
            key,
            value: value.to_string(),
            context: "expected a socket address like 127.0.0.1:9000",
        })
}

fn parse_channel(value: String) -> Result<Channel, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ipc" => Ok(Channel::Ipc),
        "ws" | "websocket" => Ok(Channel::Ws),
        _ => Err(ConfigError::InvalidValue {
            key: "INGEST_CHANNEL",
            value,
            context: "expected `ipc` or `ws`",
        }),
    }
}

fn parse_execution_mode(value: String) -> Result<ExecutionMode, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "shadow" | "research" => Ok(ExecutionMode::Shadow),
        "canary" => Ok(ExecutionMode::Canary),
        "live" => Ok(ExecutionMode::Live),
        _ => Err(ConfigError::InvalidValue {
            key: "EXECUTION_MODE",
            value,
            context: "expected `shadow`, `canary`, or `live`",
        }),
    }
}

fn parse_u128(key: &'static str, value: &str) -> Result<u128, ConfigError> {
    value
        .parse::<u128>()
        .map_err(|_| ConfigError::InvalidValue {
            key,
            value: value.to_string(),
            context: "expected an unsigned 128-bit integer encoded as a base-10 string",
        })
}

fn validate_positive_u64(value: u64, message: &'static str) -> Result<(), ConfigError> {
    if value == 0 {
        return Err(ConfigError::Validation(message));
    }

    Ok(())
}

fn validate_positive_usize(value: usize, message: &'static str) -> Result<(), ConfigError> {
    if value == 0 {
        return Err(ConfigError::Validation(message));
    }

    Ok(())
}

fn validate_backoff(
    initial: u64,
    max: u64,
    initial_key: &'static str,
    max_key: &'static str,
) -> Result<(), ConfigError> {
    if initial == 0 {
        return Err(ConfigError::Validation(match initial_key {
            "INGEST_RECONNECT_INITIAL_MS" => {
                "INGEST_RECONNECT_INITIAL_MS must be greater than zero"
            }
            _ => "initial backoff must be greater than zero",
        }));
    }

    if max < initial {
        return Err(ConfigError::Validation(match max_key {
            "INGEST_RECONNECT_MAX_MS" => {
                "INGEST_RECONNECT_MAX_MS must be greater than or equal to INGEST_RECONNECT_INITIAL_MS"
            }
            "RPC_RETRY_MAX_MS" => {
                "RPC_RETRY_MAX_MS must be greater than or equal to RPC_RETRY_INITIAL_MS"
            }
            _ => "maximum backoff must be greater than or equal to initial backoff",
        }));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_env(keys: &[&str]) {
        for key in keys {
            env::remove_var(key);
        }
    }

    #[test]
    fn supports_legacy_base_env_aliases() {
        clear_env(&[
            "INGEST_CHANNEL",
            "INGEST_IPC_FILE_PATH",
            "BASE_TRANSPORT",
            "BASE_IPC_PATH",
        ]);

        env::set_var("BASE_TRANSPORT", "ipc");
        env::set_var("BASE_IPC_PATH", "/tmp/reth.ipc");

        let config = IngestConfig::from_env().unwrap();

        assert_eq!(config.channel, Channel::Ipc);
        assert_eq!(config.ipc_file_path.as_deref(), Some("/tmp/reth.ipc"));

        clear_env(&["BASE_TRANSPORT", "BASE_IPC_PATH"]);
    }

    #[test]
    fn canary_requires_limits() {
        let error = ExecutionConfig {
            mode: ExecutionMode::Canary,
            submit_timeout_secs: 30,
            receipt_timeout_secs: 60,
            router_address: Some("0x1111111111111111111111111111111111111111".to_string()),
            canary_max_trade_wei: None,
            canary_max_daily_wei: None,
            gas_limit_multiplier_bps: 12_000,
            max_fee_per_gas_wei: None,
            max_priority_fee_per_gas_wei: None,
        }
        .validate()
        .unwrap_err();

        assert_eq!(
            error,
            ConfigError::MissingRequired {
                key: "EXECUTION_CANARY_MAX_TRADE_WEI",
                context: "canary mode requires a per-trade limit",
            }
        );
    }

    #[test]
    fn execution_gas_limit_multiplier_must_not_reduce_simulation() {
        let error = ExecutionConfig {
            mode: ExecutionMode::Live,
            submit_timeout_secs: 30,
            receipt_timeout_secs: 60,
            router_address: None,
            canary_max_trade_wei: None,
            canary_max_daily_wei: None,
            gas_limit_multiplier_bps: 9_999,
            max_fee_per_gas_wei: None,
            max_priority_fee_per_gas_wei: None,
        }
        .validate()
        .unwrap_err();

        assert_eq!(
            error,
            ConfigError::Validation("EXECUTION_GAS_LIMIT_MULTIPLIER_BPS must be at least 10000",)
        );
    }
}

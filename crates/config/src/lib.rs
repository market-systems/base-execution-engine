#![forbid(unsafe_code)]

//! Runtime configuration.
//!
//! The configuration layer reads a flat environment with crate-prefixed keys.
//! The first implemented slice covers `INGEST_*`.

use serde::{Deserialize, Serialize};
use std::env;
use thiserror::Error;
use types::ingest::Channel;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    pub ingest: IngestConfig,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        Ok(Self {
            ingest: IngestConfig::from_env()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestConfig {
    pub chain_id: u64,
    pub channel: Channel,
    pub ipc_file_path: Option<String>,
    pub ws_url: Option<String>,
    pub subscribe_transactions: bool,
    pub subscribe_logs: bool,
    pub subscribe_blocks: bool,
    pub reconnect_initial_ms: u64,
    pub reconnect_max_ms: u64,
    pub heartbeat_timeout_secs: u64,
    pub dedup_cache_size: usize,
    pub event_channel_capacity: usize,
    pub runtime_channel_capacity: usize,
}

impl IngestConfig {
    pub fn from_env() -> Result<Self, ConfigError> {
        let channel = parse_channel(required_var("INGEST_CHANNEL")?)?;
        let config = Self {
            chain_id: u64_var("INGEST_CHAIN_ID", 8_453)?,
            channel,
            ipc_file_path: optional_non_empty("INGEST_IPC_FILE_PATH"),
            ws_url: optional_non_empty("INGEST_WS_URL"),
            subscribe_transactions: bool_var("INGEST_SUBSCRIBE_TRANSACTIONS", true)?,
            subscribe_logs: bool_var("INGEST_SUBSCRIBE_LOGS", true)?,
            subscribe_blocks: bool_var("INGEST_SUBSCRIBE_BLOCKS", true)?,
            reconnect_initial_ms: u64_var("INGEST_RECONNECT_INITIAL_MS", 500)?,
            reconnect_max_ms: u64_var("INGEST_RECONNECT_MAX_MS", 10_000)?,
            heartbeat_timeout_secs: u64_var("INGEST_HEARTBEAT_TIMEOUT_SECS", 15)?,
            dedup_cache_size: usize_var("INGEST_DEDUP_CACHE_SIZE", 50_000)?,
            event_channel_capacity: usize_var("INGEST_EVENT_CHANNEL_CAPACITY", 4_096)?,
            runtime_channel_capacity: usize_var("INGEST_RUNTIME_CHANNEL_CAPACITY", 512)?,
        };

        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.chain_id == 0 {
            return Err(ConfigError::Validation(
                "INGEST_CHAIN_ID must be greater than zero",
            ));
        }

        match self.channel {
            Channel::Ipc => {
                if self.ipc_file_path.is_none() {
                    return Err(ConfigError::MissingRequired {
                        key: "INGEST_IPC_FILE_PATH",
                        context: "INGEST_CHANNEL=ipc requires an IPC file path",
                    });
                }
            }
            Channel::Ws => {
                if self.ws_url.is_none() {
                    return Err(ConfigError::MissingRequired {
                        key: "INGEST_WS_URL",
                        context: "INGEST_CHANNEL=ws requires a websocket URL",
                    });
                }
            }
        }

        if !self.subscribe_transactions && !self.subscribe_logs && !self.subscribe_blocks {
            return Err(ConfigError::Validation(
                "at least one of INGEST_SUBSCRIBE_TRANSACTIONS, INGEST_SUBSCRIBE_LOGS, or INGEST_SUBSCRIBE_BLOCKS must be enabled",
            ));
        }

        if self.reconnect_initial_ms == 0 {
            return Err(ConfigError::Validation(
                "INGEST_RECONNECT_INITIAL_MS must be greater than zero",
            ));
        }

        if self.reconnect_max_ms < self.reconnect_initial_ms {
            return Err(ConfigError::Validation(
                "INGEST_RECONNECT_MAX_MS must be greater than or equal to INGEST_RECONNECT_INITIAL_MS",
            ));
        }

        if self.heartbeat_timeout_secs == 0 {
            return Err(ConfigError::Validation(
                "INGEST_HEARTBEAT_TIMEOUT_SECS must be greater than zero",
            ));
        }

        if self.dedup_cache_size == 0 {
            return Err(ConfigError::Validation(
                "INGEST_DEDUP_CACHE_SIZE must be greater than zero",
            ));
        }

        if self.event_channel_capacity == 0 {
            return Err(ConfigError::Validation(
                "INGEST_EVENT_CHANNEL_CAPACITY must be greater than zero",
            ));
        }

        if self.runtime_channel_capacity == 0 {
            return Err(ConfigError::Validation(
                "INGEST_RUNTIME_CHANNEL_CAPACITY must be greater than zero",
            ));
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

fn required_var(key: &'static str) -> Result<String, ConfigError> {
    match env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ConfigError::MissingRequired {
            key,
            context: "value is required",
        }),
    }
}

fn optional_non_empty(key: &'static str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn bool_var(key: &'static str, default: bool) -> Result<bool, ConfigError> {
    let Some(value) = env::var(key).ok() else {
        return Ok(default);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::InvalidValue {
            key,
            value,
            context: "expected a boolean value",
        }),
    }
}

fn u64_var(key: &'static str, default: u64) -> Result<u64, ConfigError> {
    let Some(value) = env::var(key).ok() else {
        return Ok(default);
    };

    value.parse::<u64>().map_err(|_| ConfigError::InvalidValue {
        key,
        value,
        context: "expected an unsigned integer",
    })
}

fn usize_var(key: &'static str, default: usize) -> Result<usize, ConfigError> {
    let Some(value) = env::var(key).ok() else {
        return Ok(default);
    };

    value
        .parse::<usize>()
        .map_err(|_| ConfigError::InvalidValue {
            key,
            value,
            context: "expected a positive integer",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ipc_channel_requirements() {
        let config = IngestConfig {
            chain_id: 8_453,
            channel: Channel::Ipc,
            ipc_file_path: None,
            ws_url: Some("ws://127.0.0.1:8546".to_string()),
            subscribe_transactions: true,
            subscribe_logs: true,
            subscribe_blocks: true,
            reconnect_initial_ms: 500,
            reconnect_max_ms: 10_000,
            heartbeat_timeout_secs: 15,
            dedup_cache_size: 50_000,
            event_channel_capacity: 4_096,
            runtime_channel_capacity: 512,
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::MissingRequired {
                key: "INGEST_IPC_FILE_PATH",
                ..
            })
        ));
    }

    #[test]
    fn validates_ws_channel_requirements() {
        let config = IngestConfig {
            chain_id: 8_453,
            channel: Channel::Ws,
            ipc_file_path: Some("/tmp/reth.ipc".to_string()),
            ws_url: None,
            subscribe_transactions: true,
            subscribe_logs: true,
            subscribe_blocks: true,
            reconnect_initial_ms: 500,
            reconnect_max_ms: 10_000,
            heartbeat_timeout_secs: 15,
            dedup_cache_size: 50_000,
            event_channel_capacity: 4_096,
            runtime_channel_capacity: 512,
        };

        assert!(matches!(
            config.validate(),
            Err(ConfigError::MissingRequired {
                key: "INGEST_WS_URL",
                ..
            })
        ));
    }

    #[test]
    fn rejects_disabled_subscriptions() {
        let config = IngestConfig {
            chain_id: 8_453,
            channel: Channel::Ipc,
            ipc_file_path: Some("/tmp/reth.ipc".to_string()),
            ws_url: None,
            subscribe_transactions: false,
            subscribe_logs: false,
            subscribe_blocks: false,
            reconnect_initial_ms: 500,
            reconnect_max_ms: 10_000,
            heartbeat_timeout_secs: 15,
            dedup_cache_size: 50_000,
            event_channel_capacity: 4_096,
            runtime_channel_capacity: 512,
        };

        assert!(matches!(config.validate(), Err(ConfigError::Validation(_))));
    }
}

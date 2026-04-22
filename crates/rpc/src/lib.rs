#![forbid(unsafe_code)]

//! RPC topology and endpoint selection helpers.
//!
//! The full alloy-backed provider implementation will live here. This first
//! slice gives the rest of the workspace a single place to reason about primary
//! and fallback endpoints instead of reading environment variables ad hoc.

use config::RpcConfig;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RpcEndpoint {
    Http(String),
    Ws(String),
    Ipc(String),
    FlashblocksWs(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcTopology {
    pub chain_id: u64,
    pub primary: Option<RpcEndpoint>,
    pub fallbacks: Vec<RpcEndpoint>,
    pub multicall3_address: Option<String>,
    pub retry_initial_ms: u64,
    pub retry_max_ms: u64,
    pub request_timeout_ms: u64,
}

impl RpcTopology {
    pub fn from_config(config: &RpcConfig) -> Self {
        let mut ordered = Vec::new();

        if let Some(ipc) = &config.ipc_file_path {
            ordered.push(RpcEndpoint::Ipc(ipc.clone()));
        }

        if let Some(ws) = &config.ws_url {
            ordered.push(RpcEndpoint::Ws(ws.clone()));
        }

        if let Some(http) = &config.http_url {
            ordered.push(RpcEndpoint::Http(http.clone()));
        }

        let primary = ordered.first().cloned();
        let fallbacks = ordered.into_iter().skip(1).collect();

        Self {
            chain_id: config.chain_id,
            primary,
            fallbacks,
            multicall3_address: config.multicall3_address.clone(),
            retry_initial_ms: config.retry_initial_ms,
            retry_max_ms: config.retry_max_ms,
            request_timeout_ms: config.request_timeout_ms,
        }
    }

    pub fn flashblocks(&self, config: &RpcConfig) -> Option<RpcEndpoint> {
        config
            .flashblocks_ws_url
            .as_ref()
            .map(|url| RpcEndpoint::FlashblocksWs(url.clone()))
    }
}

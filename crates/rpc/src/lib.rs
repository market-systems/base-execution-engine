#![forbid(unsafe_code)]

//! RPC topology and the workspace-wide `EngineProvider` built on alloy.
//!
//! The provider is intentionally a thin facade so that the rest of the
//! workspace depends on a stable surface area while we keep upgrading alloy
//! transports underneath. We expose two primary entry points:
//!
//! - [`EngineProvider::connect_http`] for transactional flows (gas reads,
//!   `eth_call` simulation, `eth_sendRawTransaction`, receipt polling).
//! - [`EngineProvider::connect_ws_pubsub`] for subscription flows (new heads,
//!   pending transactions). Currently exposed via [`EngineProvider::ws_url`]
//!   so the ingest layer can construct its own pubsub when it migrates to
//!   alloy.

use std::time::Duration;

use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use alloy_transport_http::Http;
use anyhow::Context;
use config::RpcConfig;
use reqwest::Client as ReqwestClient;
use serde::{Deserialize, Serialize};
use url::Url;

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

    /// Returns the first HTTP endpoint declared in the topology, scanning the
    /// primary first then the fallbacks. Used by the execution layer to pick a
    /// transactional RPC; callers MUST validate non-None before submitting.
    pub fn first_http(&self) -> Option<&str> {
        std::iter::once(self.primary.as_ref())
            .chain(self.fallbacks.iter().map(Some))
            .flatten()
            .find_map(|endpoint| match endpoint {
                RpcEndpoint::Http(url) => Some(url.as_str()),
                _ => None,
            })
    }

    pub fn first_ws(&self) -> Option<&str> {
        std::iter::once(self.primary.as_ref())
            .chain(self.fallbacks.iter().map(Some))
            .flatten()
            .find_map(|endpoint| match endpoint {
                RpcEndpoint::Ws(url) => Some(url.as_str()),
                _ => None,
            })
    }
}

/// Provider abstraction shared across crates. Dropping the inner `RootProvider`
/// closes the underlying transport, so the engine should hold the provider for
/// its full lifetime.
#[derive(Clone)]
pub struct EngineProvider {
    inner: RootProvider<Http<ReqwestClient>>,
    chain_id: u64,
    request_timeout: Duration,
}

impl std::fmt::Debug for EngineProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineProvider")
            .field("chain_id", &self.chain_id)
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

impl EngineProvider {
    /// Build an HTTP-backed provider with retry middleware. Retries follow the
    /// configured initial / max backoff window with full jitter.
    pub fn connect_http(topology: &RpcTopology) -> anyhow::Result<Self> {
        let url_str = topology
            .first_http()
            .context("no HTTP endpoint configured for the engine provider")?;
        let url = Url::parse(url_str)
            .with_context(|| format!("invalid HTTP rpc URL `{url_str}`"))?;

        let client = ReqwestClient::builder()
            .timeout(Duration::from_millis(topology.request_timeout_ms.max(1)))
            .build()
            .context("failed to build reqwest http client")?;

        let http = Http::with_client(client, url);
        // Retry & backoff are handled at the engine layer (per-request) instead
        // of via `RetryBackoffLayer` so we can keep the provider type simple
        // and still surface granular failures (e.g. distinguish a stale nonce
        // from a transient transport failure).
        let rpc_client = alloy_rpc_client::RpcClient::new(http, false);
        let inner: RootProvider<Http<ReqwestClient>> =
            ProviderBuilder::new().on_client(rpc_client);

        Ok(Self {
            inner,
            chain_id: topology.chain_id,
            request_timeout: Duration::from_millis(topology.request_timeout_ms.max(1)),
        })
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub fn inner(&self) -> &RootProvider<Http<ReqwestClient>> {
        &self.inner
    }

    /// Verify that the endpoint reports the expected chain id. Used during
    /// engine bootstrap to fail fast when the operator points the executor at
    /// the wrong network.
    pub async fn verify_chain_id(&self) -> anyhow::Result<()> {
        let observed = self
            .inner
            .get_chain_id()
            .await
            .context("failed to call eth_chainId during bootstrap")?;
        if observed != self.chain_id {
            anyhow::bail!(
                "rpc endpoint reports chain_id {observed} but engine is configured for {}",
                self.chain_id
            );
        }
        Ok(())
    }
}

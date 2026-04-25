//! Alloy pubsub session.
//!
//! This module replaces the previous hand-rolled tungstenite + Unix-socket
//! transport with `alloy-provider`'s pubsub frontend. The trade-offs:
//!
//! - **Wire compatibility**: alloy-pubsub speaks the same JSON-RPC dialect
//!   the streams rely on, so we do not need to rewrite the downstream
//!   `RawLogMessage` / `RawTransactionMessage` / `RawBlockMessage`
//!   decoders. Each subscription item is serialised back to
//!   [`serde_json::Value`] before being handed off, which is essentially
//!   free compared to the network round-trip.
//! - **Subscription multiplexing**: alloy maintains a single backend
//!   connection per provider with channel fan-out, so we no longer need
//!   the manual "queued subscription payloads" buffer the old session
//!   carried for re-entrant `request` calls during a `subscribe` flow.
//! - **Pending transaction shape**: alloy's typed
//!   `subscribe_pending_transactions()` always returns hashes (`B256`),
//!   never the Geth-only `["newPendingTransactions", true]` full-tx form.
//!   That is a strict portability win and the existing `materialize_*`
//!   step in `transaction.rs` already knows to follow up with
//!   `eth_getTransactionByHash`, so the on-the-wire behaviour is identical
//!   for the consumers downstream.

use crate::channel::IngestEndpoint;
use crate::error::IngestError;
use crate::stream::StreamSubscription;
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use alloy_pubsub::PubSubFrontend;
use alloy_rpc_types_eth::Filter;
use alloy_transport_ipc::IpcConnect;
use alloy_transport_ws::WsConnect;
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use std::pin::Pin;

pub(crate) struct AlloyPubsubSession {
    provider: RootProvider<PubSubFrontend>,
    stream_name: &'static str,
}

impl AlloyPubsubSession {
    pub(crate) async fn connect(
        endpoint: &IngestEndpoint,
        stream_name: &'static str,
    ) -> Result<Self, IngestError> {
        let provider: RootProvider<PubSubFrontend> = match endpoint {
            IngestEndpoint::Ws { url } => {
                let connect = WsConnect::new(url.clone());
                ProviderBuilder::new().on_ws(connect).await?
            }
            IngestEndpoint::Ipc { file_path } => {
                let connect = IpcConnect::new(file_path.clone());
                ProviderBuilder::new().on_ipc(connect).await?
            }
        };
        Ok(Self {
            provider,
            stream_name,
        })
    }

    /// Open a subscription. Returns a [`SubscriptionHandle`] that exposes a
    /// JSON-shaped stream of payloads aligned with what the legacy
    /// `JsonRpcSession::next_subscription_payload` returned, so the per-stream
    /// decoders need no changes.
    pub(crate) async fn subscribe(
        &self,
        subscription: StreamSubscription,
    ) -> Result<SubscriptionHandle, IngestError> {
        match subscription {
            StreamSubscription::Logs => {
                let sub = self.provider.subscribe_logs(&Filter::new()).await?;
                let id = format!("{:#x}", sub.local_id());
                let stream = sub.into_stream().map(|log| {
                    serde_json::to_value(log).map_err(IngestError::from)
                });
                Ok(SubscriptionHandle {
                    id,
                    stream: Box::pin(stream),
                })
            }
            StreamSubscription::Blocks => {
                let sub = self.provider.subscribe_blocks().await?;
                let id = format!("{:#x}", sub.local_id());
                let stream = sub.into_stream().map(|header| {
                    serde_json::to_value(header).map_err(IngestError::from)
                });
                Ok(SubscriptionHandle {
                    id,
                    stream: Box::pin(stream),
                })
            }
            StreamSubscription::Transactions => {
                let sub = self.provider.subscribe_pending_transactions().await?;
                let id = format!("{:#x}", sub.local_id());
                let stream = sub.into_stream().map(|hash| {
                    // Hashes serialise to `"0x..."` strings, which the
                    // transaction stream's `materialize_transaction` step
                    // then expands via `eth_getTransactionByHash`.
                    serde_json::to_value(hash).map_err(IngestError::from)
                });
                Ok(SubscriptionHandle {
                    id,
                    stream: Box::pin(stream),
                })
            }
        }
    }

    /// Generic JSON-RPC call. Mirrors the legacy session's `request` method
    /// so the per-stream code that materialises pending transactions does
    /// not need to know which transport is underneath.
    pub(crate) async fn request(
        &self,
        method: &'static str,
        params: Value,
    ) -> Result<Value, IngestError> {
        // Deserialise the params into a `Vec<Value>` shaped tuple via
        // alloy_json_rpc by converting through its dynamic param type.
        let result: Value = self
            .provider
            .client()
            .request(method, params)
            .await
            .map_err(|e| IngestError::JsonRpc {
                stream_name: self.stream_name,
                message: e.to_string(),
            })?;
        Ok(result)
    }
}

/// A live subscription. Drops the underlying alloy `Subscription` when
/// dropped; the alloy frontend then sends the matching `eth_unsubscribe`
/// best-effort. Treat this handle as `!Sync` even though the trait bounds
/// would allow it: the inner stream is `!Send` across `&mut` boundaries.
pub(crate) struct SubscriptionHandle {
    id: String,
    stream: Pin<Box<dyn Stream<Item = Result<Value, IngestError>> + Send>>,
}

impl SubscriptionHandle {
    /// Subscription id reported by the upstream node. Used purely for
    /// observability/log lines today; the alloy frontend tracks the
    /// subscription internally so we do not need to pass the id back when
    /// pulling new payloads.
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    /// Pull the next payload. Returns `Err(StreamFailure)` when the upstream
    /// stream has closed, which the per-stream layer translates into a
    /// reconnect via its standard backoff loop.
    pub(crate) async fn next_payload(
        &mut self,
        stream_name: &'static str,
    ) -> Result<Value, IngestError> {
        match self.stream.next().await {
            Some(Ok(value)) => Ok(value),
            Some(Err(err)) => Err(err),
            None => Err(IngestError::StreamFailure {
                stream_name,
                message: "alloy subscription stream closed".to_string(),
            }),
        }
    }
}

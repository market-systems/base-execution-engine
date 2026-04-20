use super::StreamSubscription;
use crate::channel::IngestEndpoint;
use crate::error::IngestError;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

pub(crate) struct JsonRpcSession {
    connection: JsonRpcConnection,
    next_request_id: u64,
    queued_subscription_payloads: VecDeque<Value>,
    stream_name: &'static str,
}

impl JsonRpcSession {
    pub(crate) async fn connect(
        endpoint: &IngestEndpoint,
        stream_name: &'static str,
    ) -> Result<Self, IngestError> {
        Ok(Self {
            connection: JsonRpcConnection::open(endpoint).await?,
            next_request_id: 1,
            queued_subscription_payloads: VecDeque::new(),
            stream_name,
        })
    }

    pub(crate) async fn subscribe(
        &mut self,
        subscription: StreamSubscription,
    ) -> Result<String, IngestError> {
        let params = match subscription {
            StreamSubscription::Transactions => json!(["newPendingTransactions", true]),
            StreamSubscription::Logs => json!(["logs", {}]),
            StreamSubscription::Blocks => json!(["newHeads"]),
        };

        let response = self.request("eth_subscribe", params).await?;
        response
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| IngestError::JsonRpc {
                stream_name: self.stream_name,
                message: "subscription response did not return a subscription id".to_string(),
            })
    }

    pub(crate) async fn request(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Value, IngestError> {
        let id = self.next_request_id;
        self.next_request_id += 1;

        self.connection
            .send_json(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .await?;

        loop {
            let message = self.connection.next_json(self.stream_name).await?;

            if let Some(payload) = try_extract_subscription_payload(&message, None)? {
                self.queued_subscription_payloads.push_back(payload);
                continue;
            }

            let Some(message_id) = message.get("id").and_then(Value::as_u64) else {
                continue;
            };

            if message_id == id {
                return extract_result(self.stream_name, &message);
            }
        }
    }

    pub(crate) async fn next_subscription_payload(
        &mut self,
        subscription_id: &str,
    ) -> Result<Value, IngestError> {
        if let Some(payload) = self.queued_subscription_payloads.pop_front() {
            return Ok(payload);
        }

        loop {
            let message = self.connection.next_json(self.stream_name).await?;

            if let Some(payload) =
                try_extract_subscription_payload(&message, Some(subscription_id))?
            {
                return Ok(payload);
            }
        }
    }
}

enum JsonRpcConnection {
    Ws(WebSocketStream<MaybeTlsStream<TcpStream>>),
    Ipc(IpcConnection),
}

impl JsonRpcConnection {
    async fn open(endpoint: &IngestEndpoint) -> Result<Self, IngestError> {
        match endpoint {
            IngestEndpoint::Ws { url } => {
                let (stream, _) = connect_async(url).await?;
                Ok(Self::Ws(stream))
            }
            IngestEndpoint::Ipc { file_path } => {
                let stream = UnixStream::connect(file_path).await?;
                Ok(Self::Ipc(IpcConnection {
                    stream,
                    read_buffer: Vec::with_capacity(4096),
                }))
            }
        }
    }

    async fn send_json(&mut self, value: &Value) -> Result<(), IngestError> {
        match self {
            Self::Ws(stream) => {
                stream.send(Message::Text(value.to_string())).await?;
                Ok(())
            }
            Self::Ipc(connection) => connection.send_json(value).await,
        }
    }

    async fn next_json(&mut self, stream_name: &'static str) -> Result<Value, IngestError> {
        match self {
            Self::Ws(stream) => loop {
                let Some(message) = stream.next().await else {
                    return Err(IngestError::StreamFailure {
                        stream_name,
                        message: "websocket connection closed".to_string(),
                    });
                };

                match message? {
                    Message::Text(text) => return Ok(serde_json::from_str(&text)?),
                    Message::Binary(bytes) => return Ok(serde_json::from_slice(&bytes)?),
                    Message::Ping(payload) => stream.send(Message::Pong(payload)).await?,
                    Message::Pong(_) | Message::Frame(_) => continue,
                    Message::Close(frame) => {
                        return Err(IngestError::StreamFailure {
                            stream_name,
                            message: format!("websocket closed: {frame:?}"),
                        });
                    }
                }
            },
            Self::Ipc(connection) => connection.next_json(stream_name).await,
        }
    }
}

struct IpcConnection {
    stream: UnixStream,
    read_buffer: Vec<u8>,
}

impl IpcConnection {
    async fn send_json(&mut self, value: &Value) -> Result<(), IngestError> {
        self.stream.write_all(value.to_string().as_bytes()).await?;
        Ok(())
    }

    async fn next_json(&mut self, stream_name: &'static str) -> Result<Value, IngestError> {
        loop {
            if let Some((value, consumed)) = try_parse_json(&self.read_buffer)? {
                self.read_buffer.drain(..consumed);
                return Ok(value);
            }

            let mut chunk = vec![0_u8; 4096];
            let read = self.stream.read(&mut chunk).await?;

            if read == 0 {
                return Err(IngestError::StreamFailure {
                    stream_name,
                    message: "ipc connection closed".to_string(),
                });
            }

            self.read_buffer.extend_from_slice(&chunk[..read]);
        }
    }
}

fn try_parse_json(buffer: &[u8]) -> Result<Option<(Value, usize)>, IngestError> {
    let mut stream = serde_json::Deserializer::from_slice(buffer).into_iter::<Value>();

    match stream.next() {
        Some(Ok(value)) => Ok(Some((value, stream.byte_offset()))),
        Some(Err(error)) if error.is_eof() => Ok(None),
        Some(Err(error)) => Err(error.into()),
        None => Ok(None),
    }
}

fn extract_result(stream_name: &'static str, message: &Value) -> Result<Value, IngestError> {
    if let Some(error) = message.get("error") {
        return Err(IngestError::JsonRpc {
            stream_name,
            message: error.to_string(),
        });
    }

    message
        .get("result")
        .cloned()
        .ok_or_else(|| IngestError::JsonRpc {
            stream_name,
            message: "missing `result` field in json-rpc response".to_string(),
        })
}

fn try_extract_subscription_payload(
    message: &Value,
    subscription_id: Option<&str>,
) -> Result<Option<Value>, IngestError> {
    if message.get("method").and_then(Value::as_str) != Some("eth_subscription") {
        return Ok(None);
    }

    let params = message
        .get("params")
        .and_then(Value::as_object)
        .ok_or_else(|| IngestError::JsonRpc {
            stream_name: "json_rpc_session",
            message: "subscription notification missing params object".to_string(),
        })?;

    let message_subscription = params
        .get("subscription")
        .and_then(Value::as_str)
        .ok_or_else(|| IngestError::JsonRpc {
            stream_name: "json_rpc_session",
            message: "subscription notification missing subscription id".to_string(),
        })?;

    if let Some(expected) = subscription_id {
        if message_subscription != expected {
            return Ok(None);
        }
    }

    Ok(Some(params.get("result").cloned().unwrap_or(Value::Null)))
}

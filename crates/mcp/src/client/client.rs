//! JSON-RPC 2.0 client built on top of a `Transport`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::time::timeout;
use tracing::{debug, warn};

use crate::protocol::{
    IncomingMessage, JsonRpcError, JsonRpcResponse, RequestId, encode_notification, encode_request,
};
use crate::transport::{Transport, TransportError};

use super::pending::PendingRequests;

/// Default timeout applied to a single `request`.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Failures surfaced by `McpClient`.
#[derive(Debug, Error)]
pub enum McpClientError {
    /// Underlying transport error.
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    /// Failed to serialize outbound request parameters.
    #[error("serialize request: {0}")]
    Serialize(serde_json::Error),
    /// Failed to deserialize inbound response payload into the expected shape.
    #[error("deserialize response: {0}")]
    Deserialize(serde_json::Error),
    /// The peer returned a JSON-RPC error object instead of a result.
    #[error("peer returned rpc error: code={code} message={message}")]
    Rpc {
        /// The JSON-RPC error code.
        code: i64,
        /// The human-readable message.
        message: String,
    },
    /// The request timed out before a response arrived.
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    /// The client has been shut down.
    #[error("client is shut down")]
    Shutdown,
    /// The response frame was missing both `result` and `error`.
    #[error("response had neither result nor error")]
    EmptyResponse,
}

/// Correlates outbound JSON-RPC requests with inbound responses over a
/// `Transport`.
pub struct McpClient {
    transport: Arc<dyn Transport>,
    pending: Arc<PendingRequests>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Builds a new client over the given transport and spawns the inbound
    /// dispatcher task.
    pub fn new(transport: Arc<dyn Transport>) -> Result<Self, McpClientError> {
        let inbound = transport.take_inbound()?;
        let pending = Arc::new(PendingRequests::default());
        spawn_dispatcher(inbound, Arc::clone(&pending));
        Ok(Self {
            transport,
            pending,
            next_id: AtomicU64::new(1),
        })
    }

    /// Issues one request and awaits the response with the default timeout.
    pub async fn request<P, R>(&self, method: &str, params: &P) -> Result<R, McpClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        self.request_with_timeout(method, params, DEFAULT_REQUEST_TIMEOUT)
            .await
    }

    /// Issues one request with an explicit timeout.
    pub async fn request_with_timeout<P, R>(
        &self,
        method: &str,
        params: &P,
        request_timeout: Duration,
    ) -> Result<R, McpClientError>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = RequestId::from_u64(self.next_id.fetch_add(1, Ordering::Relaxed));
        let frame = encode_request(&id, method, Some(params)).map_err(McpClientError::Serialize)?;
        let rx = self.pending.register(id.clone());
        self.transport.send(frame).await?;

        match timeout(request_timeout, rx).await {
            Ok(Ok(response)) => interpret_response(response),
            Ok(Err(_)) => {
                self.pending.drop_entry(&id);
                Err(McpClientError::Shutdown)
            }
            Err(_) => {
                self.pending.drop_entry(&id);
                Err(McpClientError::Timeout(request_timeout))
            }
        }
    }

    /// Sends a notification (no id, no reply expected).
    pub async fn notify<P: Serialize>(
        &self,
        method: &str,
        params: &P,
    ) -> Result<(), McpClientError> {
        let frame = encode_notification(method, Some(params)).map_err(McpClientError::Serialize)?;
        self.transport.send(frame).await?;
        Ok(())
    }

    /// Sends a notification with no params.
    pub async fn notify_bare(&self, method: &str) -> Result<(), McpClientError> {
        let frame = encode_notification::<()>(method, None).map_err(McpClientError::Serialize)?;
        self.transport.send(frame).await?;
        Ok(())
    }

    /// Shuts down the transport and drops any in-flight requests.
    pub async fn shutdown(&self) -> Result<(), McpClientError> {
        self.pending.clear();
        self.transport.shutdown().await?;
        Ok(())
    }

    /// Number of requests currently awaiting a response. Primarily exposed for
    /// tests; production code should not depend on this.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

fn interpret_response<R: DeserializeOwned>(response: JsonRpcResponse) -> Result<R, McpClientError> {
    if let Some(err) = response.error {
        let JsonRpcError { code, message, .. } = err;
        return Err(McpClientError::Rpc { code, message });
    }
    let result = response.result.ok_or(McpClientError::EmptyResponse)?;
    serde_json::from_value(result).map_err(McpClientError::Deserialize)
}

fn spawn_dispatcher(
    mut inbound: tokio::sync::mpsc::UnboundedReceiver<IncomingMessage>,
    pending: Arc<PendingRequests>,
) {
    tokio::spawn(async move {
        while let Some(msg) = inbound.recv().await {
            match msg {
                IncomingMessage::Response(resp) => pending.resolve(resp),
                IncomingMessage::Notification(n) => {
                    debug!(method = %n.method, "mcp client: received notification (unhandled in v1)");
                }
                IncomingMessage::Request(req) => {
                    warn!(
                        method = %req.method,
                        "mcp client: server-initiated request ignored in v1",
                    );
                }
            }
        }
    });
}

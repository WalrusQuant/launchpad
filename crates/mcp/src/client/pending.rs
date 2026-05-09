//! Correlation map for in-flight JSON-RPC requests.

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

use crate::protocol::{JsonRpcResponse, RequestId};

/// Correlates `RequestId`s with the oneshot channel awaiting the response.
#[derive(Default)]
pub struct PendingRequests {
    inner: Mutex<HashMap<RequestId, oneshot::Sender<JsonRpcResponse>>>,
}

impl PendingRequests {
    /// Registers a pending request. The returned receiver resolves when the
    /// matching response arrives.
    pub fn register(&self, id: RequestId) -> oneshot::Receiver<JsonRpcResponse> {
        let (tx, rx) = oneshot::channel();
        if let Ok(mut map) = self.inner.lock() {
            map.insert(id, tx);
        }
        rx
    }

    /// Drops a pending entry (used on timeout / cancel cleanup).
    pub fn drop_entry(&self, id: &RequestId) {
        if let Ok(mut map) = self.inner.lock() {
            map.remove(id);
        }
    }

    /// Resolves a pending request. If the id is unknown, the response is
    /// silently dropped — the spec allows out-of-order / unexpected responses.
    pub fn resolve(&self, response: JsonRpcResponse) {
        let sender = match self.inner.lock() {
            Ok(mut map) => map.remove(&response.id),
            Err(_) => None,
        };
        if let Some(tx) = sender {
            let _ = tx.send(response);
        }
    }

    /// Clears all pending entries, typically on transport shutdown.
    pub fn clear(&self) {
        if let Ok(mut map) = self.inner.lock() {
            map.clear();
        }
    }

    /// Returns how many requests are currently in flight.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Returns true when no requests are in flight.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

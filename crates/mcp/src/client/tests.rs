//! Unit tests for `McpClient` using a channel-only fake transport.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pretty_assertions::assert_eq;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::protocol::{
    IncomingMessage, JsonRpcResponse, RequestId, encode_response_success, parse_message,
};
use crate::transport::{Transport, TransportError};

use super::client::{McpClient, McpClientError};

/// In-memory transport: captured outbound frames + scripted inbound feed.
struct FakeTransport {
    outbound: Mutex<Vec<Vec<u8>>>,
    inbound_tx: mpsc::UnboundedSender<IncomingMessage>,
    inbound_rx: Mutex<Option<mpsc::UnboundedReceiver<IncomingMessage>>>,
}

impl FakeTransport {
    fn new() -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            outbound: Mutex::new(Vec::new()),
            inbound_tx: tx,
            inbound_rx: Mutex::new(Some(rx)),
        })
    }

    fn last_outbound(&self) -> Option<Vec<u8>> {
        self.outbound.lock().unwrap().last().cloned()
    }

    fn push_inbound(&self, msg: IncomingMessage) {
        let _ = self.inbound_tx.send(msg);
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn send(&self, frame: Vec<u8>) -> Result<(), TransportError> {
        self.outbound.lock().unwrap().push(frame);
        Ok(())
    }

    fn take_inbound(&self) -> Result<mpsc::UnboundedReceiver<IncomingMessage>, TransportError> {
        self.inbound_rx
            .lock()
            .unwrap()
            .take()
            .ok_or(TransportError::InboundAlreadyTaken)
    }

    async fn shutdown(&self) -> Result<(), TransportError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Ok200 {
    ok: bool,
}

fn extract_request_id(frame: &[u8]) -> RequestId {
    let value: Value = serde_json::from_slice(frame).expect("parse outbound");
    let id = value.get("id").expect("id");
    serde_json::from_value(id.clone()).expect("id shape")
}

#[tokio::test]
async fn request_response_round_trip() {
    let transport = FakeTransport::new();
    let client = McpClient::new(transport.clone() as Arc<dyn Transport>).expect("client");

    let params = json!({"hello": "world"});
    let client_clone = Arc::new(client);
    let client_for_task = Arc::clone(&client_clone);
    let request_task =
        tokio::spawn(async move { client_for_task.request::<_, Ok200>("ping", &params).await });

    // Wait for the outbound frame to land so we can echo a response for it.
    let frame = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(f) = transport.last_outbound() {
                return f;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("outbound frame");

    let id = extract_request_id(&frame);
    let response_bytes =
        encode_response_success(&id, json!({"ok": true})).expect("encode response");
    let msg = parse_message(&response_bytes).expect("parse response");
    transport.push_inbound(msg);

    let result = request_task.await.expect("join").expect("result");
    assert_eq!(result, Ok200 { ok: true });
    assert_eq!(client_clone.pending_len(), 0);
}

#[tokio::test]
async fn request_times_out_when_no_response() {
    let transport = FakeTransport::new();
    let client = McpClient::new(transport.clone() as Arc<dyn Transport>).expect("client");

    let err = client
        .request_with_timeout::<_, Ok200>("ping", &json!({}), Duration::from_millis(50))
        .await
        .expect_err("timeout");
    match err {
        McpClientError::Timeout(d) => assert_eq!(d, Duration::from_millis(50)),
        other => panic!("expected timeout, got {other:?}"),
    }
    assert_eq!(client.pending_len(), 0);
}

#[tokio::test]
async fn notification_does_not_register_pending_entry() {
    let transport = FakeTransport::new();
    let client = McpClient::new(transport.clone() as Arc<dyn Transport>).expect("client");
    client
        .notify("notifications/initialized", &json!({}))
        .await
        .expect("notify");
    assert_eq!(client.pending_len(), 0);
}

#[tokio::test]
async fn response_to_unknown_id_is_dropped() {
    let transport = FakeTransport::new();
    let client = McpClient::new(transport.clone() as Arc<dyn Transport>).expect("client");

    // Inject a response whose id does not match any pending entry.
    let stray = JsonRpcResponse {
        jsonrpc: "2.0".into(),
        id: RequestId::Number(99_999),
        result: Some(json!({"ok": true})),
        error: None,
    };
    transport.push_inbound(IncomingMessage::Response(stray));

    // Now a real request should still be able to time out normally.
    let err = client
        .request_with_timeout::<_, Ok200>("ping", &json!({}), Duration::from_millis(50))
        .await
        .expect_err("timeout");
    matches!(err, McpClientError::Timeout(_));
}

#[tokio::test]
async fn multiple_concurrent_requests_resolve_independently() {
    let transport = FakeTransport::new();
    let client = Arc::new(McpClient::new(transport.clone() as Arc<dyn Transport>).expect("client"));

    let c1 = Arc::clone(&client);
    let c2 = Arc::clone(&client);
    let t1 = tokio::spawn(async move { c1.request::<_, Ok200>("a", &json!({})).await });
    let t2 = tokio::spawn(async move { c2.request::<_, Ok200>("b", &json!({})).await });

    // Collect both outbound frames.
    let frames = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let snapshot = transport.outbound.lock().unwrap().clone();
            if snapshot.len() >= 2 {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("two outbound frames");

    // Reply in reverse order to confirm matching is id-based.
    let id1 = extract_request_id(&frames[0]);
    let id2 = extract_request_id(&frames[1]);
    let r2 = encode_response_success(&id2, json!({"ok": true})).unwrap();
    let r1 = encode_response_success(&id1, json!({"ok": true})).unwrap();
    transport.push_inbound(parse_message(&r2).unwrap());
    transport.push_inbound(parse_message(&r1).unwrap());

    let a = t1.await.unwrap().unwrap();
    let b = t2.await.unwrap().unwrap();
    assert_eq!(a, Ok200 { ok: true });
    assert_eq!(b, Ok200 { ok: true });
}

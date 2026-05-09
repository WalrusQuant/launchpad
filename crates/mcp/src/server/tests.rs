//! Integration-ish tests for `ServerSupervisor` that exercise the full
//! supervisor → client → transport path against a real stdio echo server.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::server::ServerSupervisor;
use crate::{McpServerId, McpServerRecord, McpStartupPolicy, McpStartupState, McpTransportConfig};

/// Echo script that implements a minimal MCP handshake + `tools/list` + `tools/call`.
const HANDSHAKE_SCRIPT: &str = r#"
import json, sys

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if "id" not in msg:
        continue
    method = msg.get("method", "")
    if method == "initialize":
        result = {"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"echo","version":"0.1"}}
    elif method == "tools/list":
        result = {"tools":[{"name":"ping","description":"Ping back","inputSchema":{"type":"object"}}]}
    elif method == "tools/call":
        args = msg.get("params", {}).get("arguments", {})
        result = {"content":[{"type":"text","text":"pong:" + json.dumps(args)}],"isError": False}
    else:
        result = {"ok": True}
    response = {"jsonrpc":"2.0","id":msg["id"],"result":result}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#;

/// Fails-on-initialize script: returns a JSON-RPC error instead of a result.
const BAD_INIT_SCRIPT: &str = r#"
import json, sys

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if "id" not in msg:
        continue
    response = {"jsonrpc":"2.0","id":msg["id"],"error":{"code":-32000,"message":"init denied"}}
    sys.stdout.write(json.dumps(response) + "\n")
    sys.stdout.flush()
"#;

fn python_bin() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|candidate| {
        std::process::Command::new(candidate)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

fn record(id: &str, script: &str) -> Option<McpServerRecord> {
    let python = python_bin()?;
    Some(McpServerRecord {
        id: McpServerId(id.into()),
        display_name: id.to_owned(),
        transport: McpTransportConfig::Stdio {
            command: vec![python.into(), "-c".into(), script.to_owned()],
            cwd: Option::<PathBuf>::None,
            env: BTreeMap::new(),
        },
        startup_policy: McpStartupPolicy::Lazy,
        enabled: true,
        trust_level: crate::TrustLevel::default(),
    })
}

/// Waits for a state to match `want` within `total` or returns the last seen state.
async fn await_state(
    handle: &crate::server::ServerHandle,
    want: McpStartupState,
    total: Duration,
) -> McpStartupState {
    let start = std::time::Instant::now();
    loop {
        let current = handle.startup_state();
        if current == want || start.elapsed() >= total {
            return current;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn initialize_then_list_tools_populates_catalog() {
    let Some(rec) = record("echo", HANDSHAKE_SCRIPT) else {
        eprintln!("skipping: python not found");
        return;
    };
    let handle = ServerSupervisor::spawn(rec);

    // First refresh triggers the startup lifecycle.
    let catalog = handle.refresh().await.expect("refresh");
    assert_eq!(catalog.tools.len(), 1);
    assert_eq!(catalog.tools[0].name, "ping");
    assert_eq!(catalog.tools[0].description, "Ping back");

    let state = await_state(&handle, McpStartupState::Ready, Duration::from_secs(2)).await;
    assert_eq!(state, McpStartupState::Ready);

    handle.shutdown();
}

#[tokio::test]
async fn invoke_tool_routes_through_client() {
    let Some(rec) = record("echo", HANDSHAKE_SCRIPT) else {
        return;
    };
    let handle = ServerSupervisor::spawn(rec);

    let result = handle
        .invoke_tool("ping", json!({"x": 1}))
        .await
        .expect("invoke");
    assert!(!result.is_error);
    match &result.content[0] {
        crate::protocol::ContentBlock::Text { text } => assert!(text.starts_with("pong:")),
        other => panic!("expected text, got {other:?}"),
    }
    handle.shutdown();
}

#[tokio::test]
async fn shutdown_is_idempotent() {
    let Some(rec) = record("echo", HANDSHAKE_SCRIPT) else {
        return;
    };
    let handle = ServerSupervisor::spawn(rec);
    handle.shutdown();
    handle.shutdown();
}

#[tokio::test]
async fn initialize_failure_transitions_to_failed_state() {
    let Some(rec) = record("bad", BAD_INIT_SCRIPT) else {
        return;
    };
    let handle = ServerSupervisor::spawn(rec);

    let err = handle.refresh().await.expect_err("init should fail");
    assert!(matches!(err, crate::McpError::McpStartupFailed { .. }));

    let state = await_state(&handle, McpStartupState::Failed, Duration::from_secs(2)).await;
    assert_eq!(state, McpStartupState::Failed);
    handle.shutdown();
}

#[tokio::test]
async fn invoke_tool_before_ready_returns_unavailable_after_cap() {
    let Some(rec) = record("bad2", BAD_INIT_SCRIPT) else {
        return;
    };
    let handle = ServerSupervisor::spawn(rec);

    // Drive it past the retry cap by hammering refresh.
    for _ in 0..5 {
        let _ = handle.refresh().await;
    }
    let err = handle
        .invoke_tool("whatever", json!({}))
        .await
        .expect_err("should fail");
    assert!(matches!(
        err,
        crate::McpError::McpServerUnavailable { .. } | crate::McpError::McpStartupFailed { .. }
    ));
    handle.shutdown();
}

//! Integration test: `StdioTransport` + `McpClient` against a tiny echo server.
//!
//! The fixture speaks a minimal MCP-shaped JSON-RPC over stdio:
//! - responds to any request with `{"ok": true, "method": "<method>"}` echoed in `result`.
//! - accepts notifications silently.
//!
//! Unix-only: we invoke the fixture via `python3` which is universally available
//! on macOS and Linux CI runners.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use lpa_mcp::client::McpClient;
use lpa_mcp::transport::{StdioTransport, Transport};
use serde_json::{Value, json};

const ECHO_SCRIPT: &str = r#"
import json
import sys

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except Exception:
        continue
    # Notifications have no id -> ignore silently.
    if "id" not in msg:
        continue
    response = {
        "jsonrpc": "2.0",
        "id": msg["id"],
        "result": {"ok": True, "method": msg.get("method", "")},
    }
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

#[tokio::test]
async fn initialize_round_trip_against_echo_server() {
    let Some(python) = python_bin() else {
        eprintln!("skipping: python3 not found");
        return;
    };
    let command = vec![python.to_owned(), "-c".to_owned(), ECHO_SCRIPT.to_owned()];

    let transport = Arc::new(
        StdioTransport::spawn(&command, None, &BTreeMap::new()).expect("spawn echo server"),
    );
    let client = McpClient::new(Arc::clone(&transport) as Arc<dyn Transport>).expect("client");

    let result: Value = client
        .request_with_timeout(
            "initialize",
            &json!({"protocolVersion": "2025-06-18"}),
            Duration::from_secs(5),
        )
        .await
        .expect("initialize succeeded");

    assert_eq!(result, json!({"ok": true, "method": "initialize"}));

    // Notifications should not stall the client.
    client
        .notify("notifications/initialized", &json!({}))
        .await
        .expect("notify");

    client.shutdown().await.expect("shutdown");
}

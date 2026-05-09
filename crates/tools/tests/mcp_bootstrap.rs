//! End-to-end smoke test for the MCP bootstrap path:
//!
//!  1. Start a real stdio MCP server (Python fixture).
//!  2. Build a `StdMcpManager` from `McpConfig` pointing at it.
//!  3. Auto-start, query `statuses`, `register_mcp_tools` into a `ToolRegistry`.
//!  4. Assert the namespaced `mcp__<server>__<tool>` name appears in the registry.
//!
//! Unix-only: depends on `python3`/`python` being on PATH.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use lpa_mcp::{
    McpConfig, McpManager, McpServerId, McpServerRecord, McpStartupPolicy, McpTransportConfig,
    StdMcpManager, TrustLevel,
};
use lpa_tools::{ToolRegistry, register_mcp_tools};

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
        result = {"content":[{"type":"text","text":"pong"}],"isError": False}
    else:
        result = {"ok": True}
    sys.stdout.write(json.dumps({"jsonrpc":"2.0","id":msg["id"],"result":result}) + "\n")
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
async fn bootstrap_registers_mcp_tools_into_registry() {
    let Some(python) = python_bin() else {
        eprintln!("skipping: python not found");
        return;
    };

    let config = McpConfig {
        auto_start: true,
        refresh_on_config_reload: false,
        servers: vec![McpServerRecord {
            id: McpServerId("echo".into()),
            display_name: "Echo".into(),
            transport: McpTransportConfig::Stdio {
                command: vec![python.into(), "-c".into(), HANDSHAKE_SCRIPT.into()],
                cwd: Option::<PathBuf>::None,
                env: BTreeMap::new(),
            },
            startup_policy: McpStartupPolicy::Eager,
            enabled: true,
            trust_level: TrustLevel::Prompt,
        }],
    };

    let concrete = Arc::new(StdMcpManager::from_config(&config).expect("manager"));
    concrete
        .start_configured(&config)
        .await
        .expect("start_configured");
    let manager: Arc<dyn McpManager> = Arc::clone(&concrete) as Arc<dyn McpManager>;

    let statuses = manager.statuses().await.expect("statuses");
    assert_eq!(statuses.len(), 1);

    let mut registry = ToolRegistry::new();
    register_mcp_tools(&mut registry, Arc::clone(&manager), &statuses);

    let tool = registry
        .get("mcp__echo__ping")
        .expect("mcp tool registered");
    assert_eq!(tool.name(), "mcp__echo__ping");
    assert_eq!(tool.description(), "Ping back");

    concrete.shutdown_all().await;
}

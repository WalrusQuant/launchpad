//! Concrete `McpManager` implementation — façade over per-server supervisors.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::RwLock;
use tracing::warn;

use crate::server::{ServerHandle, ServerSupervisor};
use crate::{
    McpAuthState, McpConfig, McpError, McpManager, McpServerId, McpServerStatus, McpStartupPolicy,
};

/// The default `McpManager`: owns one `ServerHandle` per configured server
/// and dispatches `tools/call` through the matching supervisor task.
pub struct StdMcpManager {
    handles: Arc<RwLock<BTreeMap<McpServerId, ServerHandle>>>,
    auto_start: bool,
}

impl StdMcpManager {
    /// Builds a new manager, spawning one supervisor per enabled server.
    ///
    /// Supervisors do not actually contact their child processes until the
    /// first `ensure_started` call (triggered by `refresh`, `invoke_tool`, or
    /// the eager `start_configured` helper below).
    pub fn from_config(config: &McpConfig) -> Result<Self, McpError> {
        let mut handles = BTreeMap::new();
        for record in &config.servers {
            if !record.enabled {
                continue;
            }
            if handles.contains_key(&record.id) {
                warn!(server = %record.id, "duplicate mcp server id, skipping");
                continue;
            }
            let handle = ServerSupervisor::spawn(record.clone());
            handles.insert(record.id.clone(), handle);
        }
        Ok(Self {
            handles: Arc::new(RwLock::new(handles)),
            auto_start: config.auto_start,
        })
    }

    /// Eagerly starts every supervisor whose policy is `Eager` (or every
    /// enabled supervisor when `auto_start` is set on the global config).
    pub async fn start_configured(&self, config: &McpConfig) -> Result<(), McpError> {
        for record in &config.servers {
            if !record.enabled {
                continue;
            }
            let should_start =
                self.auto_start || matches!(record.startup_policy, McpStartupPolicy::Eager);
            if !should_start {
                continue;
            }
            let handles = self.handles.read().await;
            let Some(handle) = handles.get(&record.id).cloned() else {
                continue;
            };
            drop(handles);
            if let Err(err) = handle.refresh().await {
                warn!(server = %record.id, error = %err, "mcp auto-start failed");
            }
        }
        Ok(())
    }

    /// Shuts down every supervisor, best-effort.
    pub async fn shutdown_all(&self) {
        let handles = {
            let mut map = self.handles.write().await;
            std::mem::take(&mut *map)
        };
        for (_, handle) in handles {
            handle.shutdown();
        }
    }

    async fn handle_for(&self, server_id: &McpServerId) -> Option<ServerHandle> {
        self.handles.read().await.get(server_id).cloned()
    }
}

#[async_trait]
impl McpManager for StdMcpManager {
    async fn statuses(&self) -> Result<Vec<McpServerStatus>, McpError> {
        let handles = self.handles.read().await;
        let mut out = Vec::with_capacity(handles.len());
        for (id, handle) in handles.iter() {
            let startup_state = handle.startup_state();
            let catalog = handle.catalog().await.unwrap_or_default();
            out.push(McpServerStatus {
                server_id: id.clone(),
                startup_state,
                auth_state: McpAuthState::NotRequired,
                tools: catalog.tools,
                resources: catalog.resources,
                resource_templates: catalog.resource_templates,
                last_refreshed_at: catalog.last_refreshed_at,
            });
        }
        Ok(out)
    }

    async fn refresh(&self, server_id: &McpServerId) -> Result<McpServerStatus, McpError> {
        let handle =
            self.handle_for(server_id)
                .await
                .ok_or_else(|| McpError::McpServerUnavailable {
                    server_id: server_id.clone(),
                })?;
        let catalog = handle.refresh().await?;
        Ok(McpServerStatus {
            server_id: server_id.clone(),
            startup_state: handle.startup_state(),
            auth_state: McpAuthState::NotRequired,
            tools: catalog.tools,
            resources: catalog.resources,
            resource_templates: catalog.resource_templates,
            last_refreshed_at: catalog.last_refreshed_at,
        })
    }

    async fn invoke_tool(
        &self,
        server_id: &McpServerId,
        tool_name: &str,
        input: Value,
    ) -> Result<Value, McpError> {
        let handle =
            self.handle_for(server_id)
                .await
                .ok_or_else(|| McpError::McpServerUnavailable {
                    server_id: server_id.clone(),
                })?;
        let result = handle.invoke_tool(tool_name, input).await?;
        if result.is_error {
            let message =
                first_text(&result.content).unwrap_or_else(|| "tool reported is_error".to_owned());
            return Err(McpError::McpToolInvocationFailed {
                server_id: server_id.clone(),
                tool_name: tool_name.to_owned(),
                message,
            });
        }
        serde_json::to_value(&result).map_err(|err| McpError::McpProtocolError {
            server_id: server_id.clone(),
            message: err.to_string(),
        })
    }

    async fn read_resource(&self, server_id: &McpServerId, uri: &str) -> Result<Value, McpError> {
        let _ = uri;
        Err(McpError::McpResourceReadFailed {
            server_id: server_id.clone(),
            uri: uri.to_owned(),
            message: "not implemented".to_owned(),
        })
    }
}

fn first_text(content: &[crate::protocol::ContentBlock]) -> Option<String> {
    for block in content {
        if let crate::protocol::ContentBlock::Text { text } = block {
            return Some(text.clone());
        }
    }
    None
}

/// Returns `McpServerStatus` for a single handle — exposed so the bootstrap
/// can pre-render statuses without holding the handle map lock.
pub async fn snapshot_status(handle: &ServerHandle) -> McpServerStatus {
    let catalog = handle.catalog().await.unwrap_or_default();
    McpServerStatus {
        server_id: handle.server_id().clone(),
        startup_state: handle.startup_state(),
        auth_state: McpAuthState::NotRequired,
        tools: catalog.tools,
        resources: catalog.resources,
        resource_templates: catalog.resource_templates,
        last_refreshed_at: catalog.last_refreshed_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{McpConfig, McpServerId, McpServerRecord, McpStartupPolicy, McpTransportConfig};

    fn fake_record(id: &str) -> McpServerRecord {
        McpServerRecord {
            id: McpServerId(id.into()),
            display_name: id.to_owned(),
            transport: McpTransportConfig::Stdio {
                command: vec!["/this/binary/does/not/exist".into()],
                cwd: None,
                env: Default::default(),
            },
            startup_policy: McpStartupPolicy::Manual,
            enabled: true,
            trust_level: crate::TrustLevel::default(),
        }
    }

    #[tokio::test]
    async fn statuses_enumerates_all_servers() {
        let config = McpConfig {
            servers: vec![fake_record("a"), fake_record("b")],
            auto_start: false,
            refresh_on_config_reload: false,
        };
        let manager = StdMcpManager::from_config(&config).expect("manager");
        let statuses = manager.statuses().await.expect("statuses");
        assert_eq!(statuses.len(), 2);
        assert!(statuses.iter().any(|s| s.server_id.0.as_str() == "a"));
        assert!(statuses.iter().any(|s| s.server_id.0.as_str() == "b"));
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn unknown_server_id_returns_unavailable() {
        let config = McpConfig {
            servers: vec![fake_record("only")],
            auto_start: false,
            refresh_on_config_reload: false,
        };
        let manager = StdMcpManager::from_config(&config).expect("manager");
        let err = manager
            .invoke_tool(
                &McpServerId("missing".into()),
                "ping",
                serde_json::json!({}),
            )
            .await
            .expect_err("should be unavailable");
        assert!(matches!(err, McpError::McpServerUnavailable { .. }));
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn read_resource_returns_not_implemented() {
        let config = McpConfig {
            servers: vec![fake_record("a")],
            auto_start: false,
            refresh_on_config_reload: false,
        };
        let manager = StdMcpManager::from_config(&config).expect("manager");
        let err = manager
            .read_resource(&McpServerId("a".into()), "resource://doc")
            .await
            .expect_err("stubbed");
        match err {
            McpError::McpResourceReadFailed { message, .. } => {
                assert_eq!(message, "not implemented");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
        manager.shutdown_all().await;
    }

    #[tokio::test]
    async fn duplicate_server_ids_deduped_at_construction() {
        let config = McpConfig {
            servers: vec![fake_record("dup"), fake_record("dup")],
            auto_start: false,
            refresh_on_config_reload: false,
        };
        let manager = StdMcpManager::from_config(&config).expect("manager");
        let statuses = manager.statuses().await.expect("statuses");
        assert_eq!(statuses.len(), 1);
        manager.shutdown_all().await;
    }
}

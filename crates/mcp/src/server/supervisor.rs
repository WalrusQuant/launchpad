//! Per-server supervisor task.
//!
//! Owns the `McpClient` for one configured MCP server and drives its lifecycle
//! state machine. Failures of one supervisor never affect another — each runs
//! in its own dedicated tokio task.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::client::{McpClient, McpClientError};
use crate::metrics;
use crate::protocol::{
    CallToolParams, ClientCapabilities, ClientInfo, InitializeParams, ListToolsResult,
    MCP_PROTOCOL_VERSION,
};
use crate::transport::{StdioTransport, Transport};
use crate::{McpError, McpServerRecord, McpStartupState, McpToolDescriptor, McpTransportConfig};

use super::catalog::ServerCatalog;
use super::handle::{ServerHandle, SupervisorCmd};

const DEFAULT_CLIENT_NAME: &str = "launchpad-agent";
const DEFAULT_CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(15);
const TOOLS_LIST_TIMEOUT: Duration = Duration::from_secs(15);
const TOOLS_CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum number of consecutive (re)start attempts before we park in `Failed`.
const MAX_START_ATTEMPTS: u32 = 3;
/// Base backoff before a retry attempt.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// Per-server supervisor. Owns the child process transport + client.
pub struct ServerSupervisor {
    record: McpServerRecord,
    status_tx: watch::Sender<McpStartupState>,
    catalog: ServerCatalog,
    client: Option<Arc<McpClient>>,
    start_attempts: u32,
}

impl ServerSupervisor {
    /// Spawns the supervisor task and returns a handle to talk to it.
    pub fn spawn(record: McpServerRecord) -> ServerHandle {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<SupervisorCmd>();
        let (status_tx, status_rx) = watch::channel(McpStartupState::Stopped);
        let server_id = record.id.clone();

        let sup = Self {
            record,
            status_tx,
            catalog: ServerCatalog::default(),
            client: None,
            start_attempts: 0,
        };
        tokio::spawn(sup.run(cmd_rx));

        ServerHandle {
            server_id,
            cmd_tx,
            status_rx,
        }
    }

    async fn run(mut self, mut cmd_rx: mpsc::UnboundedReceiver<SupervisorCmd>) {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                SupervisorCmd::Shutdown => {
                    self.shutdown_client().await;
                    let _ = self.status_tx.send(McpStartupState::Stopped);
                    break;
                }
                SupervisorCmd::CurrentCatalog { reply } => {
                    let _ = reply.send(self.catalog.clone());
                }
                SupervisorCmd::Refresh { reply } => {
                    let outcome = self.ensure_started().await;
                    let response = match outcome {
                        Ok(()) => self.refresh_tools().await.map(|_| self.catalog.clone()),
                        Err(err) => Err(err),
                    };
                    let _ = reply.send(response);
                }
                SupervisorCmd::InvokeTool {
                    tool_name,
                    arguments,
                    reply,
                } => {
                    let outcome = self.ensure_started().await;
                    let response = match outcome {
                        Ok(()) => self.invoke_tool(&tool_name, arguments).await,
                        Err(err) => Err(err),
                    };
                    let _ = reply.send(response);
                }
            }
        }
        self.shutdown_client().await;
    }

    async fn ensure_started(&mut self) -> Result<(), McpError> {
        if matches!(self.status_tx.borrow().clone(), McpStartupState::Ready) {
            return Ok(());
        }
        if matches!(self.status_tx.borrow().clone(), McpStartupState::Failed)
            && self.start_attempts >= MAX_START_ATTEMPTS
        {
            return Err(McpError::McpServerUnavailable {
                server_id: self.record.id.clone(),
            });
        }
        self.start().await
    }

    async fn start(&mut self) -> Result<(), McpError> {
        let _ = self.status_tx.send(McpStartupState::Starting);
        metrics::server_start(&self.record.id);

        let client = match self.build_client().await {
            Ok(c) => c,
            Err(err) => {
                self.start_attempts = self.start_attempts.saturating_add(1);
                let _ = self.status_tx.send(McpStartupState::Failed);
                metrics::server_failed(&self.record.id, &err.to_string());
                return Err(err);
            }
        };

        if let Err(err) = self.handshake(&client).await {
            self.start_attempts = self.start_attempts.saturating_add(1);
            let _ = self.status_tx.send(McpStartupState::Failed);
            metrics::server_failed(&self.record.id, &err.to_string());
            let _ = client.shutdown().await;
            // Backoff between attempts to avoid hammering a crash-looping server.
            tokio::time::sleep(backoff_for_attempt(self.start_attempts)).await;
            return Err(err);
        }

        self.client = Some(Arc::new(client));
        self.refresh_tools().await?;
        self.start_attempts = 0;
        let _ = self.status_tx.send(McpStartupState::Ready);
        metrics::server_ready(&self.record.id, self.catalog.tools.len());
        info!(server = %self.record.id, "mcp server ready");
        Ok(())
    }

    async fn build_client(&self) -> Result<McpClient, McpError> {
        let transport = self.build_transport()?;
        McpClient::new(transport).map_err(|err| McpError::McpStartupFailed {
            server_id: self.record.id.clone(),
            message: err.to_string(),
        })
    }

    fn build_transport(&self) -> Result<Arc<dyn Transport>, McpError> {
        match &self.record.transport {
            McpTransportConfig::Stdio { command, cwd, env } => {
                let transport =
                    StdioTransport::spawn(command, cwd.as_deref(), env).map_err(|err| {
                        McpError::McpStartupFailed {
                            server_id: self.record.id.clone(),
                            message: format!("stdio spawn failed: {err}"),
                        }
                    })?;
                Ok(Arc::new(transport))
            }
            McpTransportConfig::StreamableHttp { .. } => Err(McpError::McpStartupFailed {
                server_id: self.record.id.clone(),
                message: "streamable-http transport not enabled in this build".to_owned(),
            }),
        }
    }

    async fn handshake(&self, client: &McpClient) -> Result<(), McpError> {
        let params = InitializeParams {
            protocol_version: MCP_PROTOCOL_VERSION.to_owned(),
            capabilities: ClientCapabilities {
                extensions: BTreeMap::new(),
            },
            client_info: ClientInfo {
                name: DEFAULT_CLIENT_NAME.to_owned(),
                version: DEFAULT_CLIENT_VERSION.to_owned(),
            },
        };
        let _: Value = client
            .request_with_timeout("initialize", &params, INITIALIZE_TIMEOUT)
            .await
            .map_err(|err| self.startup_error(err))?;

        client
            .notify("notifications/initialized", &json!({}))
            .await
            .map_err(|err| self.startup_error(err))?;
        Ok(())
    }

    async fn refresh_tools(&mut self) -> Result<(), McpError> {
        let Some(client) = self.client.as_ref() else {
            return Err(McpError::McpServerUnavailable {
                server_id: self.record.id.clone(),
            });
        };
        let result: ListToolsResult = client
            .request_with_timeout("tools/list", &json!({}), TOOLS_LIST_TIMEOUT)
            .await
            .map_err(|err| self.protocol_error(err))?;

        let tools = result
            .tools
            .into_iter()
            .map(|spec| McpToolDescriptor {
                server_id: self.record.id.clone(),
                name: spec.name,
                description: spec.description,
                input_schema: spec.input_schema,
                annotations: spec.annotations,
            })
            .collect();
        self.catalog = ServerCatalog::with_tools(tools);
        Ok(())
    }

    async fn invoke_tool(
        &mut self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<crate::protocol::CallToolResult, McpError> {
        let Some(client) = self.client.as_ref() else {
            return Err(McpError::McpServerUnavailable {
                server_id: self.record.id.clone(),
            });
        };
        metrics::tool_call(&self.record.id, tool_name);
        let params = CallToolParams {
            name: tool_name.to_owned(),
            arguments,
        };
        let result: Result<crate::protocol::CallToolResult, _> = client
            .request_with_timeout("tools/call", &params, TOOLS_CALL_TIMEOUT)
            .await;
        match result {
            Ok(r) => {
                if r.is_error {
                    metrics::tool_call_failure(&self.record.id, tool_name, "is_error");
                }
                Ok(r)
            }
            Err(err) => {
                metrics::tool_call_failure(&self.record.id, tool_name, &err.to_string());
                Err(McpError::McpToolInvocationFailed {
                    server_id: self.record.id.clone(),
                    tool_name: tool_name.to_owned(),
                    message: err.to_string(),
                })
            }
        }
    }

    async fn shutdown_client(&mut self) {
        if let Some(client) = self.client.take() {
            if let Err(err) = client.shutdown().await {
                warn!(server = %self.record.id, error = %err, "mcp shutdown error");
            } else {
                debug!(server = %self.record.id, "mcp client shut down");
            }
        }
    }

    fn startup_error(&self, err: McpClientError) -> McpError {
        error!(server = %self.record.id, error = %err, "mcp startup failed");
        McpError::McpStartupFailed {
            server_id: self.record.id.clone(),
            message: err.to_string(),
        }
    }

    fn protocol_error(&self, err: McpClientError) -> McpError {
        McpError::McpProtocolError {
            server_id: self.record.id.clone(),
            message: err.to_string(),
        }
    }
}

fn backoff_for_attempt(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => BASE_BACKOFF,
        2 => BASE_BACKOFF * 4,
        _ => BASE_BACKOFF * 16,
    }
}

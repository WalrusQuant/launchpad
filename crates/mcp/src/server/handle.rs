//! Public handle used by the manager to talk to a running supervisor task.

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};

use crate::protocol::CallToolResult;
use crate::{McpError, McpServerId, McpStartupState};

use super::catalog::ServerCatalog;

/// Command messages accepted by a `ServerSupervisor`.
pub(super) enum SupervisorCmd {
    /// Asks the supervisor to re-run `tools/list` and refresh its catalog.
    Refresh {
        /// The oneshot used to return the result.
        reply: oneshot::Sender<Result<ServerCatalog, McpError>>,
    },
    /// Asks the supervisor to invoke one tool.
    InvokeTool {
        /// The tool name as the server advertised it.
        tool_name: String,
        /// The JSON argument payload.
        arguments: Value,
        /// The oneshot used to return the result.
        reply: oneshot::Sender<Result<CallToolResult, McpError>>,
    },
    /// Asks the supervisor to fetch the current catalog snapshot.
    CurrentCatalog {
        /// The oneshot used to return the snapshot.
        reply: oneshot::Sender<ServerCatalog>,
    },
    /// Signals the supervisor to shut the server down and exit its loop.
    Shutdown,
}

/// Cheap-to-clone handle used by `StdMcpManager` to interact with one supervisor.
#[derive(Clone)]
pub struct ServerHandle {
    pub(super) server_id: McpServerId,
    pub(super) cmd_tx: mpsc::UnboundedSender<SupervisorCmd>,
    pub(super) status_rx: watch::Receiver<McpStartupState>,
}

impl ServerHandle {
    /// Returns the server identifier this handle addresses.
    pub fn server_id(&self) -> &McpServerId {
        &self.server_id
    }

    /// Returns the latest observed startup state.
    pub fn startup_state(&self) -> McpStartupState {
        self.status_rx.borrow().clone()
    }

    /// Returns the current catalog snapshot.
    pub async fn catalog(&self) -> Result<ServerCatalog, McpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::CurrentCatalog { reply: tx })
            .map_err(|_| McpError::McpServerUnavailable {
                server_id: self.server_id.clone(),
            })?;
        rx.await.map_err(|_| McpError::McpServerUnavailable {
            server_id: self.server_id.clone(),
        })
    }

    /// Asks the supervisor to re-run discovery.
    pub async fn refresh(&self) -> Result<ServerCatalog, McpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::Refresh { reply: tx })
            .map_err(|_| McpError::McpServerUnavailable {
                server_id: self.server_id.clone(),
            })?;
        rx.await.map_err(|_| McpError::McpServerUnavailable {
            server_id: self.server_id.clone(),
        })?
    }

    /// Dispatches one `tools/call`.
    pub async fn invoke_tool(
        &self,
        tool_name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, McpError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SupervisorCmd::InvokeTool {
                tool_name: tool_name.to_owned(),
                arguments,
                reply: tx,
            })
            .map_err(|_| McpError::McpServerUnavailable {
                server_id: self.server_id.clone(),
            })?;
        rx.await.map_err(|_| McpError::McpServerUnavailable {
            server_id: self.server_id.clone(),
        })?
    }

    /// Signals orderly shutdown.
    pub fn shutdown(&self) {
        let _ = self.cmd_tx.send(SupervisorCmd::Shutdown);
    }
}

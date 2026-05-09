use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use tokio::sync::{Mutex, mpsc};

use crate::{
    ClientTransportKind, ConnectionState, InitializeResult, ProtocolErrorCode, ServerCapabilities,
    approval::SharedApprovalManager,
    execution::{RuntimeSession, ServerRuntimeDependencies},
    persistence::RolloutStore,
};

mod connection_runtime;
mod execute_turn;
mod handlers_events;
mod handlers_session;
mod handlers_turn;
mod items;
mod session_titles;
mod skills;

use connection_runtime::ConnectionRuntime;

pub struct ServerRuntime {
    metadata: InitializeResult,
    deps: ServerRuntimeDependencies,
    rollout_store: RolloutStore,
    sessions: Mutex<HashMap<lpa_core::SessionId, Arc<Mutex<RuntimeSession>>>>,
    connections: Mutex<HashMap<u64, ConnectionRuntime>>,
    active_tasks: Mutex<HashMap<lpa_core::SessionId, tokio::task::AbortHandle>>,
    next_connection_id: AtomicU64,
    approval_manager: SharedApprovalManager,
}

impl ServerRuntime {
    pub fn new(server_home: PathBuf, deps: ServerRuntimeDependencies) -> Arc<Self> {
        let rollout_store = RolloutStore::new(server_home.clone());
        Arc::new(Self {
            metadata: InitializeResult {
                server_name: "lpa-server".into(),
                server_version: env!("CARGO_PKG_VERSION").into(),
                platform_family: std::env::consts::FAMILY.into(),
                platform_os: std::env::consts::OS.into(),
                server_home,
                capabilities: ServerCapabilities {
                    session_resume: true,
                    session_fork: true,
                    turn_interrupt: true,
                    approval_requests: true,
                    event_streaming: true,
                },
            },
            deps,
            rollout_store,
            sessions: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            active_tasks: Mutex::new(HashMap::new()),
            next_connection_id: AtomicU64::new(1),
            approval_manager: Arc::new(Mutex::new(crate::approval::ApprovalManager::new())),
        })
    }

    pub async fn load_persisted_sessions(self: &Arc<Self>) -> anyhow::Result<()> {
        let sessions = self.rollout_store.load_sessions(&self.deps)?;
        tracing::info!(session_count = sessions.len(), "loaded persisted sessions");
        let mut runtime_sessions = self.sessions.lock().await;
        runtime_sessions.extend(sessions);
        Ok(())
    }

    pub async fn register_connection(
        self: &Arc<Self>,
        transport: ClientTransportKind,
        sender: mpsc::UnboundedSender<serde_json::Value>,
    ) -> u64 {
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::SeqCst);
        let mut connections = self.connections.lock().await;
        connections.insert(
            connection_id,
            ConnectionRuntime {
                transport,
                state: ConnectionState::Connected,
                sender,
                opt_out_notification_methods: std::collections::HashSet::new(),
                subscriptions: Vec::new(),
                next_event_seq: 1,
            },
        );
        tracing::info!(
            connection_id,
            transport = ?connections
                .get(&connection_id)
                .map(|connection| connection.transport.clone())
                .expect("connection inserted"),
            active_connections = connections.len(),
            "registered client connection"
        );
        connection_id
    }

    pub async fn unregister_connection(&self, connection_id: u64) {
        let mut connections = self.connections.lock().await;
        let removed = connections.remove(&connection_id);
        tracing::info!(
            connection_id,
            transport = ?removed.as_ref().map(|connection| connection.transport.clone()),
            active_connections = connections.len(),
            "unregistered client connection"
        );
    }

    pub async fn handle_incoming(
        self: &Arc<Self>,
        connection_id: u64,
        message: serde_json::Value,
    ) -> Option<serde_json::Value> {
        let method = message.get("method")?.as_str()?.to_string();
        let id = message.get("id").cloned();
        let params = message
            .get("params")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        tracing::debug!(
            connection_id,
            method,
            has_id = id.is_some(),
            "received client message"
        );

        if method == "initialized" {
            if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
                connection.state = ConnectionState::Ready;
            }
            tracing::info!(connection_id, "client completed initialized handshake");
            return None;
        }
        if method == "initialize" {
            return Some(self.handle_initialize(connection_id, id, params).await);
        }
        if !self.connection_ready(connection_id).await {
            return id.map(|request_id| {
                self.error_response(
                    request_id,
                    ProtocolErrorCode::NotInitialized,
                    "connection has not completed initialize/initialized",
                )
            });
        }

        match method.as_str() {
            "session/start" => Some(self.handle_session_start(connection_id, id?, params).await),
            "session/list" => Some(self.handle_session_list(id?, params).await),
            "session/title/update" => Some(self.handle_session_title_update(id?, params).await),
            "session/resume" => Some(self.handle_session_resume(connection_id, id?, params).await),
            "session/fork" => Some(self.handle_session_fork(connection_id, id?, params).await),
            "skills/list" => Some(self.handle_skills_list(id?, params).await),
            "skills/changed" => Some(self.handle_skills_changed(id?, params).await),
            "turn/start" => Some(self.handle_turn_start(id?, params).await),
            "turn/interrupt" => Some(self.handle_turn_interrupt(id?, params).await),
            "turn/steer" => Some(self.handle_turn_steer(connection_id, id?, params).await),
            "approval/respond" => Some(self.handle_approval_respond(id?, params).await),
            "events/subscribe" => Some(
                self.handle_events_subscribe(connection_id, id?, params)
                    .await,
            ),
            _ => Some(self.error_response(
                id?,
                ProtocolErrorCode::InvalidParams,
                format!("unknown method: {method}"),
            )),
        }
    }
}

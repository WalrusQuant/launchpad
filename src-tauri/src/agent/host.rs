use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, mpsc};

use lpa_core::{
    AppConfigLoader, FileSystemAppConfigLoader, FileSystemSkillCatalog, ModelCatalog,
    PresetModelCatalog, SkillsConfig,
};
use lpa_mcp::{McpManager, StdMcpManager, TrustLevel};
use lpa_protocol::ClientTransportKind;
use lpa_server::{ServerRuntime, ServerRuntimeDependencies, load_server_provider};
use lpa_tools::{MCP_TOOL_PREFIX, ToolRegistry, register_mcp_tools};
use lpa_utils::FileSystemConfigPathResolver;

/// Per-window connection bookkeeping.
struct AgentConn {
    connection_id: u64,
    /// Forwarder task that drains the mpsc receiver into Tauri events.
    /// Dropping the JoinHandle aborts the task on window unregister.
    forwarder: tokio::task::JoinHandle<()>,
}

/// Process-wide agent state. One `ServerRuntime` shared across windows;
/// each window owns its own connection_id.
///
/// The runtime is wrapped in a `Mutex<Option<...>>` (rather than a
/// `OnceCell`) so settings changes can drop and rebuild it. Active
/// connections that point at the old runtime are torn down at reload time;
/// each chat tab reconnects on its next user action.
pub struct AgentState {
    runtime: Mutex<Option<Arc<ServerRuntime>>>,
    /// Concrete MCP manager kept around so we can call `shutdown_all` on
    /// quit / reload.
    mcp_manager: Mutex<Option<Arc<StdMcpManager>>>,
    /// Connections keyed by Tauri window label.
    connections: Mutex<HashMap<String, AgentConn>>,
}

impl AgentState {
    pub fn new() -> Self {
        // Write safe defaults to ~/.launchpad/agent-config.json on first
        // launch if the file is empty/missing those fields. Makes disk the
        // single source of truth so the UI dropdown and the session_start
        // envelope always agree on what the user's defaults are.
        super::config::initialize_defaults_if_missing();
        Self {
            runtime: Mutex::new(None),
            mcp_manager: Mutex::new(None),
            connections: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the shared runtime, building it on first call. Requires an
    /// `AppHandle` so Launchpad-native tools can emit Tauri events back to
    /// the frontend. Reload drops the runtime; the next `runtime()` call
    /// rebuilds it with a fresh handle from whichever command triggered it.
    pub async fn runtime(&self, app: &AppHandle) -> Result<Arc<ServerRuntime>> {
        // Fast path: runtime already built.
        if let Some(runtime) = self.runtime.lock().await.clone() {
            return Ok(runtime);
        }
        // Slow path: build under the lock so concurrent callers share the
        // same runtime instance. The Tokio Mutex held across an await is
        // intentional — we want the second caller to wait, not race.
        let mut guard = self.runtime.lock().await;
        if let Some(runtime) = guard.clone() {
            return Ok(runtime);
        }
        let runtime = self.build_runtime(app.clone()).await?;
        *guard = Some(runtime.clone());
        Ok(runtime)
    }

    /// Tear down every connection and drop the runtime. The next
    /// `runtime()` call rebuilds it from the latest agent-config.json (so
    /// provider/model/key changes take effect without an app restart).
    /// Active turns get cancelled when their connection drops.
    pub async fn reload(&self) -> Result<()> {
        // Lock order matters: every other path that touches both runtime
        // and connections takes runtime FIRST (see connect_window), so this
        // must too — otherwise reload (connections→runtime) deadlocks
        // against connect_window (runtime→connections) under concurrency.
        //
        // Snapshot the runtime + drain connections under each lock
        // independently — never holding both at once — then do the
        // unregister/abort/shutdown work after both locks have been
        // released.
        let runtime_opt = self.runtime.lock().await.take();
        let drained: Vec<AgentConn> = {
            let mut conns = self.connections.lock().await;
            conns.drain().map(|(_, conn)| conn).collect()
        };
        for conn in drained {
            conn.forwarder.abort();
            if let Some(rt) = runtime_opt.as_ref() {
                rt.unregister_connection(conn.connection_id).await;
            }
        }
        // Drop the old MCP supervisors and the runtime itself last.
        let mcp = self.mcp_manager.lock().await.take();
        if let Some(m) = mcp {
            m.shutdown_all().await;
        }
        drop(runtime_opt);
        Ok(())
    }

    async fn build_runtime(&self, app: AppHandle) -> Result<Arc<ServerRuntime>> {
        // First-run: seed `~/.lpagent/skills/` with starter skills so the
        // slash menu isn't empty out of the box. No-op once the dir exists,
        // even if the user has deleted some — that signals deliberate edit.
        seed_starter_skills_if_missing();

        // Mirrors `lpa_server::run_server_process` minus the listener loop.
        let resolver = FileSystemConfigPathResolver::from_env()?;
        let loader = FileSystemAppConfigLoader::new(resolver.user_config_dir());
        let config = loader.load(None)?;

        // Sync block: write LPA_* env vars from agent-config.json, then load
        // the provider that reads those same vars. The env-write guard is
        // held across both so a concurrent agent_is_configured can never
        // see a half-written state. MUST drop before the awaits below —
        // std::sync::MutexGuard is not async-safe.
        let provider = {
            let _env_guard = crate::agent::config::apply_env_from_user_config();
            load_server_provider(&resolver.user_config_file(), None)?
        };

        let mut registry = ToolRegistry::new();
        lpa_tools::register_builtin_tools(&mut registry);
        crate::agent::native_tools::register_native_tools(&mut registry, app);

        let concrete_mcp_manager = Arc::new(StdMcpManager::from_config(&config.mcp)?);
        let mcp_manager: Arc<dyn McpManager> =
            Arc::clone(&concrete_mcp_manager) as Arc<dyn McpManager>;
        if config.mcp.auto_start {
            concrete_mcp_manager.start_configured(&config.mcp).await?;
        }
        let statuses = mcp_manager.statuses().await.unwrap_or_default();
        register_mcp_tools(&mut registry, Arc::clone(&mcp_manager), &statuses);

        let trusted_server_ids: std::collections::HashSet<_> = config
            .mcp
            .servers
            .iter()
            .filter(|s| matches!(s.trust_level, TrustLevel::Trusted))
            .map(|s| s.id.clone())
            .collect();
        let trusted_mcp_tool_names: Vec<String> = statuses
            .iter()
            .filter(|s| trusted_server_ids.contains(&s.server_id))
            .flat_map(|s| {
                s.tools
                    .iter()
                    .map(|t| format!("{MCP_TOOL_PREFIX}{}__{}", s.server_id, t.name))
            })
            .collect();

        let model_catalog: Arc<dyn ModelCatalog> = Arc::new(PresetModelCatalog::load()?);
        let user_skill_roots = config
            .skills
            .user_roots
            .iter()
            .cloned()
            .map(|root| {
                if root.is_absolute() {
                    root
                } else {
                    resolver.user_config_dir().join(root)
                }
            })
            .collect();
        // Honor the configured workspace_roots (default: ["skills"]). The
        // catalog joins each entry against the cwd passed to discover() at
        // skills/list time, so per-project skill folders work as long as the
        // frontend forwards the project path.
        let workspace_skill_roots = config.skills.workspace_roots.clone();
        let skill_catalog = Box::new(FileSystemSkillCatalog::new(SkillsConfig {
            enabled: config.skills.enabled,
            user_roots: user_skill_roots,
            workspace_roots: workspace_skill_roots,
            watch_for_changes: config.skills.watch_for_changes,
        }));

        let runtime = ServerRuntime::new(
            resolver.user_config_dir(),
            ServerRuntimeDependencies::new(
                provider.provider,
                Arc::new(registry),
                provider.default_model,
                model_catalog,
                None,
                skill_catalog,
                Arc::clone(&mcp_manager),
                trusted_mcp_tool_names,
            ),
        );
        runtime.load_persisted_sessions().await?;
        *self.mcp_manager.lock().await = Some(concrete_mcp_manager);
        Ok(runtime)
    }

    /// Register a new per-window connection and start its forwarder task.
    /// Returns the connection_id.
    pub async fn connect_window(&self, app: AppHandle, window_label: String) -> Result<u64> {
        let runtime = self.runtime(&app).await?;
        let (tx, rx) = mpsc::unbounded_channel::<serde_json::Value>();
        // WebSocket-style transport gates delivery on explicit
        // events/subscribe per session; Stdio always delivers every event to
        // every connection. We want per-window isolation so each chat tab
        // only sees events for sessions it subscribed to.
        let connection_id = runtime
            .register_connection(ClientTransportKind::WebSocket, tx)
            .await;

        let event_name = format!("agent:event:{}", window_label);
        let forwarder = tokio::spawn(forward_events(app, event_name, rx));

        let mut conns = self.connections.lock().await;
        // If a window reconnects (devtools reload), tear down the old one first.
        if let Some(prev) = conns.remove(&window_label) {
            prev.forwarder.abort();
            let runtime = runtime.clone();
            tokio::spawn(async move {
                runtime.unregister_connection(prev.connection_id).await;
            });
        }
        conns.insert(
            window_label,
            AgentConn {
                connection_id,
                forwarder,
            },
        );
        Ok(connection_id)
    }

    /// Tear down a window's connection. Sessions persist in the runtime.
    pub async fn disconnect_window(&self, window_label: &str) {
        let runtime = self.runtime.lock().await.clone();
        let mut conns = self.connections.lock().await;
        if let Some(conn) = conns.remove(window_label) {
            conn.forwarder.abort();
            if let Some(rt) = runtime {
                rt.unregister_connection(conn.connection_id).await;
            }
        }
    }

    /// Look up the connection_id for a window. Returns None if the window has
    /// never called `agent_connect`.
    pub async fn connection_id_for(&self, window_label: &str) -> Option<u64> {
        self.connections
            .lock()
            .await
            .get(window_label)
            .map(|c| c.connection_id)
    }

    /// Best-effort shutdown of MCP supervisors. Called on app exit.
    pub async fn shutdown(&self) {
        if let Some(mcp) = self.mcp_manager.lock().await.take() {
            mcp.shutdown_all().await;
        }
    }
}

impl Default for AgentState {
    fn default() -> Self {
        Self::new()
    }
}

/// Drains the per-connection mpsc and republishes each envelope as a Tauri
/// event on the per-window channel. Frontend listens via
/// `listen('agent:event:<window_label>', ...)`.
async fn forward_events(
    app: AppHandle,
    event_name: String,
    mut rx: mpsc::UnboundedReceiver<serde_json::Value>,
) {
    while let Some(envelope) = rx.recv().await {
        // Best-effort emit; webview may have closed, in which case we drop the
        // event silently rather than crashing the runtime.
        if let Err(err) = app.emit(&event_name, &envelope) {
            tracing::debug!(?err, %event_name, "agent event emit failed");
        }
    }
}

/// Bundled starter skills, embedded at compile time. Each tuple is
/// `(skill_name, SKILL.md content)` and lands at
/// `~/.lpagent/skills/<skill_name>/SKILL.md` on first run. Sourced from
/// `src-tauri/starter-skills/<name>/SKILL.md`.
const STARTER_SKILLS: &[(&str, &str)] = &[
    (
        "commit",
        include_str!("../../starter-skills/commit/SKILL.md"),
    ),
    (
        "review",
        include_str!("../../starter-skills/review/SKILL.md"),
    ),
    (
        "explain",
        include_str!("../../starter-skills/explain/SKILL.md"),
    ),
    ("plan", include_str!("../../starter-skills/plan/SKILL.md")),
];

/// Seed the user's skills folder with the starter set on first run. The
/// "first run" signal is the *absence* of `<lpa_home>/skills/`; if the user
/// has deleted skills they didn't want, the empty-but-present dir keeps us
/// from re-seeding on every launch. Best-effort — failures here log a
/// warning but don't block runtime construction.
fn seed_starter_skills_if_missing() {
    let lpa_home = if let Some(p) = std::env::var_os("LPA_HOME") {
        std::path::PathBuf::from(p)
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".lpagent")
    } else {
        return;
    };
    let skills_root = lpa_home.join("skills");
    if skills_root.exists() {
        return;
    }
    if let Err(err) = std::fs::create_dir_all(&skills_root) {
        tracing::warn!(?err, path = %skills_root.display(), "failed to create skills root for seeding");
        return;
    }
    use std::io::Write;
    let mut written = 0usize;
    for (name, body) in STARTER_SKILLS {
        let dir = skills_root.join(name);
        if let Err(err) = std::fs::create_dir_all(&dir) {
            tracing::warn!(?err, path = %dir.display(), "failed to create starter skill dir");
            continue;
        }
        let file = dir.join("SKILL.md");
        // create_new (O_EXCL on POSIX) — TOCTOU-safe vs. a concurrent
        // window doing the same seed. AlreadyExists means the other side
        // already wrote it, which is a successful no-op since the content
        // is byte-identical.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
        {
            Ok(mut f) => {
                if let Err(err) = f.write_all(body.as_bytes()) {
                    tracing::warn!(?err, path = %file.display(), "failed to write starter skill");
                } else {
                    written += 1;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                tracing::warn!(?err, path = %file.display(), "failed to open starter skill for write");
            }
        }
    }
    if written > 0 {
        tracing::info!(path = %skills_root.display(), count = written, "seeded starter skills");
    }
}

/// Helper so the public Tauri command module can resolve a window's
/// connection_id without duplicating the lookup logic.
pub(crate) async fn require_connection_id(
    state: &AgentState,
    app: &AppHandle,
    window_label: &str,
) -> Result<u64> {
    if let Some(id) = state.connection_id_for(window_label).await {
        return Ok(id);
    }
    state
        .connect_window(app.clone(), window_label.to_string())
        .await
}

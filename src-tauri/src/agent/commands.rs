//! Tauri commands that expose the in-process agent runtime to the frontend.
//!
//! Two flavors:
//!   * `agent_send_envelope` — raw JSON-RPC pass-through, used for
//!     events/subscribe, skills/list, anything we don't want a typed wrapper
//!     for.
//!   * Typed wrappers — convenience commands that build the envelope server-
//!     side so the frontend doesn't hand-roll JSON for hot-path operations.

use serde_json::{Value, json};
use tauri::{AppHandle, State, Window};
use uuid::Uuid;

use super::host::{AgentState, require_connection_id};

fn err(msg: impl std::fmt::Display) -> String {
    msg.to_string()
}

fn rpc_id() -> String {
    Uuid::new_v4().to_string()
}

/// Drop the runtime and all per-window connections so the next chat
/// interaction rebuilds it from the latest agent-config.json. Called from
/// the Settings panel's "Save & reload" button so users don't have to
/// quit the app to change provider / model / API key.
#[tauri::command]
pub async fn agent_reload(state: State<'_, AgentState>) -> Result<(), String> {
    state.reload().await.map_err(err)
}

/// Establish (or refresh) this window's connection to the agent runtime.
/// The first chat tab in a window calls this; subsequent calls are no-ops.
#[tauri::command]
pub async fn agent_connect(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
) -> Result<u64, String> {
    state
        .connect_window(app, window.label().to_string())
        .await
        .map_err(err)
}

/// Tear down this window's connection. Sessions persist on the runtime side
/// and remain resumable from any window.
#[tauri::command]
pub async fn agent_disconnect(
    window: Window,
    state: State<'_, AgentState>,
) -> Result<(), String> {
    state.disconnect_window(window.label()).await;
    Ok(())
}

/// Pass-through JSON-RPC envelope. Routes to the calling window's
/// connection.
#[tauri::command]
pub async fn agent_send_envelope(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    envelope: Value,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// Send the `initialize` handshake. Required once per connection before
/// any other request will be accepted by the runtime.
#[tauri::command]
pub async fn agent_initialize(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let init = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "initialize",
        "params": {
            "client_name": "launchpad",
            "client_version": env!("CARGO_PKG_VERSION"),
            "transport": "web_socket",
            "supports_streaming": true,
            "supports_binary_images": true,
            "opt_out_notification_methods": []
        }
    });
    let response = runtime.handle_incoming(connection_id, init).await;

    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    let _ = runtime.handle_incoming(connection_id, initialized).await;
    Ok(response)
}

/// Start a new session rooted at `project_path`. Returns the runtime's
/// `session/start` response (caller extracts session_id from `result`).
#[tauri::command]
pub async fn agent_session_start(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    project_path: Option<String>,
    model: Option<String>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    // session/start requires `cwd` (PathBuf) and `ephemeral` (bool).
    // Falls back to the user's home dir if no project is open — the runtime
    // refuses to construct a session without a working directory.
    let cwd = project_path
        .filter(|p| !p.is_empty())
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| ".".to_string());
    let mut params = serde_json::Map::new();
    params.insert("cwd".into(), Value::String(cwd));
    params.insert("ephemeral".into(), Value::Bool(false));
    if let Some(slug) = model {
        params.insert("model".into(), Value::String(slug));
    }
    // Project the user's saved approval + sandbox preferences onto the
    // session/start envelope so the runtime's StaticPermissionPolicy actually
    // honors them. Unknown values get filtered server-side; absent values let
    // the runtime fall back to its own defaults.
    let cfg = super::config::read_user_config();
    if let Some(mode) = cfg.default_approval_policy.as_deref().map(str::trim) {
        if !mode.is_empty() {
            params.insert("permission_mode".into(), Value::String(mode.to_string()));
        }
    }
    if let Some(sandbox) = cfg.default_sandbox_mode.as_deref().map(str::trim) {
        if !sandbox.is_empty() {
            params.insert(
                "sandbox_mode".into(),
                Value::String(sandbox.to_string()),
            );
        }
    }
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "session/start",
        "params": Value::Object(params),
    });
    let response = runtime.handle_incoming(connection_id, envelope).await;

    // Auto-subscribe this connection to the new session's events so the
    // frontend doesn't race against the first item delta.
    if let Some(session_id) = response
        .as_ref()
        .and_then(|r| r.get("result"))
        .and_then(|r| r.get("session_id"))
        .and_then(|s| s.as_str())
    {
        let subscribe = json!({
            "jsonrpc": "2.0",
            "id": rpc_id(),
            "method": "events/subscribe",
            "params": { "session_id": session_id }
        });
        let _ = runtime.handle_incoming(connection_id, subscribe).await;
    }
    Ok(response)
}

/// Send a user-text turn. `mentions` is an optional list of file paths the
/// user @-referenced in the composer; each becomes a structured Mention input
/// item so the agent can resolve them deterministically (the literal "@path"
/// stays in the visible text for the user's own readability). `skills` is an
/// optional list of skill ids the user picked via slash command; each becomes
/// a structured Skill input item that the runtime resolves into the SKILL.md
/// body and injects ahead of the text.
///
/// Order matters for context priming: skills first (instructions), then
/// mentions (file context), then text (the actual user message).
///
/// Returns the `turn/start` response.
#[tauri::command]
pub async fn agent_send_message(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    session_id: String,
    text: String,
    mentions: Option<Vec<String>>,
    skills: Option<Vec<String>>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let mut input: Vec<Value> = Vec::new();
    if let Some(ids) = skills {
        for id in ids.into_iter().filter(|s| !s.is_empty()) {
            input.push(json!({ "type": "skill", "id": id }));
        }
    }
    if let Some(paths) = mentions {
        // Defense-in-depth: the JS composer only inserts paths that came
        // from search_files (already project-scoped), but the IPC
        // boundary is local-only and a future caller / test could pass
        // anything. Reject paths with embedded NULs or non-absolute paths
        // to prevent leaking arbitrary host files into the model context.
        for p in paths.into_iter().filter(|s| !s.is_empty()) {
            if p.contains('\0') {
                tracing::warn!(path = %p, "agent_send_message: rejected mention with NUL byte");
                continue;
            }
            input.push(json!({ "type": "mention", "path": p }));
        }
    }
    input.push(json!({ "type": "text", "text": text }));

    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "turn/start",
        "params": {
            "session_id": session_id,
            "input": input,
        }
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// Cancel the active turn for a session.
#[tauri::command]
pub async fn agent_interrupt_turn(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    session_id: String,
    turn_id: Option<String>,
    reason: Option<String>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let mut params = serde_json::Map::new();
    params.insert("session_id".into(), Value::String(session_id));
    if let Some(tid) = turn_id {
        params.insert("turn_id".into(), Value::String(tid));
    }
    if let Some(r) = reason {
        params.insert("reason".into(), Value::String(r));
    }
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "turn/interrupt",
        "params": Value::Object(params),
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// Approve / deny / cancel a pending tool-call approval request.
#[tauri::command]
pub async fn agent_approve(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    session_id: String,
    turn_id: Option<String>,
    approval_id: String,
    decision: String,
    scope: Option<String>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    // ApprovalRespondParams requires both session_id and turn_id. Omitting
    // turn_id makes the params fail to deserialize → handler returns an
    // InvalidParams error → the orchestrator's oneshot never resolves →
    // the agent hangs forever. The frontend now passes tab.turnId, but we
    // also include turn_id only when supplied to keep the surface clean.
    let mut params = serde_json::Map::new();
    params.insert("session_id".into(), Value::String(session_id));
    if let Some(tid) = turn_id.filter(|s| !s.is_empty()) {
        params.insert("turn_id".into(), Value::String(tid));
    }
    params.insert("approval_id".into(), Value::String(approval_id));
    params.insert("decision".into(), Value::String(decision));
    params.insert(
        "scope".into(),
        Value::String(scope.unwrap_or_else(|| "once".to_string())),
    );
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "approval/respond",
        "params": Value::Object(params),
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// Subscribe this connection to the session's event stream. Required to
/// receive item / delta / approval notifications on `agent:event:<window>`.
#[tauri::command]
pub async fn agent_subscribe_events(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    session_id: String,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "events/subscribe",
        "params": { "session_id": session_id }
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// List sessions known to the runtime (persisted + live).
#[tauri::command]
pub async fn agent_session_list(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "session/list",
        "params": {}
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// Resume a previously persisted session against this window's connection.
#[tauri::command]
pub async fn agent_session_resume(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    session_id: String,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "session/resume",
        "params": { "session_id": session_id }
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

/// List skills (slash commands + workspace skills) discovered by the runtime.
/// `project_path`, when supplied, is forwarded as `cwd` so the catalog can
/// also walk the project's per-workspace skill roots (default: `<cwd>/skills`).
#[tauri::command]
pub async fn agent_skills_list(
    app: AppHandle,
    window: Window,
    state: State<'_, AgentState>,
    project_path: Option<String>,
) -> Result<Option<Value>, String> {
    let connection_id = require_connection_id(&state, &app, window.label())
        .await
        .map_err(err)?;
    let runtime = state.runtime(&app).await.map_err(err)?;

    let mut params = serde_json::Map::new();
    if let Some(p) = project_path.filter(|s| !s.is_empty()) {
        params.insert("cwd".into(), Value::String(p));
    }
    let envelope = json!({
        "jsonrpc": "2.0",
        "id": rpc_id(),
        "method": "skills/list",
        "params": Value::Object(params),
    });
    Ok(runtime.handle_incoming(connection_id, envelope).await)
}

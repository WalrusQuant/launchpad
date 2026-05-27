//! Persistent agent provider settings stored at
//! `~/.launchpad/agent-config.json` (chmod 0o600). Translates to the LPA_*
//! env vars that `load_server_provider` already reads.
//!
//! Schema:
//!     { "provider": "anthropic" | "openai" | "google" | "openai_compatible",
//!       "wire_api": "openai_chat_completions" | "anthropic_messages" | ...,
//!       "model": "claude-sonnet-4-...",
//!       "base_url": "https://...",       // optional
//!       "api_key": "sk-..."              // chmod 0o600 protects this
//!     }

use serde::{Deserialize, Serialize};

use crate::{atomic_write_with_mode, launchpad_dir};

/// User-facing permission rule stored in `agent-config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRuleConfig {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    pub decision: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Default approval policy applied to every new session.
    /// Values: `"auto-approve" | "interactive" | "deny"`. Forwarded as
    /// `permission_mode` on `session/start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_approval_policy: Option<String>,
    /// Default sandbox mode applied to every new session.
    /// Values: `"unrestricted" | "workspace-write" | "read-only"`. Forwarded as
    /// `sandbox_mode` on `session/start`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_sandbox_mode: Option<String>,
    /// Per-tool permission rules. Evaluated before the mode fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_rules: Option<Vec<PermissionRuleConfig>>,
}

fn config_path() -> std::path::PathBuf {
    launchpad_dir().join("agent-config.json")
}

pub fn read_user_config() -> AgentConfig {
    let path = config_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return AgentConfig::default();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

pub fn write_user_config(cfg: &AgentConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    atomic_write_with_mode(&path, body.as_bytes(), Some(0o600)).map_err(|e| e.to_string())?;
    Ok(())
}

/// One-time initialization: fills in safe defaults for `default_approval_policy`
/// and `default_sandbox_mode` when they're missing AND persists them to disk.
///
/// Called once at app startup from `AgentState::new()`. The previous design
/// (`apply_first_run_defaults` applied at every load/save) had a load/save
/// asymmetry: load returned defaults to the UI but never wrote them, so
/// `agent_session_start` (which reads disk directly) saw `None` and made the
/// runtime fall back to `AutoApprove` — misaligned with the UI showing
/// "Interactive". A single persisted initialization makes the on-disk file
/// the single source of truth.
pub fn initialize_defaults_if_missing() {
    let mut cfg = read_user_config();
    let mut changed = false;
    if cfg.default_approval_policy.is_none() {
        cfg.default_approval_policy = Some("interactive".to_string());
        changed = true;
    }
    if cfg.default_sandbox_mode.is_none() {
        cfg.default_sandbox_mode = Some("workspace-write".to_string());
        changed = true;
    }
    if changed {
        if let Err(err) = write_user_config(&cfg) {
            // Best effort — log but don't crash. The runtime falls back to
            // SessionConfig::default() (Interactive + no sandbox) if disk
            // can't be written.
            tracing::warn!(?err, "failed to persist initial agent defaults");
        }
    }
}

/// Process-wide lock serializing writes and reads of the LPA_* env vars.
/// `apply_env_from_user_config` (writer) and `agent_is_configured` (reader)
/// both take this so `std::env::set_var` can never overlap with
/// `std::env::var` from another thread — that overlap is undefined behavior
/// on POSIX (the `environ` pointer isn't atomic).
///
/// Held only across sync env operations; never across an await. Callers
/// must not invoke async work while holding it.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read the on-disk config and set the LPA_* env vars that
/// `lpa_server::load_server_provider` looks at. Always overwrites — so a
/// reload after the user changes settings actually picks up the new values.
/// (If we deferred to existing env, a saved-then-reloaded provider would
/// silently keep the first run's values.)
///
/// Returns a guard that callers can keep alive across the immediately
/// following sync env reads (e.g. `load_server_provider`) so a concurrent
/// `agent_is_configured` can't see a half-written state. Drop the guard
/// before any `.await` — `std::sync::MutexGuard` is not async-safe.
#[must_use]
pub fn apply_env_from_user_config<'a>() -> std::sync::MutexGuard<'a, ()> {
    let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let cfg = read_user_config();
    apply_or_unset("LPA_PROVIDER", cfg.provider.as_deref());
    apply_or_unset("LPA_WIRE_API", cfg.wire_api.as_deref());
    apply_or_unset("LPA_MODEL", cfg.model.as_deref());
    apply_or_unset("LPA_BASE_URL", cfg.base_url.as_deref());
    apply_or_unset("LPA_API_KEY", cfg.api_key.as_deref());
    guard
}

fn apply_or_unset(key: &str, value: Option<&str>) {
    // Safety: ENV_LOCK is held by every Launchpad code path that touches
    // these vars (writers via apply_env_from_user_config, readers via
    // agent_is_configured). The remaining theoretical race is with
    // load_server_provider's own env reads — callers keep the guard alive
    // across that call so it observes the same lock.
    unsafe {
        match value.map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

// ─── Tauri commands ──────────────────────────────────────────────────────

#[tauri::command]
pub fn agent_config_load() -> Result<AgentConfig, String> {
    // Disk is the source of truth. Defaults are written once at app startup
    // by `initialize_defaults_if_missing()` (called from `AgentState::new`),
    // so by the time this fires the file already holds the right values.
    Ok(read_user_config())
}

#[tauri::command]
pub fn agent_config_save(cfg: AgentConfig) -> Result<(), String> {
    write_user_config(&cfg)
}

/// Pre-flight check used by the chat tab to decide whether to show the
/// first-run empty-state CTA or attempt to boot a real session.
///
/// Returns `true` when *both* a provider is selected *and* the runtime has
/// some way to authenticate against it: either an api_key in our config,
/// one of the preset's documented env vars set in the process env, or the
/// preset legitimately needs no key at all (Ollama).
///
/// Cheap — just file read + env lookup, no runtime construction.
#[tauri::command]
pub fn agent_is_configured() -> bool {
    let cfg = read_user_config();
    let Some(provider) = cfg.provider.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    if cfg.api_key.as_deref().map(str::trim).is_some_and(|s| !s.is_empty()) {
        return true;
    }
    // Match the provider against a preset to learn which env vars count as
    // valid auth. Unknown provider id (user typed something custom) → fall
    // back to LPA_API_KEY.
    let env_vars: Vec<&str> = match lpa_core::preset_by_id(provider) {
        Some(p) if p.api_key_env_vars.is_empty() => return true, // e.g. Ollama
        Some(p) => p.api_key_env_vars.to_vec(),
        None => vec!["LPA_API_KEY"],
    };
    // Serialize against apply_env_from_user_config — std::env::set_var
    // racing with std::env::var is UB on POSIX.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    env_vars
        .iter()
        .any(|k| std::env::var(k).is_ok_and(|v| !v.trim().is_empty()))
}

/// Curated provider presets from `lpa-core`. Each entry has the wire API
/// it speaks, a default base URL, and a list of env var names to look at.
/// The frontend uses this to populate the Provider dropdown in the settings
/// panel so users don't have to remember base URLs.
#[derive(serde::Serialize)]
pub struct ProviderPresetView {
    pub id: &'static str,
    pub display_name: &'static str,
    pub wire_api: &'static str,
    pub default_base_url: Option<&'static str>,
    pub api_key_env_vars: Vec<&'static str>,
    pub description: &'static str,
    pub is_custom: bool,
    /// Recommended default model slug for this provider. `None` for the
    /// `custom` preset (where the user must supply their own slug). Used by
    /// the settings UI to auto-fill the model input when the user switches
    /// providers.
    pub default_model: Option<&'static str>,
}

// ─── Per-project agent config overrides ──────────────────────────────────
//
// Stored at `~/.launchpad/project-agent-config.json`, keyed by canonical
// project path (same pattern as project-env.json). Each entry is a sparse
// AgentConfig — only non-None fields override the global config.

type ProjectAgentConfigStore = std::collections::BTreeMap<String, AgentConfig>;

fn project_agent_config_path() -> std::path::PathBuf {
    launchpad_dir().join("project-agent-config.json")
}

fn read_project_agent_store() -> ProjectAgentConfigStore {
    let path = project_agent_config_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return ProjectAgentConfigStore::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

fn write_project_agent_store(store: &ProjectAgentConfigStore) -> Result<(), String> {
    let path = project_agent_config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    atomic_write_with_mode(&path, body.as_bytes(), Some(0o600)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Merge global + per-project config. Project overrides win field-by-field
/// for non-None fields. Permission rules are concatenated (project first).
pub fn merged_config_for_project(project_path: Option<&str>) -> AgentConfig {
    let global = read_user_config();
    let project_override = project_path
        .map(crate::normalize_project_path)
        .and_then(|key| read_project_agent_store().remove(&key));
    let Some(proj) = project_override else {
        return global;
    };
    AgentConfig {
        provider: proj.provider.or(global.provider),
        wire_api: proj.wire_api.or(global.wire_api),
        model: proj.model.or(global.model),
        base_url: proj.base_url.or(global.base_url),
        api_key: proj.api_key.or(global.api_key),
        default_approval_policy: proj.default_approval_policy.or(global.default_approval_policy),
        default_sandbox_mode: proj.default_sandbox_mode.or(global.default_sandbox_mode),
        permission_rules: match (proj.permission_rules, global.permission_rules) {
            (Some(mut p), Some(g)) => {
                p.extend(g);
                Some(p)
            }
            (p, g) => p.or(g),
        },
    }
}

#[tauri::command]
pub fn agent_project_config_load(project_path: String) -> Result<AgentConfig, String> {
    let key = crate::normalize_project_path(&project_path);
    let store = read_project_agent_store();
    Ok(store.get(&key).cloned().unwrap_or_default())
}

#[tauri::command]
pub fn agent_project_config_save(project_path: String, cfg: AgentConfig) -> Result<(), String> {
    let key = crate::normalize_project_path(&project_path);
    let mut store = read_project_agent_store();
    store.insert(key, cfg);
    write_project_agent_store(&store)
}

#[tauri::command]
pub fn agent_project_config_clear(project_path: String) -> Result<(), String> {
    let key = crate::normalize_project_path(&project_path);
    let mut store = read_project_agent_store();
    store.remove(&key);
    write_project_agent_store(&store)
}

// ─── Session deletion ────────────────────────────────────────────────────
//
// The runtime doesn't currently expose a "delete this session" primitive,
// so we delete the underlying rollout file(s) on disk AND maintain a local
// hide-list of session_ids so the deleted session disappears from the UI
// immediately. The runtime's in-memory map still has the entry until the
// next reload, but the frontend filters it out.

fn deleted_sessions_path() -> std::path::PathBuf {
    launchpad_dir().join("agent-deleted-sessions.json")
}

fn agent_data_root() -> std::path::PathBuf {
    // Mirrors lpa_utils::FileSystemConfigPathResolver — find_lpa_home reads
    // LPA_HOME / ~/.lpagent. We can't link the helper through here, so
    // recompute it the same way for consistency.
    if let Some(p) = std::env::var_os("LPA_HOME") {
        return std::path::PathBuf::from(p);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return std::path::PathBuf::from(home).join(".lpagent");
    }
    std::path::PathBuf::from(".lpagent")
}

fn read_deleted_set() -> std::collections::HashSet<String> {
    let path = deleted_sessions_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return std::collections::HashSet::new();
    };
    serde_json::from_str::<Vec<String>>(&data)
        .map(|v| v.into_iter().collect())
        .unwrap_or_default()
}

fn write_deleted_set(set: &std::collections::HashSet<String>) -> Result<(), String> {
    let path = deleted_sessions_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut list: Vec<&String> = set.iter().collect();
    list.sort();
    let body = serde_json::to_string_pretty(&list).map_err(|e| e.to_string())?;
    crate::atomic_write_with_mode(&path, body.as_bytes(), Some(0o600))
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn agent_session_deleted_ids() -> Vec<String> {
    let mut v: Vec<String> = read_deleted_set().into_iter().collect();
    v.sort();
    v
}

/// Delete a session: remove every rollout file whose name contains the
/// session_id and add the session_id to the hide-list. Idempotent.
///
/// `session_id` MUST look like a UUID — that's what the lpa runtime emits.
/// Without this guard, a crafted call like `agent_session_delete(".")`
/// would substring-match every rollout file in the tree.
#[tauri::command]
pub fn agent_session_delete(session_id: String) -> Result<(), String> {
    if !is_uuid_like(&session_id) {
        return Err(format!(
            "invalid session_id: {} (expected UUID-shaped string)",
            session_id
        ));
    }
    let root = agent_data_root().join("sessions");
    if root.exists() {
        // The session_id appears in `rollout-<timestamp>-<session_id>.jsonl`,
        // so a substring match on the filename is enough. We walk the
        // YYYY/MM/DD partitions; depth is bounded.
        if let Err(err) = remove_files_with_marker(&root, &session_id) {
            tracing::warn!(?err, "agent_session_delete: filesystem cleanup failed");
        }
    }
    let mut set = read_deleted_set();
    set.insert(session_id);
    write_deleted_set(&set)?;
    Ok(())
}

/// Accepts canonical UUIDs (with or without hyphens, lowercase or uppercase),
/// 32–36 chars, `[0-9a-fA-F-]` only. Strict enough to keep substring matches
/// from sweeping unrelated files; tolerant enough to cover whatever shape
/// future lpa-runtime versions emit.
fn is_uuid_like(s: &str) -> bool {
    let len = s.len();
    if !(32..=36).contains(&len) {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn remove_files_with_marker(dir: &std::path::Path, marker: &str) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let ftype = entry.file_type()?;
        if ftype.is_dir() {
            remove_files_with_marker(&path, marker)?;
        } else if ftype.is_file() {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                if name.contains(marker) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub fn agent_provider_presets() -> Vec<ProviderPresetView> {
    use lpa_core::ProviderWireApi;
    lpa_core::all_presets()
        .iter()
        .map(|p| ProviderPresetView {
            id: p.id,
            display_name: p.display_name,
            wire_api: match p.wire_api {
                ProviderWireApi::AnthropicMessages => "anthropic_messages",
                ProviderWireApi::OpenAIChatCompletions => "openai_chat_completions",
                ProviderWireApi::OpenAIResponses => "openai_responses",
                ProviderWireApi::GoogleGenerateContent => "google_generate_content",
            },
            default_base_url: p.default_base_url,
            api_key_env_vars: p.api_key_env_vars.to_vec(),
            description: p.description,
            is_custom: p.is_custom,
            default_model: p.default_model,
        })
        .collect()
}

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
    /// Default approval policy hint for the frontend; not enforced server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_approval_policy: Option<String>,
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

/// Read the on-disk config and set the LPA_* env vars that
/// `lpa_server::load_server_provider` looks at. Always overwrites — so a
/// reload after the user changes settings actually picks up the new values.
/// (If we deferred to existing env, a saved-then-reloaded provider would
/// silently keep the first run's values.)
pub fn apply_env_from_user_config() {
    let cfg = read_user_config();
    apply_or_unset("LPA_PROVIDER", cfg.provider.as_deref());
    apply_or_unset("LPA_WIRE_API", cfg.wire_api.as_deref());
    apply_or_unset("LPA_MODEL", cfg.model.as_deref());
    apply_or_unset("LPA_BASE_URL", cfg.base_url.as_deref());
    apply_or_unset("LPA_API_KEY", cfg.api_key.as_deref());
}

fn apply_or_unset(key: &str, value: Option<&str>) {
    // Safety: env mutation is safe here because the agent runtime is
    // strictly single-builder under a Mutex — no concurrent thread is
    // reading these vars while we set them.
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
    Ok(read_user_config())
}

#[tauri::command]
pub fn agent_config_save(cfg: AgentConfig) -> Result<(), String> {
    write_user_config(&cfg)
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
#[tauri::command]
pub fn agent_session_delete(session_id: String) -> Result<(), String> {
    if session_id.trim().is_empty() {
        return Err("missing session_id".into());
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
        })
        .collect()
}

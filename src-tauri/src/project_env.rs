// Per-project environment variables.
// Stored at ~/.launchpad/project-env.json, keyed by canonicalized project path.
// Injected into every PTY spawned for the active project (see spawn_pty).
// File is written atomically and chmod'd to 0o600 so other users can't read it.
use crate::*;
use serde::Serialize;
use std::fs;
use tauri::State;

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct ProjectEnvVar {
    key: String,
    value: String,
    secret: bool,
}

type ProjectEnvStore = std::collections::BTreeMap<String, Vec<ProjectEnvVar>>;

fn project_env_path() -> std::path::PathBuf {
    launchpad_dir().join("project-env.json")
}

fn read_project_env_file() -> Result<ProjectEnvStore, String> {
    let path = project_env_path();
    if !path.exists() {
        return Ok(ProjectEnvStore::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if data.trim().is_empty() {
        return Ok(ProjectEnvStore::new());
    }
    serde_json::from_str::<ProjectEnvStore>(&data)
        .map_err(|e| format!("Invalid project-env.json: {}", e))
}

fn write_project_env_file(store: &ProjectEnvStore) -> Result<(), String> {
    let path = project_env_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(store)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    // 0o600 applied to the temp file BEFORE rename so there is no
    // window where this file (which holds user secrets) exists at the
    // default umask 0o644.
    atomic_write_with_mode(&path, body.as_bytes(), Some(0o600))
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Best-effort lookup of env vars for a given project path. Swallows all errors
// and returns an empty vec — a broken env file must never break terminal spawn.
pub(crate) fn load_env_for_project(path: &str) -> Vec<(String, String)> {
    let Ok(store) = read_project_env_file() else { return Vec::new(); };
    let key = normalize_project_path(path);
    store
        .get(&key)
        .map(|vars| vars.iter().map(|v| (v.key.clone(), v.value.clone())).collect())
        .unwrap_or_default()
}

pub(crate) fn is_valid_env_key(key: &str) -> bool {
    // POSIX env var name: [A-Za-z_][A-Za-z0-9_]*
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[tauri::command]
pub(crate) fn load_project_env_vars(path: String) -> Result<Vec<ProjectEnvVar>, String> {
    let store = read_project_env_file()?;
    let key = normalize_project_path(&path);
    Ok(store.get(&key).cloned().unwrap_or_default())
}

// Read-modify-write of project-env.json. The caller MUST hold
// project_env_file_lock — these do no locking themselves so that
// remove_project (which already needs to mutate env under the lock) and the
// command wrappers share one implementation.
pub(crate) fn save_project_env_vars_locked(path: &str, vars: Vec<ProjectEnvVar>) -> Result<(), String> {
    let key = normalize_project_path(path);
    let mut store = read_project_env_file().unwrap_or_default();
    if vars.is_empty() {
        store.remove(&key);
    } else {
        store.insert(key, vars);
    }
    write_project_env_file(&store)
}

pub(crate) fn forget_project_env_locked(path: &str) -> Result<(), String> {
    let key = normalize_project_path(path);
    let mut store = match read_project_env_file() {
        Ok(s) => s,
        // Missing / unparseable file → nothing to forget. Don't propagate.
        Err(_) => return Ok(()),
    };
    if store.remove(&key).is_some() {
        write_project_env_file(&store)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn save_project_env_vars(
    path: String,
    vars: Vec<ProjectEnvVar>,
    state: State<AppState>,
) -> Result<(), String> {
    for v in &vars {
        if !is_valid_env_key(&v.key) {
            return Err(format!(
                "Invalid env var name: {:?} (must match [A-Za-z_][A-Za-z0-9_]*)",
                v.key
            ));
        }
    }
    // Hold the lock across read→mutate→write so two windows saving env for
    // different projects can't interleave and drop one's entire secret set.
    let _guard = state.project_env_file_lock.lock_safe();
    save_project_env_vars_locked(&path, vars)
}

#[tauri::command]
pub(crate) fn forget_project_env(path: String, state: State<AppState>) -> Result<(), String> {
    let _guard = state.project_env_file_lock.lock_safe();
    forget_project_env_locked(&path)
}

// Recent-projects registry: projects.json under ~/.launchpad. Each entry is a
// root directory the user has explicitly opened. Reads and writes are
// serialized by AppState::projects_file_lock so concurrent windows can't lose
// updates; the file is committed via the shared atomic_write helper.
use crate::*;
use serde::Serialize;
use std::fs;
use tauri::State;

#[derive(Clone, Serialize, serde::Deserialize)]
pub(crate) struct Project {
    name: String,
    path: String,
    #[serde(rename = "lastOpened")]
    last_opened: String,
}

fn projects_path() -> std::path::PathBuf {
    launchpad_dir().join("projects.json")
}

pub(crate) fn read_projects_file() -> Result<Vec<Project>, String> {
    let path = projects_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if data.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Project>>(&data)
        .map_err(|e| format!("Invalid projects.json: {}", e))
}

pub(crate) fn write_projects_file(projects: &[Project]) -> Result<(), String> {
    let path = projects_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(projects)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    atomic_write(&path, body.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn normalize_project_path(path: &str) -> String {
    // Canonicalize when possible so two different strings for the same dir dedupe.
    // Fall back to the raw string if the path doesn't resolve (missing dir).
    match fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.trim_end_matches('/').to_string(),
    }
}

#[tauri::command]
pub(crate) fn load_projects() -> Result<Vec<Project>, String> {
    let mut projects = read_projects_file()?;
    // Sort by lastOpened desc so recent projects come first.
    projects.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
    Ok(projects)
}

#[tauri::command]
pub(crate) fn add_project(
    path: String,
    name: Option<String>,
    last_opened: String,
    state: State<AppState>,
) -> Result<Project, String> {
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;

    let normalized = normalize_project_path(&path);
    let derived_name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        std::path::Path::new(&normalized)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| normalized.clone())
    });

    let mut projects = read_projects_file()?;
    if let Some(existing) = projects
        .iter_mut()
        .find(|p| normalize_project_path(&p.path) == normalized)
    {
        existing.last_opened = last_opened;
        let out = existing.clone();
        write_projects_file(&projects)?;
        return Ok(out);
    }

    let project = Project {
        name: derived_name,
        path: normalized,
        last_opened,
    };
    projects.push(project.clone());
    write_projects_file(&projects)?;
    Ok(project)
}

#[tauri::command]
pub(crate) fn remove_project(path: String, state: State<AppState>) -> Result<(), String> {
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;
    let target = normalize_project_path(&path);
    let mut projects = read_projects_file()?;
    projects.retain(|p| normalize_project_path(&p.path) != target);
    write_projects_file(&projects)?;
    // Best-effort: drop stored env vars for this project too. Deleting a project
    // is an explicit act; leaving the secrets behind would be a privacy leak.
    // Acquire the env-file lock for the read-modify-write (distinct file, distinct
    // lock). Ordering is always projects_file_lock → project_env_file_lock here
    // and nowhere the reverse, so no deadlock.
    {
        let _eguard = state.project_env_file_lock.lock_safe();
        let _ = crate::forget_project_env_locked(&path);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn rename_project(
    path: String,
    new_name: String,
    state: State<AppState>,
) -> Result<(), String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("Name cannot be empty".into());
    }
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;
    let target = normalize_project_path(&path);
    let mut projects = read_projects_file()?;
    if let Some(p) = projects
        .iter_mut()
        .find(|p| normalize_project_path(&p.path) == target)
    {
        p.name = trimmed.to_string();
        write_projects_file(&projects)?;
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn touch_project(
    path: String,
    last_opened: String,
    state: State<AppState>,
) -> Result<(), String> {
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;
    let target = normalize_project_path(&path);
    let mut projects = read_projects_file()?;
    if let Some(p) = projects
        .iter_mut()
        .find(|p| normalize_project_path(&p.path) == target)
    {
        p.last_opened = last_opened;
        write_projects_file(&projects)?;
    }
    Ok(())
}

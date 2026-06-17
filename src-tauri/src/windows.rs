// Multi-window management. A window IS a project; this module opens new
// windows and maintains the canonical-path → window-label routing table
// (AppState::project_windows) used to focus an existing project window
// instead of opening a duplicate.
use crate::*;
use tauri::{Manager, State};

#[tauri::command]
pub(crate) fn open_new_window(path: Option<String>, app: tauri::AppHandle) -> Result<(), String> {
    let label = format!(
        "window-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    // Project windows boot with ?folder=; bare windows land on the picker.
    let url = match &path {
        Some(p) => format!("index.html?folder={}", urlencoding::encode(p)),
        None => "index.html".to_string(),
    };

    let title = path
        .as_deref()
        .and_then(|p| p.rsplit('/').next())
        .unwrap_or("Launchpad");

    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(1200.0, 800.0)
        .min_inner_size(800.0, 500.0)
        .build()
        .map_err(|e| e.to_string())?;

    // Registration is handled by the frontend when `enterWorkspace` runs, so
    // both "new window with ?folder=" and "current window takes over a project"
    // register through the same path.
    Ok(())
}

#[tauri::command]
pub(crate) fn register_project_window(
    path: String,
    label: String,
    state: State<AppState>,
) -> Result<(), String> {
    let canonical = normalize_project_path(&path);
    if let Ok(mut map) = state.project_windows.lock() {
        // Drop any stale entry that was registered under this same label (e.g. a
        // different project previously held this window) so a label only points
        // at one project at a time.
        map.retain(|_, v| v != &label);
        map.insert(canonical, label);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn unregister_project_window(
    path: String,
    state: State<AppState>,
) -> Result<(), String> {
    let canonical = normalize_project_path(&path);
    if let Ok(mut map) = state.project_windows.lock() {
        map.remove(&canonical);
    }
    Ok(())
}

/// Remove every entry in the map whose value equals `label`. Called when a
/// window enters picker mode so a stale registration (from Cmd+R, a crash,
/// or any non-"← Projects" path) doesn't make focus_project_window think
/// this window is still hosting a project.
#[tauri::command]
pub(crate) fn unregister_window_label(
    label: String,
    state: State<AppState>,
) -> Result<(), String> {
    if let Ok(mut map) = state.project_windows.lock() {
        map.retain(|_, v| v != &label);
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn focus_project_window(
    path: String,
    app: tauri::AppHandle,
    state: State<AppState>,
) -> Result<bool, String> {
    let canonical = normalize_project_path(&path);
    // Single critical section so no other thread can insert a fresh entry
    // between our read and the stale-remove in the `None` branch.
    let mut map = state
        .project_windows
        .lock()
        .map_err(|e| format!("project_windows lock poisoned: {}", e))?;
    let Some(label) = map.get(&canonical).cloned() else {
        return Ok(false);
    };

    match app.get_webview_window(&label) {
        Some(win) => {
            // show() first in case the window is hidden/minimized; focus() alone doesn't restore.
            let _ = win.show();
            win.set_focus().map_err(|e| e.to_string())?;
            Ok(true)
        }
        None => {
            // Stale entry — the window was closed. Drop it so the caller can open fresh.
            map.remove(&canonical);
            Ok(false)
        }
    }
}

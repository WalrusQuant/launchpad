// Live filesystem watcher. Watches a project root recursively via the notify
// crate (FSEvents on macOS) with a 300ms debounce, filters out churn from
// build/VCS dirs, and emits fs-changed events scoped to the canonical root.
use crate::*;
use notify_debouncer_mini::{new_debouncer, notify};
use std::fs;
use std::time::Duration;
use tauri::{Emitter, State};

// Names of directories whose internal churn should never trigger a UI
// refresh — `git checkout`, `npm install`, `cargo build`, etc. produce
// thousands of events inside these that are pure noise from the workspace's
// perspective. Same skip list as `search_files` / `find_path_by_inode` so
// "what the user sees" stays consistent across the file browser, search,
// and live refresh.
const FS_NOISE_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    "dist",
    "build",
];

// Returns true when `event_path` should be ignored — i.e. it's inside one
// of `FS_NOISE_DIRS` *relative to the watched root*. Components above the
// root are not consulted (a user whose home dir happens to be named
// `target` shouldn't have every event filtered out).
pub(crate) fn fs_event_is_noise(event_path: &std::path::Path, root: &std::path::Path) -> bool {
    let relative = match event_path.strip_prefix(root) {
        Ok(r) => r,
        // Outside the watched root — let it through; it shouldn't reach us
        // anyway, but better to surface than to silently drop.
        Err(_) => return false,
    };
    relative.components().any(|c| match c {
        std::path::Component::Normal(name) => {
            let s = name.to_string_lossy();
            FS_NOISE_DIRS.iter().any(|noise| s == *noise)
        }
        _ => false,
    })
}

#[tauri::command]
pub(crate) fn watch_directory(
    path: String,
    state: State<AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    // Canonicalize so the stored key and the emitted event path are stable
    // regardless of symlink vs real-path or trailing-slash variations the
    // caller happened to pass in. Returns the canonical form so the
    // frontend can store it and compare fs-changed events against the
    // exact same string.
    let canonical = fs::canonicalize(&path)
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .to_string();

    let emit_path = canonical.clone();
    let root_path = std::path::PathBuf::from(&canonical);
    let handle = app.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(300),
        move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = events {
                // Filter out noise (.git/, node_modules/, target/, ...) so a
                // `git checkout`, `npm install`, or `cargo build` doesn't
                // trigger a full UI refresh per batch. Also dedupes paths so
                // a touch+modify+rename of the same file shows once.
                let mut seen = std::collections::HashSet::new();
                let changed_paths: Vec<String> = events
                    .iter()
                    .filter(|e| !fs_event_is_noise(&e.path, &root_path))
                    .filter_map(|e| {
                        let s = e.path.to_string_lossy().to_string();
                        if seen.insert(s.clone()) { Some(s) } else { None }
                    })
                    .collect();
                if !changed_paths.is_empty() {
                    let _ = handle.emit(
                        events::FS_CHANGED,
                        FsChanged {
                            path: emit_path.clone(),
                            changed_paths,
                        },
                    );
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;

    debouncer
        .watcher()
        .watch(
            std::path::Path::new(&canonical),
            notify::RecursiveMode::Recursive,
        )
        .map_err(|e| e.to_string())?;

    // Insert into the map, replacing any existing watcher for this path (dropping it stops the previous watch)
    let mut guard = state
        .fs_watcher
        .lock()
        .map_err(|e: std::sync::PoisonError<_>| e.to_string())?;
    guard.remove(&canonical);
    guard.insert(canonical.clone(), debouncer);
    Ok(canonical)
}

#[tauri::command]
pub(crate) fn unwatch_directory(path: String, state: State<AppState>) -> Result<(), String> {
    // Try both the raw path and its canonical form — callers may hold
    // either (watch_directory returns the canonical version, but a teardown
    // path built from the original project.path may still use the raw one).
    let mut guard = state.fs_watcher.lock().map_err(|e| e.to_string())?;
    guard.remove(&path);
    if let Ok(canonical) = fs::canonicalize(&path) {
        guard.remove(&canonical.to_string_lossy().to_string());
    }
    Ok(())
}

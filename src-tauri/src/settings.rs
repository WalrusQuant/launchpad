// Persistent settings: config.json (app) and file-settings.json (per-file
// editor prefs), both under ~/.launchpad. JSON is validated and normalized
// before every write, then committed via the shared atomic_write helper.
use crate::{atomic_write, launchpad_dir};
use std::fs;

fn config_path() -> std::path::PathBuf {
    launchpad_dir().join("config.json")
}

#[tauri::command]
pub(crate) fn load_settings() -> Result<String, String> {
    let path = config_path();
    if path.exists() {
        fs::read_to_string(&path).map_err(|e| e.to_string())
    } else {
        Ok("{}".to_string())
    }
}

#[tauri::command]
pub(crate) fn save_settings(data: String) -> Result<(), String> {
    // Validate JSON before writing to prevent config corruption
    let parsed: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("Invalid JSON: {}", e))?;
    let normalized = serde_json::to_string_pretty(&parsed)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(&path, normalized.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

fn file_settings_path() -> std::path::PathBuf {
    launchpad_dir().join("file-settings.json")
}

#[tauri::command]
pub(crate) fn load_file_settings() -> Result<String, String> {
    let path = file_settings_path();
    if path.exists() {
        fs::read_to_string(&path).map_err(|e| e.to_string())
    } else {
        Ok("{}".to_string())
    }
}

#[tauri::command]
pub(crate) fn save_file_settings(data: String) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(&data).map_err(|e| format!("Invalid JSON: {}", e))?;
    let normalized = serde_json::to_string_pretty(&parsed)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    let path = file_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(&path, normalized.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

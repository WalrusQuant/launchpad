use crate::*;
use serde::Serialize;

#[tauri::command]
pub(crate) async fn git_stash_save(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let sig = repo.signature().map_err(|e| e.to_string())?;
        // stash_save requires &mut Repository
        let mut repo = repo;
        repo.stash_save(&sig, "Launchpad stash", None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_pop(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        repo.stash_pop(0, None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[derive(Clone, Serialize)]
pub(crate) struct StashEntry {
    pub(crate) index: usize,
    pub(crate) message: String,
}

#[tauri::command]
pub(crate) async fn git_stash_list(path: String) -> Result<Vec<StashEntry>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        let mut entries = Vec::new();
        repo.stash_foreach(|index, message, _oid| {
            entries.push(StashEntry {
                index,
                message: message.to_string(),
            });
            true
        }).map_err(|e| e.to_string())?;
        Ok(entries)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_apply(path: String, index: usize) -> Result<(), String> {
    blocking(move || {
        let mut repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        repo.stash_apply(index, None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stash_drop(path: String, index: usize) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        repo.stash_drop(index)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

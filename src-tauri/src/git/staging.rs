use crate::*;

#[tauri::command]
pub(crate) async fn git_stage_all(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut index = repo.index().map_err(|e| e.to_string())?;
        index
            .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
            .map_err(|e| e.to_string())?;
        index.write().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_stage_file(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut index = repo.index().map_err(|e| e.to_string())?;
        index
            .add_path(std::path::Path::new(&file_path))
            .map_err(|e| e.to_string())?;
        index.write().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_unstage_file(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        match repo.head() {
            Ok(head) => {
                let head_commit = head.peel_to_commit().map_err(|e| e.to_string())?;
                let obj = head_commit.as_object();
                repo.reset_default(Some(obj), [file_path.as_str()])
                    .map_err(|e| e.to_string())?;
            }
            Err(_) => {
                // Unborn HEAD (no commits yet) — remove from index directly
                let mut index = repo.index().map_err(|e| e.to_string())?;
                index.remove_path(std::path::Path::new(&file_path)).map_err(|e| e.to_string())?;
                index.write().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_unstage_all(path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        match repo.head() {
            Ok(head) => {
                let head_commit = head.peel_to_commit().map_err(|e| e.to_string())?;
                let obj = head_commit.as_object();
                repo.reset_default(Some(obj), ["*"])
                    .map_err(|e| e.to_string())?;
            }
            Err(_) => {
                // Unborn HEAD (no commits yet) — clear the entire index
                let mut index = repo.index().map_err(|e| e.to_string())?;
                index.clear().map_err(|e| e.to_string())?;
                index.write().map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_discard_file(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let workdir = repo.workdir().ok_or("Repository has no workdir")?;
        let abs = workdir.join(&file_path);

        // Two cases route to "delete from disk":
        //   1) status_file says wt_new (untracked file at the top level).
        //   2) status_file errors with GIT_ENOTFOUND because the path lives
        //      inside an untracked directory — git reports the directory,
        //      not its contents, so status_file can't see the child. Fall
        //      back to a presence check on disk, gated on the NotFound
        //      error code so we don't misclassify other libgit2 errors.
        let status_result = repo.status_file(std::path::Path::new(&file_path));
        let treat_as_untracked = match &status_result {
            Ok(s) => s.is_wt_new(),
            Err(e) if e.code() == git2::ErrorCode::NotFound => abs.exists(),
            Err(_) => false,
        };

        if treat_as_untracked {
            let meta = std::fs::metadata(&abs).map_err(|e| e.to_string())?;
            if meta.is_dir() {
                std::fs::remove_dir_all(&abs).map_err(|e| e.to_string())?;
            } else {
                std::fs::remove_file(&abs).map_err(|e| e.to_string())?;
            }
            return Ok(());
        }

        // Surface any non-NotFound status_file error rather than silently
        // falling through to checkout_head, which would just fail again.
        status_result.map_err(|e| e.to_string())?;

        let mut checkout = git2::build::CheckoutBuilder::new();
        checkout.path(&file_path).force();
        repo.checkout_head(Some(&mut checkout))
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

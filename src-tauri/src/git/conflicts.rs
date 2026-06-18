use crate::*;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[tauri::command]
pub(crate) async fn git_merge_branch(
    path: String,
    branch_name: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    if !is_valid_git_ref(&branch_name) {
        return Err("Invalid branch name".into());
    }
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        // Merge can block on network I/O if the user's git config pulls on merge
        // or the operation touches a remote tracking ref. Route it through
        // run_git_cancellable so cancel_git_op can kill it.
        run_git_cancellable(&["merge", &branch_name], &path, &slots, &counter, &label)
    })
    .await
}

// List paths in working tree marked as conflicted by libgit2's status walk.
pub(crate) fn list_conflict_files(path: &str) -> Vec<String> {
    match get_git_status_inner(path) {
        Ok(info) => info.files.into_iter()
            .filter(|f| f.status == "conflict")
            .map(|f| f.path)
            .collect(),
        Err(_) => Vec::new(),
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct ConflictVersions {
    // Each version is None when the corresponding stage is missing (e.g.
    // a file added on both sides has no base; deleted-by-us has no ours).
    // Working tree's `merged` is None when the file isn't on disk.
    pub(crate) base: Option<String>,
    pub(crate) ours: Option<String>,
    pub(crate) theirs: Option<String>,
    pub(crate) merged: Option<String>,
}

// Reads index stages 1 (base), 2 (ours), 3 (theirs) for a conflicted file
// and the working-tree contents (`merged`). Used by the 3-pane merge tab.
// Stages encode their role in libgit2's index: 0 = normal, 1 = base,
// 2 = ours, 3 = theirs. A file may be missing any of 1/2/3 depending on
// which side added/deleted it.
#[tauri::command]
pub(crate) async fn get_conflict_versions(path: String, file_path: String) -> Result<ConflictVersions, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let index = repo.index().map_err(|e| e.to_string())?;

        // file_path arrives project-relative; the index keys paths
        // workdir-relative. They differ by the subdir prefix when the project is
        // a subdirectory of the repo, so prepend it before matching index entries
        // (the working-tree `merged` read below joins the project path + the
        // project-relative file_path, which is already correct).
        let workdir_rel = format!("{}{}", repo_subdir_prefix(&repo, &path), file_path);
        let path_bytes = workdir_rel.as_bytes();
        let mut base = None;
        let mut ours = None;
        let mut theirs = None;

        for entry in index.iter() {
            if entry.path != path_bytes {
                continue;
            }
            // libgit2 packs stage in bits 12-13 of `flags`: GIT_IDXENTRY_STAGEMASK
            // = 0x3000, shift = 12. Stage 0 = normal, 1 = base, 2 = ours, 3 = theirs.
            let stage = ((entry.flags & 0x3000) >> 12) as i32;
            let blob = match repo.find_blob(entry.id) {
                Ok(b) => b,
                Err(_) => continue,
            };
            // Blob content may not be UTF-8 (binary files, mixed encodings);
            // for the merge UI we surface the lossy conversion so the user
            // gets *something* instead of an opaque error. The 3-pane editor
            // is text-only by design.
            let content = String::from_utf8_lossy(blob.content()).into_owned();
            match stage {
                1 => base = Some(content),
                2 => ours = Some(content),
                3 => theirs = Some(content),
                _ => {}
            }
        }

        // Only read the working tree for a contained relative path. A traversal
        // or absolute file_path would have matched no index entry above (libgit2
        // stores normalized repo-relative paths), so base/ours/theirs are
        // already None; we just refuse to leak an out-of-repo file as `merged`.
        let merged = if is_safe_relative_path(&file_path) {
            let abs_path = std::path::Path::new(&path).join(&file_path);
            std::fs::read_to_string(&abs_path).ok()
        } else {
            None
        };

        Ok(ConflictVersions { base, ours, theirs, merged })
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_resolve_ours(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let mut checkout = std::process::Command::new("git");
        checkout
            .args(["checkout", "--ours", "--", &file_path])
            .current_dir(&path);
        apply_git_env(&mut checkout);
        let status = checkout.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to checkout --ours".into());
        }
        let mut add = std::process::Command::new("git");
        add.args(["add", "--", &file_path]).current_dir(&path);
        apply_git_env(&mut add);
        let status = add.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to stage resolved file".into());
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_resolve_theirs(path: String, file_path: String) -> Result<(), String> {
    blocking(move || {
        let mut checkout = std::process::Command::new("git");
        checkout
            .args(["checkout", "--theirs", "--", &file_path])
            .current_dir(&path);
        apply_git_env(&mut checkout);
        let status = checkout.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to checkout --theirs".into());
        }
        let mut add = std::process::Command::new("git");
        add.args(["add", "--", &file_path]).current_dir(&path);
        apply_git_env(&mut add);
        let status = add.status().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Failed to stage resolved file".into());
        }
        Ok(())
    })
    .await
}

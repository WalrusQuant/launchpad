use crate::*;
use serde::Serialize;
use std::sync::Arc;
use tauri::State;

#[derive(Clone, Serialize)]
pub(crate) struct CherryPickResult {
    pub(crate) ok: bool,
    pub(crate) conflicted_files: Vec<String>,
}

#[tauri::command]
pub(crate) async fn git_cherry_pick(
    path: String,
    oid: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<CherryPickResult, String> {
    if !is_valid_git_oid(&oid) {
        return Err("Invalid commit OID".into());
    }
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_cancellable(
            &["cherry-pick", &oid],
            &path,
            &slots,
            &counter,
            &label,
        );
        match result {
            Ok(_) => Ok(CherryPickResult { ok: true, conflicted_files: Vec::new() }),
            Err(stderr) => {
                // Non-zero exit could mean conflict (state file present) OR a
                // real failure (bad oid, repo state, etc.). Distinguish via
                // CHERRY_PICK_HEAD so the frontend can route to the conflict UI.
                let cp_head = git_dir(&path)?.join("CHERRY_PICK_HEAD");
                if cp_head.exists() {
                    Ok(CherryPickResult {
                        ok: false,
                        conflicted_files: list_conflict_files(&path),
                    })
                } else {
                    Err(stderr)
                }
            }
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_cherry_pick_abort(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["cherry-pick", "--abort"], &path, &slots, &counter, &label)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_cherry_pick_continue(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["cherry-pick", "--continue"], &path, &slots, &counter, &label)
    })
    .await
}

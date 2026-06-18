use crate::*;
use serde::Serialize;

#[derive(Clone, Serialize)]
pub(crate) struct PendingOpState {
    pub(crate) kind: String,
    pub(crate) current_step: Option<u32>,
    pub(crate) total_steps: Option<u32>,
    pub(crate) head_message: Option<String>,
}

// Single libgit2-driven query the frontend polls each cycle. Detects the
// three rebase-family operations that leave state files on disk:
//   merge        → MERGE_HEAD
//   cherry-pick  → CHERRY_PICK_HEAD
//   rebase       → rebase-merge/ or rebase-apply/ directory
// Returns "none" when the working tree is clean of pending state. The
// frontend banner is the only consumer.
#[tauri::command]
pub(crate) async fn get_pending_op_state(path: String) -> Result<PendingOpState, String> {
    blocking(move || {
        let none = PendingOpState {
            kind: "none".into(),
            current_step: None,
            total_steps: None,
            head_message: None,
        };
        let gd = match git_dir(&path) {
            Ok(d) => d,
            Err(_) => return Ok(none),
        };

        let head_message = (|| -> Option<String> {
            let repo = Repository::discover(&path).ok()?;
            let head = repo.head().ok()?;
            let commit = head.peel_to_commit().ok()?;
            commit.summary().map(|s| s.to_string())
        })();

        if gd.join("MERGE_HEAD").exists() {
            return Ok(PendingOpState {
                kind: "merge".into(),
                current_step: None,
                total_steps: None,
                head_message,
            });
        }

        if gd.join("CHERRY_PICK_HEAD").exists() {
            return Ok(PendingOpState {
                kind: "cherry_pick".into(),
                current_step: None,
                total_steps: None,
                head_message,
            });
        }

        let rebase_dir = if gd.join("rebase-merge").is_dir() {
            Some(gd.join("rebase-merge"))
        } else if gd.join("rebase-apply").is_dir() {
            Some(gd.join("rebase-apply"))
        } else {
            None
        };

        if let Some(rd) = rebase_dir {
            // rebase-merge/msgnum + end give "interactive rebase" progress.
            // rebase-apply/next + last give "git am"-style rebase progress.
            // Either layout, we read the two text files and parse u32.
            let read_num = |name: &str| -> Option<u32> {
                std::fs::read_to_string(rd.join(name))
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok())
            };
            let (current_step, total_steps) = if rd.file_name() == Some(std::ffi::OsStr::new("rebase-merge")) {
                (read_num("msgnum"), read_num("end"))
            } else {
                (read_num("next"), read_num("last"))
            };
            return Ok(PendingOpState {
                kind: "rebase".into(),
                current_step,
                total_steps,
                head_message,
            });
        }

        Ok(none)
    })
    .await
}

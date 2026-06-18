use crate::*;
use git2::BranchType;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tauri::State;

// ═══════════════════════════════════════════════════════════════════════════
// Interactive rebase (PR4 / Phase 5a)
//
// Driver pattern: we generate two tiny Python scripts in a per-run temp dir
// and point GIT_SEQUENCE_EDITOR / GIT_EDITOR at them. They read the user's
// drag-reordered todo list (state.json) and the per-step counter from the
// state dir, no argv plumbing through git. The state dir + a pre-rebase
// backup tag survive conflict pauses; cleanup is gated on a TERMINAL op
// (clean completion / abort / final continue or skip), not on the initial
// apply returning. See specs/git-features-plan.md Phase 5 for the rationale.
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Clone, Serialize)]
pub(crate) struct RebaseCandidateCommit {
    pub(crate) oid: String,
    pub(crate) short_oid: String,
    pub(crate) message: String,
    pub(crate) author: String,
    pub(crate) timestamp: i64,
    pub(crate) on_remote: bool,
}

#[derive(Clone, Serialize)]
pub(crate) struct RebaseCandidates {
    pub(crate) commits: Vec<RebaseCandidateCommit>,
    pub(crate) upstream_known: bool,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct RebaseTodoEntry {
    pub(crate) action: String, // "pick" | "reword" | "squash" | "fixup" | "drop" | "edit"
    pub(crate) oid: String,
    pub(crate) new_message: Option<String>,
}

#[derive(Clone, Serialize)]
pub(crate) struct RebaseResult {
    pub(crate) ok: bool,
    pub(crate) stopped_at: Option<String>,
    pub(crate) conflicted_files: Vec<String>,
    pub(crate) completed: bool,
    pub(crate) backup_tag: String,
}

pub(crate) const VALID_REBASE_ACTIONS: &[&str] = &["pick", "reword", "squash", "fixup", "drop", "edit"];

#[tauri::command]
pub(crate) async fn get_rebase_candidate_commits(
    path: String,
    count: usize,
) -> Result<RebaseCandidates, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        if matches!(repo.head_detached(), Ok(true)) {
            // Use a recognizable typed prefix so the frontend can show a useful
            // error toast without parsing free-form text.
            return Err("detached_head".into());
        }

        let head = repo.head().map_err(|e| format!("HEAD: {}", e))?;
        let head_oid = head
            .target()
            .ok_or_else(|| "HEAD has no target".to_string())?;

        // Determine the upstream cutoff: walk forward from `head` until we hit
        // a commit reachable from the upstream's tip. That commit (and earlier)
        // are "shared", so we don't include them in the rebase candidate list.
        let upstream_oid = (|| -> Option<git2::Oid> {
            let head_branch_name = head.shorthand()?;
            let local = repo.find_branch(head_branch_name, BranchType::Local).ok()?;
            let upstream = local.upstream().ok()?;
            upstream.get().target()
        })();

        let upstream_known = upstream_oid.is_some();

        // Build an "on any remote ref" set so we can flag commits as on_remote.
        // For typical repos this is a small set; we materialize it once.
        let mut remote_tips: Vec<git2::Oid> = Vec::new();
        if let Ok(refs) = repo.references_glob("refs/remotes/*") {
            for r in refs.flatten() {
                if let Some(t) = r.target() {
                    remote_tips.push(t);
                }
            }
        }

        let mut walk = repo.revwalk().map_err(|e| e.to_string())?;
        walk.push(head_oid).map_err(|e| e.to_string())?;

        let mut commits: Vec<RebaseCandidateCommit> = Vec::new();
        for oid_res in walk.take(count) {
            let oid = match oid_res {
                Ok(o) => o,
                Err(_) => break,
            };
            // Stop walking once we hit the upstream-shared frontier — anything
            // beyond is published history we shouldn't be rewriting silently.
            if let Some(up) = upstream_oid {
                if oid == up {
                    break;
                }
                if matches!(repo.graph_descendant_of(up, oid), Ok(true)) {
                    break;
                }
            }

            let on_remote = remote_tips.iter().any(|tip| {
                *tip == oid || matches!(repo.graph_descendant_of(*tip, oid), Ok(true))
            });

            let commit = repo.find_commit(oid).map_err(|e| e.to_string())?;
            commits.push(RebaseCandidateCommit {
                oid: oid.to_string(),
                short_oid: oid.to_string()[..7].to_string(),
                message: commit.summary().unwrap_or("(no message)").to_string(),
                author: commit.author().name().unwrap_or("Unknown").to_string(),
                timestamp: commit.time().seconds(),
                on_remote,
            });
        }

        Ok(RebaseCandidates { commits, upstream_known })
    })
    .await
}

// Lightweight tag at HEAD before the rebase mutates anything. Tag name is
// timestamp-based so two consecutive rebases don't collide.
pub(crate) fn create_rebase_backup_tag(repo: &Repository) -> Result<String, String> {
    let head_oid = repo
        .head()
        .map_err(|e| format!("HEAD: {}", e))?
        .target()
        .ok_or_else(|| "HEAD has no target".to_string())?;
    let target = repo.find_object(head_oid, None).map_err(|e| e.to_string())?;
    // Nanos avoid the within-same-second collision two consecutive rebases
    // would otherwise hit (tag_lightweight refuses to overwrite without
    // force=true, surfacing as a confusing "backup tag" error).
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tag_name = format!("launchpad/pre-rebase/{}", nanos);
    repo.tag_lightweight(&tag_name, &target, false)
        .map_err(|e| format!("backup tag: {}", e))?;
    Ok(tag_name)
}

pub(crate) fn delete_rebase_backup_tag(repo_path: &str, tag_name: &str) {
    if let Ok(repo) = Repository::discover(repo_path) {
        let _ = repo.tag_delete(tag_name);
    }
}

// Generate the per-run temp dir holding state.json + the two editor scripts.
// Uses tempfile::Builder so the directory name has a secure random suffix
// (no TOCTOU window where a local attacker can pre-create the path) and is
// created atomically. Directory mode is set to 0o700 immediately after
// creation; state.json is written at 0o600 so other users on shared
// machines can't read commit-message contents.
pub(crate) fn create_rebase_state_dir(todo: &[RebaseTodoEntry]) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;

    // tempfile::Builder uses O_EXCL with a securely random suffix — no
    // pre-creation race possible. .into_path() detaches the cleanup guard
    // because we need the directory to outlive this function (cleanup is
    // owned by the rebase command lifecycle in run_git_with_rebase_env's
    // callers; see finalize_rebase).
    let temp = tempfile::Builder::new()
        .prefix("launchpad-rebase-")
        .tempdir()
        .map_err(|e| format!("create state dir: {}", e))?;
    let dir = temp.keep();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod state dir: {}", e))?;

    let state_json = serde_json::json!({
        "version": 1,
        "todo": todo,
    });
    let state_path = dir.join("state.json");
    std::fs::write(
        &state_path,
        serde_json::to_string(&state_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write state.json: {}", e))?;
    std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod state.json: {}", e))?;

    // seq_editor.sh — git invokes this once with $1 = path to the rebase-todo
    // file. It reads our state.json and rewrites $1 in our drag-reordered
    // order, dropping `drop` entries.
    let seq_script = r#"#!/usr/bin/env python3
import json, os, sys
state_dir = os.environ["LAUNCHPAD_REBASE_STATE_DIR"]
with open(os.path.join(state_dir, "state.json")) as f:
    state = json.load(f)
todo_path = sys.argv[1]
with open(todo_path, "w") as out:
    for entry in state["todo"]:
        action = entry["action"]
        if action == "drop":
            continue
        out.write(f"{action} {entry['oid']}\n")
"#;
    let seq_path = dir.join("seq_editor.sh");
    std::fs::write(&seq_path, seq_script).map_err(|e| format!("write seq_editor.sh: {}", e))?;
    std::fs::set_permissions(&seq_path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod seq_editor.sh: {}", e))?;

    // commit_editor.sh — git invokes this once per reword/squash entry with
    // $1 = path to the COMMIT_EDITMSG file. We track which reword/squash
    // we're on via a counter file and write the matching new_message (or
    // leave the file untouched when new_message is null/missing). `edit`
    // does NOT invoke this — git pauses for shell on `edit` and the user
    // resumes via Continue from our UI.
    let commit_script = r#"#!/usr/bin/env python3
import json, os, sys, fcntl
state_dir = os.environ["LAUNCHPAD_REBASE_STATE_DIR"]
with open(os.path.join(state_dir, "state.json")) as f:
    state = json.load(f)

counter_path = os.path.join(state_dir, "commit_editor.counter")
# Atomic open-or-create + flock-protected increment.
fd = os.open(counter_path, os.O_RDWR | os.O_CREAT, 0o600)
try:
    fcntl.flock(fd, fcntl.LOCK_EX)
    raw = os.read(fd, 64).decode().strip()
    n = int(raw) if raw else 0
    os.lseek(fd, 0, os.SEEK_SET)
    os.ftruncate(fd, 0)
    os.write(fd, str(n + 1).encode())
finally:
    fcntl.flock(fd, fcntl.LOCK_UN)
    os.close(fd)

target = None
seen = 0
for entry in state["todo"]:
    if entry["action"] in ("reword", "squash"):
        if seen == n:
            target = entry
            break
        seen += 1

msg_path = sys.argv[1]
if target and target.get("new_message"):
    with open(msg_path, "w") as f:
        f.write(target["new_message"])
        if not target["new_message"].endswith("\n"):
            f.write("\n")
# else: leave file untouched — preserves existing message (git's default)
"#;
    let commit_path = dir.join("commit_editor.sh");
    std::fs::write(&commit_path, commit_script)
        .map_err(|e| format!("write commit_editor.sh: {}", e))?;
    std::fs::set_permissions(&commit_path, std::fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod commit_editor.sh: {}", e))?;

    Ok(dir)
}

pub(crate) fn cleanup_rebase_state_dir(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

// True when git's rebase state files indicate the rebase has finished
// (state files removed). We poll this after each command result to decide
// whether to clean up the state dir + (optionally) the backup tag.
pub(crate) fn rebase_in_progress(repo_path: &str) -> bool {
    let gd = match git_dir(repo_path) {
        Ok(d) => d,
        Err(_) => return false,
    };
    gd.join("rebase-merge").is_dir() || gd.join("rebase-apply").is_dir()
}

#[tauri::command]
pub(crate) async fn git_rebase_interactive_apply(
    path: String,
    base_oid: String,
    todo: Vec<RebaseTodoEntry>,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<RebaseResult, String> {
    // Accept either a literal OID or a rev expression (HEAD^, abc1234~2, a
    // branch name). Validate via the ref/oid grammar (so shell metachars
    // stay out), then revparse_single below to a concrete OID before
    // shelling out. This lets the frontend pass `<oid>^` for "rebase from
    // here" without needing a separate parent-resolution IPC.
    if !is_valid_git_ref(&base_oid) && !is_valid_git_oid(&base_oid) {
        return Err("Invalid base ref".into());
    }
    for entry in &todo {
        if !VALID_REBASE_ACTIONS.contains(&entry.action.as_str()) {
            return Err(format!("Invalid action: {}", entry.action));
        }
        if !is_valid_git_oid(&entry.oid) {
            return Err(format!("Invalid commit oid: {}", entry.oid));
        }
    }

    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();

    blocking(move || {
        // Resolve the (possibly rev-expression) base to a concrete OID up front
        // — both for cleaner argv to `git rebase -i` and so the frontend gets
        // a real OID back in `stopped_at` if anything goes wrong.
        let resolved_base = {
            let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
            // Compute the OID inside the same scope as `repo` so the borrow
            // graph (obj → repo) stays valid; only the resulting String escapes.
            let oid = repo
                .revparse_single(&base_oid)
                .map_err(|e| format!("Resolve `{}`: {}", base_oid, e))?
                .peel_to_commit()
                .map_err(|e| format!("`{}` does not resolve to a commit: {}", base_oid, e))?
                .id();
            oid.to_string()
        };

        // Hold the rebase_state lock across the in-progress check + tag/dir
        // creation + register, so two near-simultaneous IPC calls can't both
        // pass the in-progress check and clobber each other's state. Also
        // refuses if our own AppState already has a rebase registered (i.e.
        // an earlier attempt is paused mid-conflict).
        let (backup_tag, state_dir) = {
            let mut guard = rebase_state.lock_safe();
            if guard.is_some() || rebase_in_progress(&path) {
                return Err("A rebase is already in progress — abort or continue it before starting a new one".into());
            }

            let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
            let backup_tag = create_rebase_backup_tag(&repo)?;
            drop(repo);

            let state_dir = match create_rebase_state_dir(&todo) {
                Ok(d) => d,
                Err(e) => {
                    // Tag was created but we never spawned — drop it so we
                    // don't leave noise behind.
                    delete_rebase_backup_tag(&path, &backup_tag);
                    return Err(e);
                }
            };
            *guard = Some(RebaseStateInfo {
                state_dir: state_dir.clone(),
                backup_tag: backup_tag.clone(),
            });
            (backup_tag, state_dir)
        };

        let result = spawn_rebase(&path, &resolved_base, &state_dir, &slots, &counter, &label);

        // Inspect git's repo state to decide whether this was a terminal result
        // (rebase finished) or a pause (state files still on disk). Three cases
        // matter: (1) rebase still paused → conflict-or-edit pause; (2) finished
        // with Ok → clean completion; (3) finished with Err → terminal failure.
        let rebase_paused = rebase_in_progress(&path);

        if rebase_paused {
            // Conflict (or `edit`) pause — leave state dir + tag intact for
            // continue/abort to consume. Surface the actual stopped commit so
            // the frontend can highlight it.
            return Ok(RebaseResult {
                ok: false,
                stopped_at: read_rebase_stopped_sha(&path),
                conflicted_files: list_conflict_files(&path),
                completed: false,
                backup_tag,
            });
        }

        match result {
            Ok(_) => {
                // Clean completion — drop the state dir, KEEP the backup tag
                // (user-visible safety net surfaced in the success toast).
                cleanup_rebase_state_dir(&state_dir);
                *rebase_state.lock_safe() = None;
                Ok(RebaseResult {
                    ok: true,
                    stopped_at: None,
                    conflicted_files: Vec::new(),
                    completed: true,
                    backup_tag,
                })
            }
            Err(err) => {
                // Terminal failure — clean up everything we created.
                cleanup_rebase_state_dir(&state_dir);
                delete_rebase_backup_tag(&path, &backup_tag);
                *rebase_state.lock_safe() = None;
                Err(err)
            }
        }
    })
    .await
}

// During a rebase pause, .git/rebase-merge/stopped-sha contains the OID of
// the commit that caused the stop (conflict or `edit`). Best-effort: a
// missing or unparseable file just yields None, which the frontend tolerates.
pub(crate) fn read_rebase_stopped_sha(repo_path: &str) -> Option<String> {
    let gd = git_dir(repo_path).ok()?;
    let candidates = [
        gd.join("rebase-merge").join("stopped-sha"),
        gd.join("rebase-apply").join("stopped-sha"),
    ];
    for p in &candidates {
        if let Ok(s) = std::fs::read_to_string(p) {
            let trimmed = s.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

pub(crate) fn spawn_rebase(
    path: &str,
    base_oid: &str,
    state_dir: &std::path::Path,
    slots: &Arc<Mutex<GitOpSlots>>,
    counter: &Arc<AtomicU64>,
    label: &str,
) -> Result<String, String> {
    // We can't reuse run_git_cancellable directly — it doesn't take env
    // overrides. Instead replicate its slot reservation pattern around a
    // bespoke spawn that sets the editor envs.
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    slots.lock_safe().insert(
        label.to_string(),
        GitOpSlot {
            op_id,
            pid: None,
            cancelled: false,
        },
    );

    let result = (|| -> Result<String, String> {
        let mut command = std::process::Command::new("git");
        command
            .args(["rebase", "-i", base_oid])
            .current_dir(path)
            .env(
                "GIT_SEQUENCE_EDITOR",
                state_dir.join("seq_editor.sh"),
            )
            .env("GIT_EDITOR", state_dir.join("commit_editor.sh"))
            .env("LAUNCHPAD_REBASE_STATE_DIR", state_dir)
            // Neutralize user wrappers — GIT_*_EDITOR take precedence over
            // EDITOR/VISUAL/core.editor in git's resolution order, but
            // clear EDITOR/VISUAL too just in case a user wrapper script
            // ignores our overrides.
            .env_remove("EDITOR")
            .env_remove("VISUAL")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        apply_git_env(&mut command);

        let child = command.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();

        let kill_now = {
            let mut guard = slots.lock_safe();
            match guard.get_mut(label) {
                Some(s) if s.op_id == op_id => {
                    s.pid = Some(pid);
                    s.cancelled
                }
                _ => false,
            }
        };
        if kill_now {
            kill_pid_hard(pid);
        }

        let output = child.wait_with_output().map_err(|e| e.to_string())?;

        {
            let mut guard = slots.lock_safe();
            if let Some(s) = guard.get_mut(label) {
                if s.op_id == op_id {
                    s.pid = None;
                }
            }
        }

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    })();

    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get(label) {
            if s.op_id == op_id {
                guard.remove(label);
            }
        }
    }

    result
}

// Common cleanup logic for terminal results from continue/skip/abort.
// `delete_tag` controls whether the backup tag is also removed (true on
// abort; false on clean continue/skip — the tag is the user's safety net).
pub(crate) fn finalize_rebase(rebase_state: &Arc<Mutex<Option<RebaseStateInfo>>>, repo_path: &str, delete_tag: bool) {
    let info = rebase_state.lock_safe().take();
    if let Some(info) = info {
        cleanup_rebase_state_dir(&info.state_dir);
        if delete_tag {
            delete_rebase_backup_tag(repo_path, &info.backup_tag);
        }
    }
}

pub(crate) fn run_git_with_rebase_env(
    args: &[&str],
    path: &str,
    slots: &Arc<Mutex<GitOpSlots>>,
    counter: &Arc<AtomicU64>,
    rebase_state: &Arc<Mutex<Option<RebaseStateInfo>>>,
    label: &str,
) -> Result<String, String> {
    // Rebase --continue/--skip may invoke commit_editor.sh again (for the
    // next reword/squash entry), so we have to keep the env vars set.
    // Replicate run_git_cancellable's slot pattern with the env overrides.
    let info = rebase_state.lock_safe().clone();
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    slots.lock_safe().insert(
        label.to_string(),
        GitOpSlot {
            op_id,
            pid: None,
            cancelled: false,
        },
    );

    let result = (|| -> Result<String, String> {
        let mut command = std::process::Command::new("git");
        command
            .args(args)
            .current_dir(path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(info) = info.as_ref() {
            command
                .env("GIT_SEQUENCE_EDITOR", info.state_dir.join("seq_editor.sh"))
                .env("GIT_EDITOR", info.state_dir.join("commit_editor.sh"))
                .env("LAUNCHPAD_REBASE_STATE_DIR", &info.state_dir)
                .env_remove("EDITOR")
                .env_remove("VISUAL");
        }
        apply_git_env(&mut command);

        let child = command.spawn().map_err(|e| e.to_string())?;
        let pid = child.id();
        let kill_now = {
            let mut guard = slots.lock_safe();
            match guard.get_mut(label) {
                Some(s) if s.op_id == op_id => {
                    s.pid = Some(pid);
                    s.cancelled
                }
                _ => false,
            }
        };
        if kill_now {
            kill_pid_hard(pid);
        }

        let output = child.wait_with_output().map_err(|e| e.to_string())?;
        {
            let mut guard = slots.lock_safe();
            if let Some(s) = guard.get_mut(label) {
                if s.op_id == op_id {
                    s.pid = None;
                }
            }
        }

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    })();

    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get(label) {
            if s.op_id == op_id {
                guard.remove(label);
            }
        }
    }

    result
}

#[tauri::command]
pub(crate) async fn git_rebase_continue(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<RebaseResult, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_with_rebase_env(&["rebase", "--continue"], &path, &slots, &counter, &rebase_state, &label);
        let still_paused = rebase_in_progress(&path);
        // rebase_state is intentionally read BEFORE finalize_rebase runs:
        // run_git_with_rebase_env doesn't touch it, so backup_tag is still
        // Some(_) here whether the rebase paused again or completed. The
        // finalize_rebase call below is what clears it on terminal results.
        let backup_tag = rebase_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.backup_tag.clone())
            .unwrap_or_default();

        if still_paused {
            // Another conflict — keep state dir + tag.
            return Ok(RebaseResult {
                ok: false,
                stopped_at: read_rebase_stopped_sha(&path),
                conflicted_files: list_conflict_files(&path),
                completed: false,
                backup_tag,
            });
        }

        // Terminal: rebase finished. Clean up state dir; keep tag on success.
        let ok = result.is_ok();
        finalize_rebase(&rebase_state, &path, /*delete_tag=*/ false);
        if !ok {
            let err = result.err().unwrap_or_else(|| "Rebase continue failed".into());
            return Err(err);
        }
        Ok(RebaseResult {
            ok: true,
            stopped_at: None,
            conflicted_files: Vec::new(),
            completed: true,
            backup_tag,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_rebase_skip(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<RebaseResult, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_with_rebase_env(&["rebase", "--skip"], &path, &slots, &counter, &rebase_state, &label);
        let still_paused = rebase_in_progress(&path);
        let backup_tag = rebase_state
            .lock()
            .unwrap()
            .as_ref()
            .map(|i| i.backup_tag.clone())
            .unwrap_or_default();

        if still_paused {
            return Ok(RebaseResult {
                ok: false,
                stopped_at: read_rebase_stopped_sha(&path),
                conflicted_files: list_conflict_files(&path),
                completed: false,
                backup_tag,
            });
        }
        let ok = result.is_ok();
        finalize_rebase(&rebase_state, &path, /*delete_tag=*/ false);
        if !ok {
            return Err(result.err().unwrap_or_else(|| "Rebase skip failed".into()));
        }
        Ok(RebaseResult {
            ok: true,
            stopped_at: None,
            conflicted_files: Vec::new(),
            completed: true,
            backup_tag,
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_rebase_abort(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let rebase_state = Arc::clone(&state.rebase_state);
    let label = window.label().to_string();
    blocking(move || {
        let result = run_git_with_rebase_env(&["rebase", "--abort"], &path, &slots, &counter, &rebase_state, &label);
        // Abort always cleans up — drop the tag too. Even on abort failure we
        // still try to drop the state dir (it's our temp resource), but leave
        // the tag in place so the user can recover manually if abort failed.
        if result.is_ok() {
            finalize_rebase(&rebase_state, &path, /*delete_tag=*/ true);
        } else {
            // Best-effort: clean the state dir without removing the tag.
            let info = rebase_state.lock_safe().take();
            if let Some(info) = info {
                cleanup_rebase_state_dir(&info.state_dir);
            }
        }
        result
    })
    .await
}

// ═══════════════════════════════════════════════════════════════════════════
// End interactive rebase block
// ═══════════════════════════════════════════════════════════════════════════

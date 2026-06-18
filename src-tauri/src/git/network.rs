use crate::*;
use git2::BranchType;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::State;

pub(crate) struct GitOpSlot {
    pub(crate) op_id: u64,
    pub(crate) pid: Option<u32>,
    pub(crate) cancelled: bool,
}

// Per-window git op tracking, keyed by `tauri::Window::label()`. At most one
// entry per window; absent key means no in-flight op for that window.
pub(crate) type GitOpSlots = HashMap<String, GitOpSlot>;

// SIGKILL, not SIGTERM. A git process blocked on a stalled TCP recv() often
// ignores SIGTERM — which is the exact situation cancel is supposed to
// escape — so we skip the polite signal and go straight to -9.
pub(crate) fn kill_pid_hard(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
}

// SSH_AUTH_SOCK resolution. Tauri .app bundles launched from Finder/Dock
// don't inherit SSH_AUTH_SOCK from the user's login shell, so SSH git
// remotes fail with "Permission denied (publickey)". On macOS we fall back
// to `launchctl getenv` which returns the per-user value the system
// ssh-agent advertises.
//
// Positive results are cached for the process lifetime (the socket path
// doesn't change after login). Negative results are cached only for
// SSH_AUTH_SOCK_NEGATIVE_TTL so a user whose agent registers late (or who
// adds their first key after launch) recovers without an app restart.
pub(crate) static SSH_AUTH_SOCK_CACHE: Mutex<Option<(Option<String>, Instant)>> = Mutex::new(None);
pub(crate) const SSH_AUTH_SOCK_NEGATIVE_TTL: Duration = Duration::from_secs(60);

pub(crate) fn resolve_ssh_auth_sock() -> Option<String> {
    let mut guard = SSH_AUTH_SOCK_CACHE.lock_safe();
    if let Some((cached, at)) = guard.as_ref() {
        if cached.is_some() || at.elapsed() < SSH_AUTH_SOCK_NEGATIVE_TTL {
            return cached.clone();
        }
    }
    let resolved = compute_ssh_auth_sock();
    *guard = Some((resolved.clone(), Instant::now()));
    resolved
}

pub(crate) fn compute_ssh_auth_sock() -> Option<String> {
    if let Ok(v) = std::env::var("SSH_AUTH_SOCK") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("launchctl")
            .args(["getenv", "SSH_AUTH_SOCK"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

// Apply the standard env every spawned `git` Command should carry:
// - GIT_TERMINAL_PROMPT=0 / GIT_ASKPASS=echo: disable interactive credential
//   prompts that would block forever in a GUI bundle with no TTY. Auth errors
//   now return immediately instead of hanging.
// - SSH_AUTH_SOCK: forwarded via resolve_ssh_auth_sock() so SSH remotes work
//   when the .app is launched from Finder/Dock.
pub(crate) fn apply_git_env(cmd: &mut std::process::Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_ASKPASS", "echo");
    if let Some(sock) = resolve_ssh_auth_sock() {
        cmd.env("SSH_AUTH_SOCK", sock);
    }
}

// Spawn a git child under an already-reserved slot (caller owns the
// reservation and the final cleanup). Records the PID on the per-window slot,
// honors a cancel that arrived during the spawn window by killing
// immediately, and on return clears the PID on the slot so a subsequent call
// under the same reservation gets a fresh pid field and can't accidentally
// target the wrong child. The cancelled flag persists across calls on the
// same slot.
pub(crate) fn spawn_git_under_slot(
    args: &[&str],
    path: &str,
    slots: &Arc<Mutex<GitOpSlots>>,
    label: &str,
    op_id: u64,
) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    command
        .args(args)
        .current_dir(path)
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
            // Slot for this window was claimed by a later op (concurrent
            // push/pull from two clicks in the same window). Treat as
            // not-ours; the other op owns cancellation now.
            _ => false,
        }
    };
    if kill_now {
        kill_pid_hard(pid);
    }

    let output = child.wait_with_output().map_err(|e| e.to_string())?;

    // Clear PID on the slot — but NOT the slot itself. The caller may run
    // another spawn under the same reservation (git_push's auto-upstream
    // retry does this), and we want the shared `cancelled` flag to persist.
    {
        let mut guard = slots.lock_safe();
        if let Some(s) = guard.get_mut(label) {
            if s.op_id == op_id {
                s.pid = None;
            }
        }
    }

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Ok(if stdout.trim().is_empty() { stderr } else { stdout })
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

// Reserve a slot for `label`, spawn a single git command, clean up. For
// multi-step ops (e.g. push-then-retry-with-upstream) that need a cancel to
// span the full sequence, reserve a slot manually and call
// spawn_git_under_slot directly.
pub(crate) fn run_git_cancellable(
    args: &[&str],
    path: &str,
    slots: &Arc<Mutex<GitOpSlots>>,
    counter: &Arc<AtomicU64>,
    label: &str,
) -> Result<String, String> {
    // Reserve the slot BEFORE spawn. If cancel arrives in the window between
    // spawn() and PID-record, it sets `cancelled=true` on our slot, which
    // spawn_git_under_slot observes and acts on by killing the child.
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    slots.lock_safe().insert(
        label.to_string(),
        GitOpSlot {
            op_id,
            pid: None,
            cancelled: false,
        },
    );

    let result = spawn_git_under_slot(args, path, slots, label, op_id);

    // Only clear the slot if it still matches our op. A subsequent operation
    // in the same window may already have replaced it — in that case leave
    // it alone.
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

// Returns (has_upstream, current_branch_name). Determined via libgit2 so
// we don't depend on parsing localized stderr text from git(1).
pub(crate) fn git_upstream_status(path: &str) -> (bool, Option<String>) {
    let Ok(repo) = Repository::discover(path) else {
        return (false, None);
    };
    let Ok(head) = repo.head() else {
        return (false, None);
    };
    let Some(branch_name) = head.shorthand().map(str::to_string) else {
        return (false, None);
    };
    let Ok(branch) = repo.find_branch(&branch_name, BranchType::Local) else {
        return (false, Some(branch_name));
    };
    let has = branch.upstream().is_ok();
    (has, Some(branch_name))
}

#[tauri::command]
pub(crate) async fn git_push(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        let (has_upstream, branch) = git_upstream_status(&path);

        // Reserve ONE slot that spans both the initial push and the optional
        // auto-upstream retry. A cancel arriving in the window between the two
        // attempts would otherwise find an empty slot and be lost — with one
        // shared reservation, the cancelled flag persists and the retry's child
        // is killed as soon as it's spawned.
        let op_id = counter.fetch_add(1, Ordering::SeqCst);
        slots.lock_safe().insert(
            label.to_string(),
            GitOpSlot {
                op_id,
                pid: None,
                cancelled: false,
            },
        );

        let result = if has_upstream {
            spawn_git_under_slot(&["push"], &path, &slots, &label, op_id)
        } else {
            match branch {
                Some(b) if !b.is_empty() => spawn_git_under_slot(
                    &["push", "--set-upstream", "origin", &b],
                    &path,
                    &slots,
                    &label,
                    op_id,
                ),
                _ => Err("Could not determine current branch for upstream push".to_string()),
            }
        };

        // Release the reservation.
        {
            let mut guard = slots.lock_safe();
            if let Some(s) = guard.get(&label) {
                if s.op_id == op_id {
                    guard.remove(&label);
                }
            }
        }

        result
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_pull(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["pull", "--prune"], &path, &slots, &counter, &label)
    })
    .await
}

#[tauri::command]
pub(crate) async fn git_fetch(
    path: String,
    state: State<'_, AppState>,
    window: tauri::Window,
) -> Result<String, String> {
    let slots = Arc::clone(&state.git_op_slots);
    let counter = Arc::clone(&state.git_op_counter);
    let label = window.label().to_string();
    blocking(move || {
        run_git_cancellable(&["fetch", "--all", "--prune"], &path, &slots, &counter, &label)
    })
    .await
}

// Mark the active git slot for `label` cancelled and return the PID (if any)
// that the caller should kill. Split out so tests can exercise the state
// machine without spawning a real git process.
//
// Slot is intentionally NOT removed here — run_git_cancellable's cleanup
// only removes when its own op_id matches, so a stale cancel can't wipe a
// newer op's registration. The cancelled flag persists so a still-spawning
// op can observe it after recording its PID.
pub(crate) fn cancel_git_op_inner(slots: &Mutex<GitOpSlots>, label: &str) -> Option<u32> {
    let mut guard = slots.lock_safe();
    if let Some(slot) = guard.get_mut(label) {
        slot.cancelled = true;
        return slot.pid;
    }
    None
}

#[tauri::command]
pub(crate) fn cancel_git_op(state: State<AppState>, window: tauri::Window) -> Result<(), String> {
    if let Some(pid) = cancel_git_op_inner(&state.git_op_slots, window.label()) {
        kill_pid_hard(pid);
    }
    Ok(())
}

#[tauri::command]
pub(crate) async fn get_remote_url(path: String) -> Result<String, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let remote = repo.find_remote("origin").map_err(|e| e.to_string())?;
        let url = remote.url().ok_or("No URL for origin")?.to_string();
        Ok(url)
    })
    .await
}

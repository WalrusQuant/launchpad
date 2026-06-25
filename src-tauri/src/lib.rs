use notify_debouncer_mini::notify;
use portable_pty::MasterPty;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

// Extracted command modules. Each is re-exported at the crate root so the
// `tauri::generate_handler!` list and the test module (which does
// `use super::*`) can reference commands by bare name. Shared state structs
// and helpers (AppState, blocking, atomic_write, …) stay in lib.rs as
// pub(crate) and the modules pull them back in via `use crate::…`.
mod settings;
pub(crate) use settings::*;
mod projects;
pub(crate) use projects::*;
mod project_env;
pub(crate) use project_env::*;
mod windows;
pub(crate) use windows::*;
mod watcher;
pub(crate) use watcher::*;
mod pty;
pub(crate) use pty::*;
mod fs;
pub(crate) use fs::*;
mod git;
pub(crate) use git::*;
mod lsp;
pub(crate) use lsp::*;

// Poison-tolerant mutex locking. A thread panicking while holding one of our
// process-lifetime mutexes (git op slots, rebase state, the SSH-sock cache)
// would poison it, and a plain `.lock().unwrap()` then turns that one-off
// panic into a permanent app-wide wedge — every later git op panics on lock.
// None of these mutexes guard an invariant that a panic could leave half-built
// (they hold maps/options of owned data), so recovering the inner value is
// safe and strictly better than cascading panics. `into_inner()` reclaims the
// guard from the poison error.
pub(crate) trait MutexExt<T> {
    fn lock_safe(&self) -> std::sync::MutexGuard<'_, T>;
}
impl<T> MutexExt<T> for Mutex<T> {
    fn lock_safe(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub(crate) async fn blocking<F, T>(f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    match tauri::async_runtime::spawn_blocking(f).await {
        Ok(inner) => inner,
        Err(join_err) => {
            // The panic itself is captured to ~/.launchpad/panic.log by the
            // panic hook installed in run(); this extra eprintln gives the
            // join error context in the dev console so it's correlated with
            // whichever command was running when the panic happened.
            eprintln!("[blocking] spawn_blocking task failed: {join_err}");
            Err(format!("blocking task panicked: {join_err}"))
        }
    }
}

pub(crate) type FsWatcher = notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>;

// Tauri event names. Mirror src/events.js — keep both lists in sync, since
// a typo on either side silently drops events. Centralizing here also makes
// "what events does the backend emit?" answerable in one grep.
pub(crate) mod events {
    pub const PTY_OUTPUT: &str = "pty-output";
    pub const PTY_EXIT: &str = "pty-exit";
    pub const FS_CHANGED: &str = "fs-changed";
    pub const LSP_MESSAGE: &str = "lsp-message";
    pub const LSP_EXIT: &str = "lsp-exit";
}

// Each tab has its own PTY writer and master
pub(crate) struct PtyInstance {
    pub(crate) writer: Box<dyn Write + Send>,
    pub(crate) master: Box<dyn MasterPty + Send>,
    pub(crate) last_size: (u16, u16), // (rows, cols) — used to skip redundant resizes
    // Reader held until the frontend has registered this PTY in paneMap, then
    // claimed by start_pty_reader. Prevents the race where pty-output events
    // fire before paneMap.set() and get silently dropped by the JS listener.
    pub(crate) pending_reader: Option<Box<dyn Read + Send>>,
    // Flow control: frontend sets this when xterm.js write queue exceeds the
    // high-water mark. The reader thread checks it after each read and sleeps
    // until cleared. Data stays in the OS PTY buffer — nothing is lost.
    pub(crate) paused: Arc<AtomicBool>,
}

pub(crate) struct AppState {
    pub(crate) ptys: Arc<Mutex<HashMap<u32, PtyInstance>>>,
    pub(crate) next_id: Arc<Mutex<u32>>,
    pub(crate) fs_watcher: Arc<Mutex<HashMap<String, FsWatcher>>>,
    // In-flight git network operations (push/pull/fetch/merge/cherry-pick/
    // rebase), keyed by Tauri window label. Each window owns its own slot
    // so two windows running concurrent agents can't clobber each other's
    // cancellation state. The slot is reserved BEFORE spawn so a cancel
    // arriving in the window between spawn() and PID-record is still
    // observed and applied. `op_id` (from git_op_counter) tags each op so
    // cleanup can tell whether the slot still belongs to us or has been
    // claimed by a later operation in the same window.
    pub(crate) git_op_slots: Arc<Mutex<GitOpSlots>>,
    pub(crate) git_op_counter: Arc<AtomicU64>,
    // Canonicalized project path → Tauri window label. Used by focus_project_window
    // to focus an existing window instead of opening a duplicate. Stale entries are
    // cleaned lazily on focus attempt (when get_webview_window returns None).
    pub(crate) project_windows: Arc<Mutex<HashMap<String, String>>>,
    // Serializes read→mutate→write of ~/.launchpad/projects.json so concurrent
    // commands (e.g. two windows both hitting add_project) can't lose updates.
    pub(crate) projects_file_lock: Arc<Mutex<()>>,
    // Same guarantee for ~/.launchpad/project-env.json. atomic_write makes each
    // write atomic but does nothing for the read-modify-write race: two windows
    // saving env for different projects concurrently could interleave and drop
    // one project's entire secret set. Held across read→mutate→write in
    // save_project_env_vars / forget_project_env.
    pub(crate) project_env_file_lock: Arc<Mutex<()>>,
    // Active interactive rebase state. Set by git_rebase_interactive_apply
    // before spawn; consulted by abort/continue/skip for cleanup. None when
    // no rebase is in progress (or one was leaked across an app restart —
    // git --continue/--abort still work; we just don't get the cleanup).
    pub(crate) rebase_state: Arc<Mutex<Option<RebaseStateInfo>>>,
    // Running language servers, keyed by "{language}:{project_path}". One server
    // per language per project (see lsp.rs). Holds the child handle + stdin pipe;
    // the reader thread owns stdout and forwards framed messages as events.
    pub(crate) lsp_servers: Arc<Mutex<HashMap<String, LspInstance>>>,
}

#[derive(Clone)]
pub(crate) struct RebaseStateInfo {
    pub(crate) state_dir: std::path::PathBuf,
    pub(crate) backup_tag: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct PtyOutput {
    pub(crate) tab_id: u32,
    pub(crate) data: String,
}

#[derive(Clone, Serialize)]
pub(crate) struct SpawnResult {
    pub(crate) tab_id: u32,
}

#[derive(Clone, Serialize)]
pub(crate) struct PtyExit {
    pub(crate) tab_id: u32,
}

#[derive(Clone, Serialize)]
pub(crate) struct FsChanged {
    pub(crate) path: String,
    // Canonical paths of files/dirs that changed in this debounce batch,
    // already filtered against the noise skip list (.git/, node_modules/,
    // target/, etc.). The frontend uses this to scope the editor-tab
    // re-read pass to only changed files instead of every open tab.
    // Empty after filtering means the whole batch was noise — but in
    // that case we don't emit at all, so consumers never see this empty.
    pub(crate) changed_paths: Vec<String>,
}

// Write `data` to `dest` atomically: write to a sibling temp file, fsync,
// then rename onto the destination. A crash / kill / ENOSPC mid-write
// leaves the old file intact; a successful rename replaces it in one step.
// Preserves the existing file's mode when overwriting.
//
// Temp name uses PID + a process-monotonic counter so two concurrent
// writes from different windows (Tauri runs all windows in one process)
// can't collide on the same temp path and corrupt each other.
static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn atomic_write(dest: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    atomic_write_with_mode(dest, data, None)
}

// Like atomic_write, but forces a specific mode on the temp file BEFORE
// rename. Use this when the destination's permissions are security-
// relevant (e.g. project-env.json at 0o600): if we rename first and
// chmod after, the file briefly exists at the default umask (0o644)
// and a concurrent reader on a shared machine could snapshot secrets.
pub(crate) fn atomic_write_with_mode(
    dest: &std::path::Path,
    data: &[u8],
    explicit_mode: Option<u32>,
) -> std::io::Result<()> {
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("launchpad-write");
    let counter = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = dest.with_file_name(format!(
        ".{}.lp-tmp-{}-{}",
        file_name,
        std::process::id(),
        counter
    ));

    // Explicit mode wins over preserved (for a new secret file we want
    // 0o600 even if the previous file — if any — was 0o644).
    let preserved_mode = std::fs::metadata(dest).ok().map(|m| m.permissions().mode());
    let effective_mode = explicit_mode.or(preserved_mode);

    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        use std::io::Write;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        // Mode setting is best-effort — failing here (e.g. on a mount
        // where chmod is denied) should not abort the whole write and
        // lose the user's data. For security-relevant modes, the worst
        // case falls back to the default umask rather than exposing a
        // 0o644 window between rename and a subsequent chmod.
        if let Some(mode) = effective_mode {
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode));
        }
        std::fs::rename(&tmp, dest)
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// Append a structured record of a panic to ~/.launchpad/panic.log so a crash
// in any background thread (PTY reader, watcher, git driver) leaves a trace
// the user can grab with /panic-log. Best-effort — if the write fails we
// fall through to the default panic hook (stderr) and don't shadow it.
fn write_panic_log(info: &std::panic::PanicHookInfo) {
    let dir = launchpad_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("panic.log");
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("<non-string panic payload>");
    let thread = std::thread::current()
        .name()
        .unwrap_or("<unnamed>")
        .to_string();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!(
        "[{}] thread={} location={}\n  payload: {}\n",
        ts, thread, location, payload
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn run() {
    // Panic hook: log every panic to ~/.launchpad/panic.log and still call the
    // default hook (stderr / process abort behavior unchanged). Installed
    // before any threads spawn so background panics in the PTY reader,
    // filesystem watcher, or git driver scripts get captured.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write_panic_log(info);
        default_hook(info);
    }));

    let app_state = AppState {
        ptys: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(Mutex::new(0)),
        fs_watcher: Arc::new(Mutex::new(HashMap::new())),
        git_op_slots: Arc::new(Mutex::new(HashMap::new())),
        git_op_counter: Arc::new(AtomicU64::new(0)),
        project_windows: Arc::new(Mutex::new(HashMap::new())),
        projects_file_lock: Arc::new(Mutex::new(())),
        project_env_file_lock: Arc::new(Mutex::new(())),
        rebase_state: Arc::new(Mutex::new(None)),
        lsp_servers: Arc::new(Mutex::new(HashMap::new())),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            spawn_pty,
            start_pty_reader,
            write_to_pty,
            resize_pty,
            pause_pty_reader,
            resume_pty_reader,
            close_pty,
            write_debug_log,
            read_directory,
            get_home_dir,
            get_git_status,
            list_branches,
            get_commits,
            get_project_card,
            checkout_branch,
            create_branch,
            load_settings,
            save_settings,
            load_file_settings,
            save_file_settings,
            load_projects,
            add_project,
            remove_project,
            rename_project,
            touch_project,
            load_project_env_vars,
            save_project_env_vars,
            forget_project_env,
            search_files,
            search_in_files,
            pick_directory,
            reveal_in_finder,
            open_in_default_app,
            read_file_preview,
            write_file,
            create_file,
            create_directory,
            delete_path,
            rename_path,
            format_file,
            get_file_inode,
            find_path_by_inode,
            get_file_diff,
            get_file_diff_vs_head,
            git_blame_file,
            get_diff_between_refs,
            git_stage_all,
            git_stage_file,
            git_commit,
            git_push,
            git_pull,
            git_fetch,
            git_merge_branch,
            git_amend_commit,
            git_head_on_remote,
            git_cherry_pick,
            git_cherry_pick_abort,
            git_cherry_pick_continue,
            get_pending_op_state,
            get_rebase_candidate_commits,
            git_rebase_interactive_apply,
            git_rebase_continue,
            git_rebase_skip,
            git_rebase_abort,
            get_conflict_versions,
            git_resolve_ours,
            git_resolve_theirs,
            git_unstage_file,
            git_unstage_all,
            git_discard_file,
            git_stash_save,
            git_stash_pop,
            git_stash_list,
            git_stash_apply,
            git_stash_drop,
            cancel_git_op,
            git_delete_branch,
            list_remote_branches,
            get_commit_details,
            get_remote_url,
            watch_directory,
            unwatch_directory,
            open_new_window,
            focus_project_window,
            register_project_window,
            unregister_project_window,
            unregister_window_label,
            lsp_start,
            lsp_send,
            lsp_stop,
            lsp_server_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

pub(crate) fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

// Resolves to the directory holding config.json / projects.json /
// project-env.json / debug.log. Honors $LAUNCHPAD_HOME so tests can
// redirect to a tempdir without clobbering the user's real config.
// `LAUNCHPAD_HOME` IS the directory (not its parent), so a test can
// `std::env::set_var("LAUNCHPAD_HOME", tempdir.path())` and immediately
// read/write files there.
pub(crate) fn launchpad_dir() -> std::path::PathBuf {
    if let Some(override_path) = std::env::var_os("LAUNCHPAD_HOME") {
        return std::path::PathBuf::from(override_path);
    }
    let home = dirs_home().unwrap_or_else(|| "/tmp".into());
    home.join(".launchpad")
}

#[cfg(test)]
mod tests;

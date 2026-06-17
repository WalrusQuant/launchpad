use git2::{BranchType, Repository, StatusOptions};
use notify_debouncer_mini::{new_debouncer, notify};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

// Extracted command modules. Each is re-exported at the crate root so the
// `tauri::generate_handler!` list and the test module (which does
// `use super::*`) can reference commands by bare name. Shared state structs
// and helpers (AppState, blocking, atomic_write, …) stay in lib.rs as
// pub(crate) and the modules pull them back in via `use crate::…`.
mod settings;
pub(crate) use settings::*;

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

type FsWatcher = notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>;

// Tauri event names. Mirror src/events.js — keep both lists in sync, since
// a typo on either side silently drops events. Centralizing here also makes
// "what events does the backend emit?" answerable in one grep.
mod events {
    pub const PTY_OUTPUT: &str = "pty-output";
    pub const PTY_EXIT: &str = "pty-exit";
    pub const FS_CHANGED: &str = "fs-changed";
}

// Each tab has its own PTY writer and master
struct PtyInstance {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    last_size: (u16, u16), // (rows, cols) — used to skip redundant resizes
    // Reader held until the frontend has registered this PTY in paneMap, then
    // claimed by start_pty_reader. Prevents the race where pty-output events
    // fire before paneMap.set() and get silently dropped by the JS listener.
    pending_reader: Option<Box<dyn Read + Send>>,
    // Flow control: frontend sets this when xterm.js write queue exceeds the
    // high-water mark. The reader thread checks it after each read and sleeps
    // until cleared. Data stays in the OS PTY buffer — nothing is lost.
    paused: Arc<AtomicBool>,
}

struct AppState {
    ptys: Arc<Mutex<HashMap<u32, PtyInstance>>>,
    next_id: Arc<Mutex<u32>>,
    fs_watcher: Arc<Mutex<HashMap<String, FsWatcher>>>,
    // In-flight git network operations (push/pull/fetch/merge/cherry-pick/
    // rebase), keyed by Tauri window label. Each window owns its own slot
    // so two windows running concurrent agents can't clobber each other's
    // cancellation state. The slot is reserved BEFORE spawn so a cancel
    // arriving in the window between spawn() and PID-record is still
    // observed and applied. `op_id` (from git_op_counter) tags each op so
    // cleanup can tell whether the slot still belongs to us or has been
    // claimed by a later operation in the same window.
    git_op_slots: Arc<Mutex<GitOpSlots>>,
    git_op_counter: Arc<AtomicU64>,
    // Canonicalized project path → Tauri window label. Used by focus_project_window
    // to focus an existing window instead of opening a duplicate. Stale entries are
    // cleaned lazily on focus attempt (when get_webview_window returns None).
    project_windows: Arc<Mutex<HashMap<String, String>>>,
    // Serializes read→mutate→write of ~/.launchpad/projects.json so concurrent
    // commands (e.g. two windows both hitting add_project) can't lose updates.
    projects_file_lock: Arc<Mutex<()>>,
    // Same guarantee for ~/.launchpad/project-env.json. atomic_write makes each
    // write atomic but does nothing for the read-modify-write race: two windows
    // saving env for different projects concurrently could interleave and drop
    // one project's entire secret set. Held across read→mutate→write in
    // save_project_env_vars / forget_project_env.
    project_env_file_lock: Arc<Mutex<()>>,
    // Active interactive rebase state. Set by git_rebase_interactive_apply
    // before spawn; consulted by abort/continue/skip for cleanup. None when
    // no rebase is in progress (or one was leaked across an app restart —
    // git --continue/--abort still work; we just don't get the cleanup).
    rebase_state: Arc<Mutex<Option<RebaseStateInfo>>>,
}

#[derive(Clone)]
struct RebaseStateInfo {
    state_dir: std::path::PathBuf,
    backup_tag: String,
}

#[derive(Clone, Serialize)]
struct PtyOutput {
    tab_id: u32,
    data: String,
}

#[derive(Clone, Serialize)]
struct SpawnResult {
    tab_id: u32,
}

#[derive(Clone, Serialize)]
struct PtyExit {
    tab_id: u32,
}

#[derive(Clone, Serialize)]
struct FsChanged {
    path: String,
    // Canonical paths of files/dirs that changed in this debounce batch,
    // already filtered against the noise skip list (.git/, node_modules/,
    // target/, etc.). The frontend uses this to scope the editor-tab
    // re-read pass to only changed files instead of every open tab.
    // Empty after filtering means the whole batch was noise — but in
    // that case we don't emit at all, so consumers never see this empty.
    changed_paths: Vec<String>,
}

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
fn fs_event_is_noise(event_path: &std::path::Path, root: &std::path::Path) -> bool {
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

#[derive(Clone, Serialize)]
struct FileEntry {
    name: String,
    path: String,
    is_dir: bool,
    is_hidden: bool,
    size: u64,
    modified: Option<u64>,
    mode: u32,
}

#[tauri::command]
fn spawn_pty(cwd: Option<String>, project_path: Option<String>, rows: Option<u16>, cols: Option<u16>, state: State<AppState>, app: tauri::AppHandle) -> Result<SpawnResult, String> {
    let mut next_id = state.next_id.lock().map_err(|e| e.to_string())?;
    let tab_id = *next_id;
    *next_id += 1;
    drop(next_id);

    let pty_rows = rows.unwrap_or(24);
    let pty_cols = cols.unwrap_or(80);

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: pty_rows,
            cols: pty_cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");

    // Inherit the parent process environment. portable-pty's CommandBuilder does
    // NOT inherit env by default — without this the shell starts with almost
    // nothing, and programs that read env directly (Claude Code, Ink-based TUIs)
    // fall back to limited terminfo and emit clear-region escape codes that don't
    // match what xterm.js actually erases. That's the cause of the "floating
    // character" ghosts (Shingle bubble fragments, status bar remnants).
    for (key, value) in std::env::vars() {
        cmd.env(&key, &value);
    }
    // Project-scoped env vars (from ~/.launchpad/project-env.json). Applied
    // AFTER parent env so per-project values override the user's shell env,
    // but BEFORE the TERM / TERM_PROGRAM overrides below so a user can't
    // accidentally break terminal capability detection by naming a var TERM.
    if let Some(p) = project_path.as_deref() {
        for (k, v) in load_env_for_project(p) {
            cmd.env(&k, &v);
        }
    }
    // Override / set the terminal-identifying vars that iTerm and Terminal.app set.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    // Masquerade as Apple_Terminal: Charm/termenv (crush, opencode, etc.) branches
    // on TERM_PROGRAM to pick a capability profile. An unknown value falls back to
    // modern terminal probes (OSC 10/11, DA queries) that xterm.js doesn't fully
    // answer, deadlocking Bubble Tea's startup and leaving a blank screen.
    // Apple_Terminal is the most conservative known-good profile.
    cmd.env("TERM_PROGRAM", "Apple_Terminal");
    // Ensure UTF-8 locale so multi-byte glyph widths get measured correctly.
    if std::env::var("LANG").is_err() {
        cmd.env("LANG", "en_US.UTF-8");
    }
    let start_dir = cwd
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| dirs_home().unwrap_or_else(|| "/Users".into()));
    cmd.cwd(start_dir);

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;
    drop(pair.slave);

    let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    // Store the reader in the instance instead of spawning the output thread
    // here. The frontend will call start_pty_reader AFTER registering the pane
    // in paneMap, closing the race where early output gets dropped.
    let instance = PtyInstance {
        writer,
        master: pair.master,
        last_size: (pty_rows, pty_cols),
        pending_reader: Some(reader),
        paused: Arc::new(AtomicBool::new(false)),
    };

    state
        .ptys
        .lock()
        .map_err(|e| e.to_string())?
        .insert(tab_id, instance);

    // Child-wait thread: detects when shell process exits and notifies frontend
    let handle2 = app.clone();
    let ptys_clone = state.ptys.clone();
    std::thread::spawn(move || {
        let _ = child.wait();
        let _ = ptys_clone.lock().map(|mut ptys| { ptys.remove(&tab_id); });
        let _ = handle2.emit(events::PTY_EXIT, PtyExit { tab_id });
    });

    Ok(SpawnResult { tab_id })
}

// Called by the frontend AFTER it has registered the pane in paneMap.
// Claims the reader stored during spawn_pty and spawns the output thread.
// Splitting this off from spawn_pty closes the race where pty-output events
// fire before the frontend's listener has a pane to route them to.
#[tauri::command]
fn start_pty_reader(tab_id: u32, state: State<AppState>, app: tauri::AppHandle) -> Result<(), String> {
    let (mut reader, paused) = {
        let mut ptys = state.ptys.lock().map_err(|e| e.to_string())?;
        let instance = ptys.get_mut(&tab_id).ok_or("Tab not found")?;
        let r = instance
            .pending_reader
            .take()
            .ok_or("Reader already started")?;
        (r, Arc::clone(&instance.paused))
    };

    let handle = app.clone();
    std::thread::spawn(move || {
        // catch_unwind so a panic in the reader (e.g. inside Tauri's emit
        // serialization, or a future addition that touches AppState mutexes)
        // gets logged via the global panic hook instead of unwinding the
        // thread and risking poisoning of any mutex it might hold.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut buf = [0u8; 4096];
            let mut leftover = Vec::new();
            loop {
                // Flow control: sleep while the frontend signals backpressure.
                // Data stays in the OS PTY buffer — nothing is lost.
                while paused.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        leftover.extend_from_slice(&buf[..n]);
                        let valid_up_to = match std::str::from_utf8(&leftover) {
                            Ok(_) => leftover.len(),
                            Err(e) => e.valid_up_to(),
                        };
                        if valid_up_to > 0 {
                            let output = String::from_utf8(leftover[..valid_up_to].to_vec())
                                .expect("valid_up_to guarantees valid UTF-8");
                            let _ = handle.emit(events::PTY_OUTPUT, PtyOutput { tab_id, data: output });
                            leftover = leftover[valid_up_to..].to_vec();
                        } else if leftover.len() > 64 * 1024 {
                            // A valid UTF-8 continuation is at most 3 bytes; if we
                            // have 64KB of invalid bytes with no valid prefix, the
                            // stream is corrupted (binary garbage, crashed shell,
                            // wrong encoding). Flush as replacement characters so
                            // the buffer can't grow without bound.
                            let output = String::from_utf8_lossy(&leftover).into_owned();
                            let _ = handle.emit(events::PTY_OUTPUT, PtyOutput { tab_id, data: output });
                            leftover.clear();
                        }
                    }
                    Err(_) => break,
                }
            }
        }));
    });

    Ok(())
}

#[tauri::command]
fn write_to_pty(tab_id: u32, data: String, state: State<AppState>) -> Result<(), String> {
    let mut ptys = state.ptys.lock().map_err(|e| e.to_string())?;
    let instance = ptys.get_mut(&tab_id).ok_or("Tab not found")?;
    instance
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| e.to_string())?;
    instance.writer.flush().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn resize_pty(tab_id: u32, rows: u16, cols: u16, state: State<AppState>) -> Result<(), String> {
    let mut ptys = state.ptys.lock().map_err(|e| e.to_string())?;
    let instance = ptys.get_mut(&tab_id).ok_or("Tab not found")?;
    // Skip redundant resizes — avoids spurious SIGWINCH and shell repaints
    if instance.last_size == (rows, cols) {
        return Ok(());
    }
    instance
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;
    instance.last_size = (rows, cols);
    Ok(())
}

#[tauri::command]
fn pause_pty_reader(tab_id: u32, state: State<AppState>) -> Result<(), String> {
    let ptys = state.ptys.lock().map_err(|e| e.to_string())?;
    let instance = ptys.get(&tab_id).ok_or("Tab not found")?;
    instance.paused.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
fn resume_pty_reader(tab_id: u32, state: State<AppState>) -> Result<(), String> {
    let ptys = state.ptys.lock().map_err(|e| e.to_string())?;
    let instance = ptys.get(&tab_id).ok_or("Tab not found")?;
    instance.paused.store(false, Ordering::Relaxed);
    Ok(())
}

// Writes a debug capture session to ~/.launchpad/debug.log. Overwrites any
// previous capture. Returns the absolute path so the frontend can display it.
#[tauri::command]
fn write_debug_log(content: String) -> Result<String, String> {
    let dir = launchpad_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("debug.log");
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().to_string())
}

#[tauri::command]
fn close_pty(tab_id: u32, state: State<AppState>) -> Result<(), String> {
    state
        .ptys
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&tab_id);
    Ok(())
}

#[tauri::command]
async fn read_directory(path: String) -> Result<Vec<FileEntry>, String> {
    blocking(move || {
        let entries = fs::read_dir(&path).map_err(|e| e.to_string())?;

        let mut files: Vec<FileEntry> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                // Use to_string_lossy so non-UTF-8 filenames (rare on macOS but
                // possible) still show up with replacement characters rather
                // than disappearing from the listing entirely.
                let name = entry.file_name().to_string_lossy().to_string();
                let is_hidden = name.starts_with('.');
                // Don't drop the entry if metadata fails (permission denied,
                // or TOCTOU where the file was removed between read_dir and
                // metadata). Fall through to sensible defaults so the user
                // still sees what's there.
                let metadata = entry.metadata().ok();
                let is_dir = metadata.as_ref().map(|m| m.is_dir()).unwrap_or(false);
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified = metadata
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs());
                let mode = metadata
                    .as_ref()
                    .map(|m| m.permissions().mode())
                    .unwrap_or(0);

                Some(FileEntry {
                    name,
                    path: entry.path().to_string_lossy().to_string(),
                    is_dir,
                    is_hidden,
                    size,
                    modified,
                    mode,
                })
            })
            .collect();

        files.sort_by(|a, b| {
            b.is_dir
                .cmp(&a.is_dir)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        Ok(files)
    })
    .await
}

#[derive(Clone, Serialize)]
struct GitFileStatus {
    path: String,
    status: String,
}

#[derive(Clone, Serialize)]
struct GitInfo {
    is_repo: bool,
    branch: Option<String>,
    files: Vec<GitFileStatus>,
    ahead: usize,
    behind: usize,
    has_upstream: bool,
}

fn get_git_status_inner(path: &str) -> Result<GitInfo, String> {
    let repo = match Repository::discover(path) {
        Ok(r) => r,
        Err(_) => {
            return Ok(GitInfo {
                is_repo: false,
                branch: None,
                files: vec![],
                ahead: 0,
                behind: 0,
                has_upstream: false,
            });
        }
    };

    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(String::from));

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false);

    let statuses = repo.statuses(Some(&mut opts)).map_err(|e| e.to_string())?;

    let mut files: Vec<GitFileStatus> = Vec::new();
    for entry in statuses.iter() {
        let path = match entry.path() {
            Some(p) => p.to_string(),
            None => continue,
        };
        let s = entry.status();

        if s.is_conflicted() {
            files.push(GitFileStatus { path, status: "conflict".into() });
            continue;
        }

        // Index (staged) changes
        if s.is_index_new() {
            files.push(GitFileStatus { path: path.clone(), status: "index_new".into() });
        } else if s.is_index_modified() {
            files.push(GitFileStatus { path: path.clone(), status: "index_modified".into() });
        } else if s.is_index_deleted() {
            files.push(GitFileStatus { path: path.clone(), status: "index_deleted".into() });
        } else if s.is_index_renamed() {
            files.push(GitFileStatus { path: path.clone(), status: "index_renamed".into() });
        }

        // Worktree changes
        if s.is_wt_new() {
            files.push(GitFileStatus { path: path.clone(), status: "new".into() });
        } else if s.is_wt_modified() {
            files.push(GitFileStatus { path: path.clone(), status: "modified".into() });
        } else if s.is_wt_deleted() {
            files.push(GitFileStatus { path: path.clone(), status: "deleted".into() });
        } else if s.is_wt_renamed() {
            files.push(GitFileStatus { path: path.clone(), status: "renamed".into() });
        }
    }

    let ab = get_ahead_behind(&repo);
    let has_upstream = ab.is_some();
    let ahead = ab.map(|(a, _)| a).unwrap_or(0);
    let behind = ab.map(|(_, b)| b).unwrap_or(0);

    Ok(GitInfo {
        is_repo: true,
        branch,
        files,
        ahead,
        behind,
        has_upstream,
    })
}

#[tauri::command]
async fn get_git_status(path: String) -> Result<GitInfo, String> {
    blocking(move || get_git_status_inner(&path)).await
}

fn get_ahead_behind(repo: &Repository) -> Option<(usize, usize)> {
    let head = repo.head().ok()?;
    let local_oid = head.target()?;
    let branch_name = head.shorthand()?;

    let upstream_ref_name = repo.branch_upstream_name(&format!("refs/heads/{}", branch_name)).ok()?;
    let upstream_name = upstream_ref_name.as_str()?;
    let upstream_ref = repo.find_reference(upstream_name).ok()?;
    let upstream_oid = upstream_ref.target()?;

    repo.graph_ahead_behind(local_oid, upstream_oid).ok()
}

#[derive(Clone, Serialize)]
struct BranchInfo {
    name: String,
    is_current: bool,
    last_commit_msg: Option<String>,
    last_commit_time: Option<i64>,
    upstream: Option<String>,
}

#[derive(Clone, Serialize)]
struct CommitInfo {
    oid: String,
    message: String,
    author: String,
    timestamp: i64,
    parent_count: usize,
}

#[tauri::command]
async fn list_branches(path: String) -> Result<Vec<BranchInfo>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let branches = repo
            .branches(Some(BranchType::Local))
            .map_err(|e| e.to_string())?;

        let current_branch = repo
            .head()
            .ok()
            .and_then(|h| h.shorthand().map(String::from));

        let mut result: Vec<BranchInfo> = branches
            .filter_map(|b| {
                let (branch, _) = b.ok()?;
                let name = branch.name().ok()??.to_string();
                let is_current = current_branch.as_deref() == Some(&name);

                let commit = branch.get().peel_to_commit().ok();
                let last_commit_msg = commit
                    .as_ref()
                    .and_then(|c| c.summary().map(String::from));
                let last_commit_time = commit.as_ref().map(|c| c.time().seconds());

                let upstream = branch
                    .upstream()
                    .ok()
                    .and_then(|u| u.name().ok().flatten().map(String::from));

                Some(BranchInfo {
                    name,
                    is_current,
                    last_commit_msg,
                    last_commit_time,
                    upstream,
                })
            })
            .collect();

        result.sort_by(|a, b| {
            b.is_current
                .cmp(&a.is_current)
                .then_with(|| a.name.cmp(&b.name))
        });

        Ok(result)
    })
    .await
}

#[tauri::command]
async fn get_commits(path: String, count: Option<usize>) -> Result<Vec<CommitInfo>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let head = match repo.head() {
            Ok(h) => h,
            Err(_) => return Ok(vec![]),
        };
        let head_oid = match head.target() {
            Some(oid) => oid,
            None => return Ok(vec![]),
        };

        let mut revwalk = repo.revwalk().map_err(|e| e.to_string())?;
        revwalk.push(head_oid).map_err(|e| e.to_string())?;

        let limit = count.unwrap_or(20);

        let commits: Vec<CommitInfo> = revwalk
            .take(limit)
            .filter_map(|oid| {
                let oid = oid.ok()?;
                let commit = repo.find_commit(oid).ok()?;
                let short_id = oid.to_string()[..7].to_string();
                let message = commit
                    .summary()
                    .unwrap_or("(no message)")
                    .to_string();
                let author = commit.author().name().unwrap_or("Unknown").to_string();
                let timestamp = commit.time().seconds();

                Some(CommitInfo {
                    oid: short_id,
                    message,
                    author,
                    timestamp,
                    parent_count: commit.parent_count(),
                })
            })
            .collect();

        Ok(commits)
    })
    .await
}

#[tauri::command]
async fn checkout_branch(path: String, branch_name: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        checkout_branch_inner(&repo, &branch_name)
    })
    .await
}

#[tauri::command]
async fn create_branch(path: String, branch_name: String, checkout: Option<bool>) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let head = repo.head().map_err(|e| e.to_string())?;
        let commit = head.peel_to_commit().map_err(|e| e.to_string())?;

        repo.branch(&branch_name, &commit, false)
            .map_err(|e| e.to_string())?;

        if checkout.unwrap_or(true) {
            checkout_branch_inner(&repo, &branch_name)?;
        }

        Ok(())
    })
    .await
}

fn checkout_branch_inner(repo: &Repository, branch_name: &str) -> Result<(), String> {
    let branch = repo
        .find_branch(branch_name, BranchType::Local)
        .map_err(|e| e.to_string())?;

    let refname = branch
        .get()
        .name()
        .ok_or("Invalid branch reference")?
        .to_string();

    // Resolve the target tree and attempt a safe checkout first. libgit2 will
    // error if the operation would overwrite uncommitted changes — matching
    // `git checkout <branch>` behavior. Only move HEAD after the working tree
    // is successfully updated so a failure here doesn't leave HEAD pointing
    // at a branch whose tree we never actually checked out.
    let target_commit = branch.get().peel_to_commit().map_err(|e| e.to_string())?;
    let target_tree = target_commit.tree().map_err(|e| e.to_string())?;

    repo.checkout_tree(target_tree.as_object(), None)
        .map_err(|e| e.to_string())?;

    repo.set_head(&refname).map_err(|e| e.to_string())?;

    Ok(())
}

// Projects
#[derive(Clone, Serialize, serde::Deserialize)]
struct Project {
    name: String,
    path: String,
    #[serde(rename = "lastOpened")]
    last_opened: String,
}

fn projects_path() -> std::path::PathBuf {
    launchpad_dir().join("projects.json")
}

fn read_projects_file() -> Result<Vec<Project>, String> {
    let path = projects_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if data.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str::<Vec<Project>>(&data)
        .map_err(|e| format!("Invalid projects.json: {}", e))
}

fn write_projects_file(projects: &[Project]) -> Result<(), String> {
    let path = projects_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(projects)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    atomic_write(&path, body.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

fn normalize_project_path(path: &str) -> String {
    // Canonicalize when possible so two different strings for the same dir dedupe.
    // Fall back to the raw string if the path doesn't resolve (missing dir).
    match fs::canonicalize(path) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(_) => path.trim_end_matches('/').to_string(),
    }
}

#[tauri::command]
fn load_projects() -> Result<Vec<Project>, String> {
    let mut projects = read_projects_file()?;
    // Sort by lastOpened desc so recent projects come first.
    projects.sort_by(|a, b| b.last_opened.cmp(&a.last_opened));
    Ok(projects)
}

#[tauri::command]
fn add_project(
    path: String,
    name: Option<String>,
    last_opened: String,
    state: State<AppState>,
) -> Result<Project, String> {
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;

    let normalized = normalize_project_path(&path);
    let derived_name = name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        std::path::Path::new(&normalized)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| normalized.clone())
    });

    let mut projects = read_projects_file()?;
    if let Some(existing) = projects
        .iter_mut()
        .find(|p| normalize_project_path(&p.path) == normalized)
    {
        existing.last_opened = last_opened;
        let out = existing.clone();
        write_projects_file(&projects)?;
        return Ok(out);
    }

    let project = Project {
        name: derived_name,
        path: normalized,
        last_opened,
    };
    projects.push(project.clone());
    write_projects_file(&projects)?;
    Ok(project)
}

#[tauri::command]
fn remove_project(path: String, state: State<AppState>) -> Result<(), String> {
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;
    let target = normalize_project_path(&path);
    let mut projects = read_projects_file()?;
    projects.retain(|p| normalize_project_path(&p.path) != target);
    write_projects_file(&projects)?;
    // Best-effort: drop stored env vars for this project too. Deleting a project
    // is an explicit act; leaving the secrets behind would be a privacy leak.
    // Acquire the env-file lock for the read-modify-write (distinct file, distinct
    // lock). Ordering is always projects_file_lock → project_env_file_lock here
    // and nowhere the reverse, so no deadlock.
    {
        let _eguard = state.project_env_file_lock.lock_safe();
        let _ = forget_project_env_locked(&path);
    }
    Ok(())
}

#[tauri::command]
fn rename_project(
    path: String,
    new_name: String,
    state: State<AppState>,
) -> Result<(), String> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        return Err("Name cannot be empty".into());
    }
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;
    let target = normalize_project_path(&path);
    let mut projects = read_projects_file()?;
    if let Some(p) = projects
        .iter_mut()
        .find(|p| normalize_project_path(&p.path) == target)
    {
        p.name = trimmed.to_string();
        write_projects_file(&projects)?;
    }
    Ok(())
}

#[tauri::command]
fn touch_project(
    path: String,
    last_opened: String,
    state: State<AppState>,
) -> Result<(), String> {
    let _guard = state
        .projects_file_lock
        .lock()
        .map_err(|e| format!("projects lock poisoned: {}", e))?;
    let target = normalize_project_path(&path);
    let mut projects = read_projects_file()?;
    if let Some(p) = projects
        .iter_mut()
        .find(|p| normalize_project_path(&p.path) == target)
    {
        p.last_opened = last_opened;
        write_projects_file(&projects)?;
    }
    Ok(())
}

// ─── Per-project environment variables ───────────────────────────────────────
// Stored at ~/.launchpad/project-env.json, keyed by canonicalized project path.
// Injected into every PTY spawned for the active project (see spawn_pty).
// File is written atomically and chmod'd to 0o600 so other users can't read it.
#[derive(Clone, Serialize, serde::Deserialize)]
struct ProjectEnvVar {
    key: String,
    value: String,
    secret: bool,
}

type ProjectEnvStore = std::collections::BTreeMap<String, Vec<ProjectEnvVar>>;

fn project_env_path() -> std::path::PathBuf {
    launchpad_dir().join("project-env.json")
}

fn read_project_env_file() -> Result<ProjectEnvStore, String> {
    let path = project_env_path();
    if !path.exists() {
        return Ok(ProjectEnvStore::new());
    }
    let data = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    if data.trim().is_empty() {
        return Ok(ProjectEnvStore::new());
    }
    serde_json::from_str::<ProjectEnvStore>(&data)
        .map_err(|e| format!("Invalid project-env.json: {}", e))
}

fn write_project_env_file(store: &ProjectEnvStore) -> Result<(), String> {
    let path = project_env_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = serde_json::to_string_pretty(store)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    // 0o600 applied to the temp file BEFORE rename so there is no
    // window where this file (which holds user secrets) exists at the
    // default umask 0o644.
    atomic_write_with_mode(&path, body.as_bytes(), Some(0o600))
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Best-effort lookup of env vars for a given project path. Swallows all errors
// and returns an empty vec — a broken env file must never break terminal spawn.
fn load_env_for_project(path: &str) -> Vec<(String, String)> {
    let Ok(store) = read_project_env_file() else { return Vec::new(); };
    let key = normalize_project_path(path);
    store
        .get(&key)
        .map(|vars| vars.iter().map(|v| (v.key.clone(), v.value.clone())).collect())
        .unwrap_or_default()
}

fn is_valid_env_key(key: &str) -> bool {
    // POSIX env var name: [A-Za-z_][A-Za-z0-9_]*
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[tauri::command]
fn load_project_env_vars(path: String) -> Result<Vec<ProjectEnvVar>, String> {
    let store = read_project_env_file()?;
    let key = normalize_project_path(&path);
    Ok(store.get(&key).cloned().unwrap_or_default())
}

// Read-modify-write of project-env.json. The caller MUST hold
// project_env_file_lock — these do no locking themselves so that
// remove_project (which already needs to mutate env under the lock) and the
// command wrappers share one implementation.
fn save_project_env_vars_locked(path: &str, vars: Vec<ProjectEnvVar>) -> Result<(), String> {
    let key = normalize_project_path(path);
    let mut store = read_project_env_file().unwrap_or_default();
    if vars.is_empty() {
        store.remove(&key);
    } else {
        store.insert(key, vars);
    }
    write_project_env_file(&store)
}

fn forget_project_env_locked(path: &str) -> Result<(), String> {
    let key = normalize_project_path(path);
    let mut store = match read_project_env_file() {
        Ok(s) => s,
        // Missing / unparseable file → nothing to forget. Don't propagate.
        Err(_) => return Ok(()),
    };
    if store.remove(&key).is_some() {
        write_project_env_file(&store)?;
    }
    Ok(())
}

#[tauri::command]
fn save_project_env_vars(
    path: String,
    vars: Vec<ProjectEnvVar>,
    state: State<AppState>,
) -> Result<(), String> {
    for v in &vars {
        if !is_valid_env_key(&v.key) {
            return Err(format!(
                "Invalid env var name: {:?} (must match [A-Za-z_][A-Za-z0-9_]*)",
                v.key
            ));
        }
    }
    // Hold the lock across read→mutate→write so two windows saving env for
    // different projects can't interleave and drop one's entire secret set.
    let _guard = state.project_env_file_lock.lock_safe();
    save_project_env_vars_locked(&path, vars)
}

#[tauri::command]
fn forget_project_env(path: String, state: State<AppState>) -> Result<(), String> {
    let _guard = state.project_env_file_lock.lock_safe();
    forget_project_env_locked(&path)
}

// Recursive file search for quick open
#[tauri::command]
async fn search_files(root: String, query: String, max_results: Option<usize>) -> Result<Vec<String>, String> {
    blocking(move || {
        let limit = max_results.unwrap_or(50);
        let query_lower = query.to_lowercase();
        let mut results = Vec::new();
        let root_path = std::path::Path::new(&root);

        fn walk(
            dir: &std::path::Path,
            root: &std::path::Path,
            query: &str,
            results: &mut Vec<String>,
            limit: usize,
            depth: usize,
        ) {
            if depth > 10 || results.len() >= limit {
                return;
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            // Sort siblings so walk order is deterministic. fs::read_dir's order
            // is filesystem-dependent; with a result limit, a non-deterministic
            // walk makes "which 20 files appear first" vary run-to-run.
            let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if results.len() >= limit {
                    return;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                // Skip hidden dirs and common large dirs
                if name.starts_with('.') || name == "node_modules" || name == "target" || name == "__pycache__" || name == "dist" || name == "build" {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, query, results, limit, depth + 1);
                } else if name.to_lowercase().contains(query) {
                    let relative = path.strip_prefix(root).unwrap_or(&path);
                    results.push(relative.to_string_lossy().to_string());
                }
            }
        }

        walk(root_path, root_path, &query_lower, &mut results, limit, 0);
        results.sort_by(|a, b| a.len().cmp(&b.len()));
        Ok(results)
    })
    .await
}

#[derive(Serialize)]
struct SearchHit {
    file: String,         // relative path from root
    line: u32,            // 1-indexed
    column: u32,          // 1-indexed column of match start (char index)
    match_length: u32,
    line_content: String, // full line, truncated to 500 chars
}

// Project-wide content search. Walks the tree applying the same skip rules as
// search_files, reads text files under a size cap, and reports matches.
#[tauri::command]
async fn search_in_files(
    root: String,
    query: String,
    case_sensitive: bool,
    is_regex: bool,
    max_results: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
    blocking(move || {
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let limit = max_results.unwrap_or(500);
        let root_path = std::path::Path::new(&root);

        // Compile pattern once.
        let pattern_src = if is_regex {
            query.clone()
        } else {
            regex::escape(&query)
        };
        let pattern = regex::RegexBuilder::new(&pattern_src)
            .case_insensitive(!case_sensitive)
            .build()
            .map_err(|e| format!("Invalid pattern: {}", e))?;

        let mut results: Vec<SearchHit> = Vec::new();

        fn walk(
            dir: &std::path::Path,
            root: &std::path::Path,
            pattern: &regex::Regex,
            results: &mut Vec<SearchHit>,
            limit: usize,
            depth: usize,
        ) {
            if depth > 12 || results.len() >= limit {
                return;
            }
            let entries = match fs::read_dir(dir) {
                Ok(e) => e,
                Err(_) => return,
            };
            // Deterministic walk (same reason as search_files): keeps the set
            // of matches visited before hitting `limit` stable across runs.
            let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if results.len() >= limit {
                    return;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.')
                    || name == "node_modules"
                    || name == "target"
                    || name == "__pycache__"
                    || name == "dist"
                    || name == "build"
                {
                    continue;
                }
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, root, pattern, results, limit, depth + 1);
                    continue;
                }
                // File size cap: skip anything over 2 MB to keep latency bounded.
                let metadata = match entry.metadata() {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                if metadata.len() > 2 * 1024 * 1024 {
                    continue;
                }
                let bytes = match fs::read(&path) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                // Binary detection: null byte in the first 512 bytes.
                if bytes.iter().take(512).any(|b| *b == 0) {
                    continue;
                }
                let content = match std::str::from_utf8(&bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().to_string();
                for (line_idx, line) in content.lines().enumerate() {
                    if results.len() >= limit {
                        return;
                    }
                    if let Some(m) = pattern.find(line) {
                        // Count chars up to byte offset for 1-indexed column.
                        let prefix = &line[..m.start()];
                        let column = prefix.chars().count() as u32 + 1;
                        let match_length = line[m.start()..m.end()].chars().count() as u32;
                        let line_content = if line.len() > 500 {
                            let mut end = 500;
                            while !line.is_char_boundary(end) && end > 0 {
                                end -= 1;
                            }
                            format!("{}…", &line[..end])
                        } else {
                            line.to_string()
                        };
                        results.push(SearchHit {
                            file: rel.clone(),
                            line: (line_idx as u32) + 1,
                            column,
                            match_length,
                            line_content,
                        });
                    }
                }
            }
        }

        walk(root_path, root_path, &pattern, &mut results, limit, 0);
        Ok(results)
    })
    .await
}

#[tauri::command]
fn pick_directory() -> Result<Option<String>, String> {
    let output = std::process::Command::new("osascript")
        .args(["-e", "POSIX path of (choose folder)"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        // Remove trailing slash
        let path = path.trim_end_matches('/').to_string();
        Ok(Some(path))
    } else {
        // User cancelled
        Ok(None)
    }
}

#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    Ok(())
}

// Hand `path` off to the OS to open with the user's default application
// for that file type — the macOS `open` CLI does this. PDF → Preview,
// PNG → Preview, .xcodeproj → Xcode, etc. No-op for paths that don't
// resolve; we surface the `open` command's exit status so the frontend
// can show the actual error rather than a silent failure.
#[tauri::command]
fn open_in_default_app(path: String) -> Result<(), String> {
    let output = std::process::Command::new("open")
        .arg(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    Ok(())
}

#[tauri::command]
async fn read_file_preview(path: String, max_bytes: Option<usize>) -> Result<String, String> {
    blocking(move || {
        let limit = max_bytes.unwrap_or(8192);

        // Stream only up to `limit` bytes so a 50 MB file opened for a 512 KB
        // preview doesn't allocate 50 MB transiently.
        let file = fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut data = Vec::with_capacity(limit.min(64 * 1024));
        use std::io::Read;
        file.take(limit as u64)
            .read_to_end(&mut data)
            .map_err(|e| e.to_string())?;

        // UTF-16 BOM: surface a specific, actionable message instead of a
        // generic "binary" refusal. UTF-16 ASCII-dominant text used to trip
        // the old null-byte check as a false positive.
        if data.len() >= 2
            && (data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]))
        {
            return Err("UTF-16 file — re-save as UTF-8 to edit".into());
        }

        // Binary heuristic: real text files rarely contain ANY null bytes, but
        // tolerate a small ratio so source files with null-in-string-literal
        // don't get rejected. UTF-16-like content will have ~50% nulls, far
        // above the 1% threshold. Only apply once we have enough bytes to
        // make the ratio meaningful — a 20-byte file with one null otherwise
        // looks like "5% null" and would be wrongly rejected.
        if data.len() > 64 {
            let null_count = data.iter().filter(|&&b| b == 0).count();
            if null_count * 100 > data.len() {
                return Err("Binary file — cannot display".into());
            }
        }

        Ok(String::from_utf8_lossy(&data).to_string())
    })
    .await
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
    let preserved_mode = fs::metadata(dest).ok().map(|m| m.permissions().mode());
    let effective_mode = explicit_mode.or(preserved_mode);

    let result = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
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
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(mode));
        }
        fs::rename(&tmp, dest)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[tauri::command]
async fn write_file(path: String, content: String) -> Result<(), String> {
    // Size limit to prevent accidental large writes. Checked before
    // spawn_blocking so the rejection round-trip is cheap.
    if content.len() > 10 * 1024 * 1024 {
        return Err("File content exceeds 10MB limit".into());
    }
    blocking(move || {
        atomic_write(std::path::Path::new(&path), content.as_bytes()).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn create_file(path: String) -> Result<(), String> {
    blocking(move || {
        if std::path::Path::new(&path).exists() {
            return Err("File already exists".into());
        }
        std::fs::write(&path, "").map_err(|e| e.to_string())
    })
    .await
}

#[tauri::command]
async fn create_directory(path: String) -> Result<(), String> {
    blocking(move || std::fs::create_dir_all(&path).map_err(|e| e.to_string())).await
}

#[tauri::command]
async fn delete_path(path: String, project_root: Option<String>) -> Result<(), String> {
    // Off-thread because remove_dir_all on a large subtree (e.g. accidental
    // delete of node_modules/) can take seconds and would otherwise block
    // the IPC dispatch thread.
    blocking(move || {
        // Refuse any deletion not proven to be inside a project root. Every
        // caller (the file-browser context menu) passes project_root, so a
        // missing root means an unexpected/foreign caller — refuse rather than
        // reach remove_dir_all unguarded. With a root, the target must canonicalize
        // to a path inside it.
        let root = project_root.ok_or("Refusing to delete: no project root supplied")?;
        let target = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        let root_canon = fs::canonicalize(&root).map_err(|e| e.to_string())?;
        if !target.starts_with(&root_canon) {
            return Err("Refusing to delete path outside project root".into());
        }
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| e.to_string())
        } else {
            std::fs::remove_file(&path).map_err(|e| e.to_string())
        }
    })
    .await
}

#[tauri::command]
async fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    blocking(move || std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())).await
}

// Inode of the file at `path`. Used to relocate editor tabs whose file
// was renamed externally (Finder, `mv`, git operations). Unix rename
// preserves the inode, so capturing it at open time lets us search for
// the same file under its new name when the original path disappears.
#[tauri::command]
async fn get_file_inode(path: String) -> Result<u64, String> {
    blocking(move || {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&path).map(|m| m.ino()).map_err(|e| e.to_string())
    })
    .await
}

// Walk the project tree looking for a file whose inode matches `inode`.
// Returns the first match. Applies the same skip rules as search_files
// (hidden dirs, node_modules, target, etc.) so a rename outside the
// interesting tree doesn't cost us a giant walk.
#[tauri::command]
async fn find_path_by_inode(root: String, inode: u64) -> Result<Option<String>, String> {
    blocking(move || {
        fn walk(
            dir: &std::path::Path,
            inode: u64,
            found: &mut Option<String>,
            depth: usize,
        ) {
            use std::os::unix::fs::MetadataExt;
            // Bounds match search_files. Project root is depth 0, so this
            // stops before descending into depth 13 (i.e. walks up to 12
            // levels below the root).
            if found.is_some() || depth > 12 {
                return;
            }
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            let mut sorted: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            sorted.sort_by_key(|e| e.file_name());
            for entry in sorted {
                if found.is_some() {
                    return;
                }
                // Avoid an allocation per entry — name is only used for the
                // skip-list check, and Cow<str> compares cleanly.
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.starts_with('.')
                    || name_str == "node_modules"
                    || name_str == "target"
                    || name_str == "__pycache__"
                    || name_str == "dist"
                    || name_str == "build"
                {
                    continue;
                }
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.is_dir() {
                    walk(&entry.path(), inode, found, depth + 1);
                } else if metadata.ino() == inode {
                    *found = Some(entry.path().to_string_lossy().into_owned());
                    return;
                }
            }
        }

        let mut found = None;
        walk(std::path::Path::new(&root), inode, &mut found, 0);
        Ok(found)
    })
    .await
}

#[derive(Clone, Serialize)]
struct DiffLine {
    old_line_no: i32,
    new_line_no: i32,
    content: String,
    origin: String,
}

#[derive(Clone, Serialize)]
struct HunkDiff {
    old_start: u32,
    new_start: u32,
    old_lines: u32,
    new_lines: u32,
    lines: Vec<DiffLine>,
}

#[derive(Clone, Serialize)]
struct FileDiff {
    old_path: Option<String>,
    new_path: Option<String>,
    hunks: Vec<HunkDiff>,
}

fn collect_structured_diff(diff: &git2::Diff) -> Result<FileDiff, String> {
    let old_path = diff.deltas().next()
        .and_then(|d| d.old_file().path().map(|p| p.to_string_lossy().to_string()));
    let new_path = diff.deltas().next()
        .and_then(|d| d.new_file().path().map(|p| p.to_string_lossy().to_string()));

    let hunks = std::cell::RefCell::new(Vec::<HunkDiff>::new());
    let current_lines = std::cell::RefCell::new(Vec::<DiffLine>::new());
    let current_hunk = std::cell::RefCell::new(Option::<(u32, u32, u32, u32)>::None);

    diff.foreach(
        &mut |_delta, _progress| true,
        None,
        Some(&mut |_delta, hunk| {
            if let Some((old_start, new_start, old_lines, new_lines)) = current_hunk.borrow_mut().take() {
                hunks.borrow_mut().push(HunkDiff {
                    old_start, new_start, old_lines, new_lines,
                    lines: current_lines.borrow_mut().drain(..).collect(),
                });
            }
            *current_hunk.borrow_mut() = Some((
                hunk.old_start(), hunk.new_start(), hunk.old_lines(), hunk.new_lines(),
            ));
            true
        }),
        Some(&mut |_delta, _hunk, line| {
            let origin = match line.origin() {
                '+' => "add",
                '-' => "remove",
                _ => "context",
            }.to_string();
            current_lines.borrow_mut().push(DiffLine {
                old_line_no: line.old_lineno().map(|n| n as i32).unwrap_or(-1),
                new_line_no: line.new_lineno().map(|n| n as i32).unwrap_or(-1),
                content: String::from_utf8_lossy(line.content()).to_string(),
                origin,
            });
            true
        }),
    ).map_err(|e| e.to_string())?;

    // Flush the last hunk
    if let Some((old_start, new_start, old_lines, new_lines)) = current_hunk.borrow_mut().take() {
        hunks.borrow_mut().push(HunkDiff {
            old_start, new_start, old_lines, new_lines,
            lines: current_lines.borrow_mut().drain(..).collect(),
        });
    }

    Ok(FileDiff { old_path, new_path, hunks: hunks.into_inner() })
}

// Walks a libgit2 Diff that may span multiple files (e.g. tree-to-tree
// between two refs) and returns one FileDiff per delta. Same line/hunk
// shape as collect_structured_diff so the frontend renderer is unchanged.
fn collect_structured_files_diff(diff: &git2::Diff) -> Result<Vec<FileDiff>, String> {
    use std::cell::RefCell;

    let files: RefCell<Vec<FileDiff>> = RefCell::new(Vec::new());
    let current_file: RefCell<Option<FileDiff>> = RefCell::new(None);
    let current_hunk: RefCell<Option<(u32, u32, u32, u32)>> = RefCell::new(None);
    let current_lines: RefCell<Vec<DiffLine>> = RefCell::new(Vec::new());

    diff.foreach(
        &mut |delta, _progress| {
            // file_cb fires once per file, before any hunk_cb for that file.
            // Flush any pending hunk into the previous file, push it, then
            // open a new accumulator for this delta.
            let mut cur = current_file.borrow_mut();
            if let Some(mut prev) = cur.take() {
                if let Some((os, ns, ol, nl)) = current_hunk.borrow_mut().take() {
                    prev.hunks.push(HunkDiff {
                        old_start: os, new_start: ns, old_lines: ol, new_lines: nl,
                        lines: current_lines.borrow_mut().drain(..).collect(),
                    });
                }
                files.borrow_mut().push(prev);
            }
            *cur = Some(FileDiff {
                old_path: delta.old_file().path().map(|p| p.to_string_lossy().to_string()),
                new_path: delta.new_file().path().map(|p| p.to_string_lossy().to_string()),
                hunks: Vec::new(),
            });
            true
        },
        None,
        Some(&mut |_delta, hunk| {
            // Flush previous hunk into the current file, then open new one.
            if let Some(file) = current_file.borrow_mut().as_mut() {
                if let Some((os, ns, ol, nl)) = current_hunk.borrow_mut().take() {
                    file.hunks.push(HunkDiff {
                        old_start: os, new_start: ns, old_lines: ol, new_lines: nl,
                        lines: current_lines.borrow_mut().drain(..).collect(),
                    });
                }
            }
            *current_hunk.borrow_mut() = Some((
                hunk.old_start(), hunk.new_start(), hunk.old_lines(), hunk.new_lines(),
            ));
            true
        }),
        Some(&mut |_delta, _hunk, line| {
            let origin = match line.origin() {
                '+' => "add",
                '-' => "remove",
                _ => "context",
            }.to_string();
            current_lines.borrow_mut().push(DiffLine {
                old_line_no: line.old_lineno().map(|n| n as i32).unwrap_or(-1),
                new_line_no: line.new_lineno().map(|n| n as i32).unwrap_or(-1),
                content: String::from_utf8_lossy(line.content()).to_string(),
                origin,
            });
            true
        }),
    ).map_err(|e| e.to_string())?;

    // Flush trailing hunk + file
    if let Some(mut last) = current_file.borrow_mut().take() {
        if let Some((os, ns, ol, nl)) = current_hunk.borrow_mut().take() {
            last.hunks.push(HunkDiff {
                old_start: os, new_start: ns, old_lines: ol, new_lines: nl,
                lines: current_lines.borrow_mut().drain(..).collect(),
            });
        }
        files.borrow_mut().push(last);
    }

    Ok(files.into_inner())
}

#[derive(Clone, Serialize)]
struct RefDiffStats {
    files_changed: usize,
    additions: usize,
    deletions: usize,
}

#[derive(Clone, Serialize)]
struct RefDiff {
    from_ref: String,
    to_ref: String,
    files: Vec<FileDiff>,
    stats: RefDiffStats,
}

#[tauri::command]
async fn get_diff_between_refs(
    path: String,
    from_ref: String,
    to_ref: String,
) -> Result<RefDiff, String> {
    blocking(move || {
        if !(is_valid_git_ref(&from_ref) || is_valid_git_oid(&from_ref)) {
            return Err("Invalid `from` ref".into());
        }
        if !(is_valid_git_ref(&to_ref) || is_valid_git_oid(&to_ref)) {
            return Err("Invalid `to` ref".into());
        }

        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let from_obj = repo.revparse_single(&from_ref).map_err(|e| format!("from `{}`: {}", from_ref, e))?;
        let to_obj = repo.revparse_single(&to_ref).map_err(|e| format!("to `{}`: {}", to_ref, e))?;
        let from_tree = from_obj.peel_to_tree().map_err(|e| format!("from `{}` peel: {}", from_ref, e))?;
        let to_tree = to_obj.peel_to_tree().map_err(|e| format!("to `{}` peel: {}", to_ref, e))?;

        let mut diff_opts = git2::DiffOptions::new();
        let diff = repo
            .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), Some(&mut diff_opts))
            .map_err(|e| e.to_string())?;

        let files = collect_structured_files_diff(&diff)?;
        let mut additions = 0usize;
        let mut deletions = 0usize;
        for f in &files {
            for h in &f.hunks {
                for l in &h.lines {
                    match l.origin.as_str() {
                        "add" => additions += 1,
                        "remove" => deletions += 1,
                        _ => {}
                    }
                }
            }
        }
        let files_changed = files.len();
        Ok(RefDiff {
            from_ref,
            to_ref,
            files,
            stats: RefDiffStats { files_changed, additions, deletions },
        })
    })
    .await
}

#[tauri::command]
async fn get_file_diff(path: String, file_path: String, staged: Option<bool>) -> Result<FileDiff, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let mut diff_opts = git2::DiffOptions::new();
        diff_opts.pathspec(&file_path);

        if staged.unwrap_or(false) {
            // Staged diff: index vs HEAD
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            let diff = repo
                .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
                .map_err(|e| e.to_string())?;
            collect_structured_diff(&diff)
        } else {
            // Unstaged diff: workdir vs index
            let diff = repo
                .diff_index_to_workdir(None, Some(&mut diff_opts))
                .map_err(|e| e.to_string())?;
            let result = collect_structured_diff(&diff)?;
            if !result.hunks.is_empty() {
                return Ok(result);
            }
            // Fall back to staged if no unstaged changes (backwards compat)
            let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
            let diff = repo
                .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
                .map_err(|e| e.to_string())?;
            collect_structured_diff(&diff)
        }
    })
    .await
}

#[tauri::command]
async fn git_stage_all(path: String) -> Result<(), String> {
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
async fn git_stage_file(path: String, file_path: String) -> Result<(), String> {
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
async fn git_commit(path: String, message: String) -> Result<String, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        let mut index = repo.index().map_err(|e| e.to_string())?;
        let tree_oid = index.write_tree().map_err(|e| e.to_string())?;
        let tree = repo.find_tree(tree_oid).map_err(|e| e.to_string())?;

        let sig = repo.signature().map_err(|e| e.to_string())?;

        let parent = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_commit().ok());

        let parents: Vec<&git2::Commit> = parent.iter().collect();

        let oid = repo
            .commit(Some("HEAD"), &sig, &sig, &message, &tree, &parents)
            .map_err(|e| e.to_string())?;

        Ok(oid.to_string()[..7].to_string())
    })
    .await
}

#[derive(Clone, Serialize)]
struct AmendResult {
    oid: String,
    requires_force_push: bool,
}

// True when HEAD's commit is reachable from any refs/remotes/* — i.e. an
// amend will rewrite a commit that already exists on a remote, so the user
// will need a force-push to share the new version.
fn head_is_on_any_remote(repo: &Repository, head_oid: git2::Oid) -> Result<bool, String> {
    let refs = repo.references_glob("refs/remotes/*").map_err(|e| e.to_string())?;
    for r in refs.flatten() {
        let target = match r.target() {
            Some(t) => t,
            None => continue,
        };
        if target == head_oid {
            return Ok(true);
        }
        if matches!(repo.graph_descendant_of(target, head_oid), Ok(true)) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[tauri::command]
async fn git_head_on_remote(path: String) -> Result<bool, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let head = match repo.head() {
            Ok(h) => h,
            Err(_) => return Ok(false), // unborn HEAD
        };
        let head_oid = head.peel_to_commit().map_err(|e| e.to_string())?.id();
        head_is_on_any_remote(&repo, head_oid)
    })
    .await
}

#[tauri::command]
async fn git_amend_commit(
    path: String,
    message: Option<String>,
    include_staged: bool,
) -> Result<AmendResult, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

        // Refuse on unborn or detached HEAD — both leave no clear branch to
        // amend onto, and silently rewriting a detached HEAD is dangerous.
        if matches!(repo.head_detached(), Ok(true)) {
            return Err("HEAD is detached — switch to a branch before amending".into());
        }
        let head_ref = repo
            .head()
            .map_err(|_| "HEAD is unborn — nothing to amend".to_string())?;
        let head_commit = head_ref.peel_to_commit().map_err(|e| e.to_string())?;
        let head_oid = head_commit.id();

        let requires_force_push = head_is_on_any_remote(&repo, head_oid)?;

        // Tree: from index when include_staged, else keep HEAD's tree.
        let new_tree_oid = if include_staged {
            let mut index = repo.index().map_err(|e| e.to_string())?;
            index.write_tree().map_err(|e| e.to_string())?
        } else {
            head_commit.tree().map_err(|e| e.to_string())?.id()
        };
        let new_tree = repo.find_tree(new_tree_oid).map_err(|e| e.to_string())?;

        let sig = repo.signature().map_err(|e| e.to_string())?;
        let new_oid = head_commit
            .amend(
                Some("HEAD"),
                Some(&sig),
                Some(&sig),
                None,
                message.as_deref(),
                Some(&new_tree),
            )
            .map_err(|e| e.to_string())?;

        Ok(AmendResult {
            oid: new_oid.to_string(),
            requires_force_push,
        })
    })
    .await
}

struct GitOpSlot {
    op_id: u64,
    pid: Option<u32>,
    cancelled: bool,
}

// Per-window git op tracking, keyed by `tauri::Window::label()`. At most one
// entry per window; absent key means no in-flight op for that window.
type GitOpSlots = HashMap<String, GitOpSlot>;

// SIGKILL, not SIGTERM. A git process blocked on a stalled TCP recv() often
// ignores SIGTERM — which is the exact situation cancel is supposed to
// escape — so we skip the polite signal and go straight to -9.
fn kill_pid_hard(pid: u32) {
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
static SSH_AUTH_SOCK_CACHE: Mutex<Option<(Option<String>, Instant)>> = Mutex::new(None);
const SSH_AUTH_SOCK_NEGATIVE_TTL: Duration = Duration::from_secs(60);

fn resolve_ssh_auth_sock() -> Option<String> {
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

fn compute_ssh_auth_sock() -> Option<String> {
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
fn apply_git_env(cmd: &mut std::process::Command) {
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
fn spawn_git_under_slot(
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
fn run_git_cancellable(
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
fn git_upstream_status(path: &str) -> (bool, Option<String>) {
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
async fn git_push(
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
async fn git_pull(
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
async fn git_fetch(
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
fn cancel_git_op_inner(slots: &Mutex<GitOpSlots>, label: &str) -> Option<u32> {
    let mut guard = slots.lock_safe();
    if let Some(slot) = guard.get_mut(label) {
        slot.cancelled = true;
        return slot.pid;
    }
    None
}

#[tauri::command]
fn cancel_git_op(state: State<AppState>, window: tauri::Window) -> Result<(), String> {
    if let Some(pid) = cancel_git_op_inner(&state.git_op_slots, window.label()) {
        kill_pid_hard(pid);
    }
    Ok(())
}

fn is_valid_git_ref(name: &str) -> bool {
    // Reject empties and leading `-` (option injection: `--no-verify`).
    // Reject `..` (git ref-format forbids it; also blocks range syntax sneaking through).
    // Allow [A-Za-z0-9._/-] plus `^` and `~` for parent/ancestor rev syntax
    // (HEAD^, abc1234~3). These are not shell-special at argv level — we
    // pass refs as discrete Command::arg arguments, never through `sh -c`.
    if name.is_empty() || name.starts_with('-') {
        return false;
    }
    if name.contains("..") {
        return false;
    }
    name.chars()
        .all(|c| c.is_alphanumeric() || "._/-^~".contains(c))
}

fn is_valid_git_oid(s: &str) -> bool {
    // Accepts 4–40 hex chars (matches `git`'s short-OID accept range; SHA-1 is 40).
    let len = s.len();
    if !(4..=40).contains(&len) {
        return false;
    }
    s.chars().all(|c| c.is_ascii_hexdigit())
}

#[tauri::command]
async fn git_merge_branch(
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

#[derive(Clone, Serialize)]
struct CherryPickResult {
    ok: bool,
    conflicted_files: Vec<String>,
}

// List paths in working tree marked as conflicted by libgit2's status walk.
fn list_conflict_files(path: &str) -> Vec<String> {
    match get_git_status_inner(path) {
        Ok(info) => info.files.into_iter()
            .filter(|f| f.status == "conflict")
            .map(|f| f.path)
            .collect(),
        Err(_) => Vec::new(),
    }
}

// Path to `.git` for `path`. Repository::path() includes the trailing slash.
fn git_dir(path: &str) -> Result<std::path::PathBuf, String> {
    let repo = Repository::discover(path).map_err(|e| e.to_string())?;
    Ok(repo.path().to_path_buf())
}

#[tauri::command]
async fn git_cherry_pick(
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
async fn git_cherry_pick_abort(
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
async fn git_cherry_pick_continue(
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

#[derive(Clone, Serialize)]
struct PendingOpState {
    kind: String,
    current_step: Option<u32>,
    total_steps: Option<u32>,
    head_message: Option<String>,
}

// Single libgit2-driven query the frontend polls each cycle. Detects the
// three rebase-family operations that leave state files on disk:
//   merge        → MERGE_HEAD
//   cherry-pick  → CHERRY_PICK_HEAD
//   rebase       → rebase-merge/ or rebase-apply/ directory
// Returns "none" when the working tree is clean of pending state. The
// frontend banner is the only consumer.
#[tauri::command]
async fn get_pending_op_state(path: String) -> Result<PendingOpState, String> {
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
struct RebaseCandidateCommit {
    oid: String,
    short_oid: String,
    message: String,
    author: String,
    timestamp: i64,
    on_remote: bool,
}

#[derive(Clone, Serialize)]
struct RebaseCandidates {
    commits: Vec<RebaseCandidateCommit>,
    upstream_known: bool,
}

#[derive(Clone, Deserialize, Serialize)]
struct RebaseTodoEntry {
    action: String, // "pick" | "reword" | "squash" | "fixup" | "drop" | "edit"
    oid: String,
    new_message: Option<String>,
}

#[derive(Clone, Serialize)]
struct RebaseResult {
    ok: bool,
    stopped_at: Option<String>,
    conflicted_files: Vec<String>,
    completed: bool,
    backup_tag: String,
}

const VALID_REBASE_ACTIONS: &[&str] = &["pick", "reword", "squash", "fixup", "drop", "edit"];

#[tauri::command]
async fn get_rebase_candidate_commits(
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
fn create_rebase_backup_tag(repo: &Repository) -> Result<String, String> {
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

fn delete_rebase_backup_tag(repo_path: &str, tag_name: &str) {
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
fn create_rebase_state_dir(todo: &[RebaseTodoEntry]) -> Result<std::path::PathBuf, String> {
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
    let dir = temp.into_path();
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

fn cleanup_rebase_state_dir(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

// True when git's rebase state files indicate the rebase has finished
// (state files removed). We poll this after each command result to decide
// whether to clean up the state dir + (optionally) the backup tag.
fn rebase_in_progress(repo_path: &str) -> bool {
    let gd = match git_dir(repo_path) {
        Ok(d) => d,
        Err(_) => return false,
    };
    gd.join("rebase-merge").is_dir() || gd.join("rebase-apply").is_dir()
}

#[tauri::command]
async fn git_rebase_interactive_apply(
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
fn read_rebase_stopped_sha(repo_path: &str) -> Option<String> {
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

fn spawn_rebase(
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
fn finalize_rebase(rebase_state: &Arc<Mutex<Option<RebaseStateInfo>>>, repo_path: &str, delete_tag: bool) {
    let info = rebase_state.lock_safe().take();
    if let Some(info) = info {
        cleanup_rebase_state_dir(&info.state_dir);
        if delete_tag {
            delete_rebase_backup_tag(repo_path, &info.backup_tag);
        }
    }
}

fn run_git_with_rebase_env(
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
async fn git_rebase_continue(
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
async fn git_rebase_skip(
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
async fn git_rebase_abort(
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

#[derive(Clone, Serialize)]
struct ConflictVersions {
    // Each version is None when the corresponding stage is missing (e.g.
    // a file added on both sides has no base; deleted-by-us has no ours).
    // Working tree's `merged` is None when the file isn't on disk.
    base: Option<String>,
    ours: Option<String>,
    theirs: Option<String>,
    merged: Option<String>,
}

// True when `p` is a repo-relative path that stays inside the repo: not
// absolute and with no `..` components. libgit2 always hands us such paths,
// but get_conflict_versions is IPC-reachable with arbitrary args, so we guard
// the working-tree read against `../../etc/passwd`-style escapes before
// joining onto the repo root.
fn is_safe_relative_path(p: &str) -> bool {
    use std::path::Component;
    let path = std::path::Path::new(p);
    if path.is_absolute() {
        return false;
    }
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

// Reads index stages 1 (base), 2 (ours), 3 (theirs) for a conflicted file
// and the working-tree contents (`merged`). Used by the 3-pane merge tab.
// Stages encode their role in libgit2's index: 0 = normal, 1 = base,
// 2 = ours, 3 = theirs. A file may be missing any of 1/2/3 depending on
// which side added/deleted it.
#[tauri::command]
async fn get_conflict_versions(path: String, file_path: String) -> Result<ConflictVersions, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let index = repo.index().map_err(|e| e.to_string())?;

        let path_bytes = file_path.as_bytes();
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
async fn git_resolve_ours(path: String, file_path: String) -> Result<(), String> {
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
async fn git_resolve_theirs(path: String, file_path: String) -> Result<(), String> {
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

#[tauri::command]
async fn git_unstage_file(path: String, file_path: String) -> Result<(), String> {
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
async fn git_unstage_all(path: String) -> Result<(), String> {
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
async fn git_discard_file(path: String, file_path: String) -> Result<(), String> {
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
            let meta = fs::metadata(&abs).map_err(|e| e.to_string())?;
            if meta.is_dir() {
                fs::remove_dir_all(&abs).map_err(|e| e.to_string())?;
            } else {
                fs::remove_file(&abs).map_err(|e| e.to_string())?;
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

#[tauri::command]
async fn git_stash_save(path: String) -> Result<(), String> {
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
async fn git_stash_pop(path: String) -> Result<(), String> {
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
struct StashEntry {
    index: usize,
    message: String,
}

#[tauri::command]
async fn git_stash_list(path: String) -> Result<Vec<StashEntry>, String> {
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
async fn git_stash_apply(path: String, index: usize) -> Result<(), String> {
    blocking(move || {
        let mut repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        repo.stash_apply(index, None)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn git_stash_drop(path: String, index: usize) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut repo = repo;
        repo.stash_drop(index)
            .map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn git_delete_branch(path: String, branch_name: String) -> Result<(), String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let mut branch = repo
            .find_branch(&branch_name, BranchType::Local)
            .map_err(|e| e.to_string())?;
        branch.delete().map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
}

#[tauri::command]
async fn list_remote_branches(path: String) -> Result<Vec<BranchInfo>, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let branches = repo
            .branches(Some(BranchType::Remote))
            .map_err(|e| e.to_string())?;

        let result: Vec<BranchInfo> = branches
            .filter_map(|b| {
                let (branch, _) = b.ok()?;
                let full_name = branch.name().ok()??.to_string();
                // Skip HEAD pointer
                if full_name.ends_with("/HEAD") {
                    return None;
                }
                // Strip "origin/" prefix for display
                let name = full_name
                    .strip_prefix("origin/")
                    .unwrap_or(&full_name)
                    .to_string();

                let commit = branch.get().peel_to_commit().ok();
                let last_commit_msg = commit
                    .as_ref()
                    .and_then(|c| c.summary().map(String::from));
                let last_commit_time = commit.as_ref().map(|c| c.time().seconds());

                Some(BranchInfo {
                    name,
                    is_current: false,
                    last_commit_msg,
                    last_commit_time,
                    upstream: None,
                })
            })
            .collect();

        Ok(result)
    })
    .await
}

#[tauri::command]
async fn get_commit_details(path: String, oid: String) -> Result<CommitDetail, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let obj = repo.revparse_single(&oid).map_err(|e| e.to_string())?;
        let commit = obj.peel_to_commit().map_err(|e| e.to_string())?;

        let tree = commit.tree().map_err(|e| e.to_string())?;
        let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

        let diff = repo
            .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None)
            .map_err(|e| e.to_string())?;

        // First pass: build file list from deltas
        let mut files: Vec<CommitFileStat> = Vec::new();
        for idx in 0..diff.deltas().len() {
            let delta = diff.get_delta(idx).unwrap();
            let file_path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let status = match delta.status() {
                git2::Delta::Added => "added",
                git2::Delta::Deleted => "deleted",
                git2::Delta::Modified => "modified",
                git2::Delta::Renamed => "renamed",
                git2::Delta::Copied => "copied",
                _ => "modified",
            };
            files.push(CommitFileStat {
                path: file_path,
                additions: 0,
                deletions: 0,
                status: status.to_string(),
            });
        }

        // Second pass: count additions/deletions per file via Patch
        let num_deltas = diff.deltas().len();
        for i in 0..num_deltas {
            if let Ok(patch) = git2::Patch::from_diff(&diff, i) {
                if let Some(patch) = patch {
                    let (_, additions, deletions) = patch.line_stats().unwrap_or((0, 0, 0));
                    files[i].additions = additions;
                    files[i].deletions = deletions;
                }
            }
        }

        let message = commit.summary().unwrap_or("(no message)").to_string();
        let author = commit.author().name().unwrap_or("Unknown").to_string();
        let timestamp = commit.time().seconds();
        let short_oid = commit.id().to_string()[..7].to_string();

        Ok(CommitDetail {
            oid: short_oid,
            message,
            author,
            timestamp,
            files,
        })
    })
    .await
}

#[tauri::command]
async fn get_remote_url(path: String) -> Result<String, String> {
    blocking(move || {
        let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
        let remote = repo.find_remote("origin").map_err(|e| e.to_string())?;
        let url = remote.url().ok_or("No URL for origin")?.to_string();
        Ok(url)
    })
    .await
}

#[derive(Clone, Serialize)]
struct CommitFileStat {
    path: String,
    additions: usize,
    deletions: usize,
    status: String,
}

#[derive(Clone, Serialize)]
struct CommitDetail {
    oid: String,
    message: String,
    author: String,
    timestamp: i64,
    files: Vec<CommitFileStat>,
}

#[tauri::command]
fn get_home_dir() -> Result<String, String> {
    dirs_home()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine home directory".to_string())
}

#[tauri::command]
fn watch_directory(
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
fn unwatch_directory(path: String, state: State<AppState>) -> Result<(), String> {
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

#[tauri::command]
fn open_new_window(path: Option<String>, app: tauri::AppHandle) -> Result<(), String> {
    let label = format!(
        "window-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );

    // Project windows boot with ?folder=; bare windows land on the picker.
    let url = match &path {
        Some(p) => format!("index.html?folder={}", urlencoding::encode(p)),
        None => "index.html".to_string(),
    };

    let title = path
        .as_deref()
        .and_then(|p| p.rsplit('/').next())
        .unwrap_or("Launchpad");

    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::App(url.into()))
        .title(title)
        .inner_size(1200.0, 800.0)
        .min_inner_size(800.0, 500.0)
        .build()
        .map_err(|e| e.to_string())?;

    // Registration is handled by the frontend when `enterWorkspace` runs, so
    // both "new window with ?folder=" and "current window takes over a project"
    // register through the same path.
    Ok(())
}

#[tauri::command]
fn register_project_window(
    path: String,
    label: String,
    state: State<AppState>,
) -> Result<(), String> {
    let canonical = normalize_project_path(&path);
    if let Ok(mut map) = state.project_windows.lock() {
        // Drop any stale entry that was registered under this same label (e.g. a
        // different project previously held this window) so a label only points
        // at one project at a time.
        map.retain(|_, v| v != &label);
        map.insert(canonical, label);
    }
    Ok(())
}

#[tauri::command]
fn unregister_project_window(
    path: String,
    state: State<AppState>,
) -> Result<(), String> {
    let canonical = normalize_project_path(&path);
    if let Ok(mut map) = state.project_windows.lock() {
        map.remove(&canonical);
    }
    Ok(())
}

/// Remove every entry in the map whose value equals `label`. Called when a
/// window enters picker mode so a stale registration (from Cmd+R, a crash,
/// or any non-"← Projects" path) doesn't make focus_project_window think
/// this window is still hosting a project.
#[tauri::command]
fn unregister_window_label(
    label: String,
    state: State<AppState>,
) -> Result<(), String> {
    if let Ok(mut map) = state.project_windows.lock() {
        map.retain(|_, v| v != &label);
    }
    Ok(())
}

#[tauri::command]
fn focus_project_window(
    path: String,
    app: tauri::AppHandle,
    state: State<AppState>,
) -> Result<bool, String> {
    let canonical = normalize_project_path(&path);
    // Single critical section so no other thread can insert a fresh entry
    // between our read and the stale-remove in the `None` branch.
    let mut map = state
        .project_windows
        .lock()
        .map_err(|e| format!("project_windows lock poisoned: {}", e))?;
    let Some(label) = map.get(&canonical).cloned() else {
        return Ok(false);
    };

    match app.get_webview_window(&label) {
        Some(win) => {
            // show() first in case the window is hidden/minimized; focus() alone doesn't restore.
            let _ = win.show();
            win.set_focus().map_err(|e| e.to_string())?;
            Ok(true)
        }
        None => {
            // Stale entry — the window was closed. Drop it so the caller can open fresh.
            map.remove(&canonical);
            Ok(false)
        }
    }
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
            get_file_inode,
            find_path_by_inode,
            get_file_diff,
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
            unregister_window_label
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

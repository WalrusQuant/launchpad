use git2::{BranchType, Repository, StatusOptions};
use notify_debouncer_mini::{new_debouncer, notify};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, State};

type FsWatcher = notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>;

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
    // In-flight git network operation (push/pull/fetch). The slot is reserved
    // BEFORE spawn so a cancel arriving in the window between spawn() and
    // PID-record is still observed and applied. `op_id` (from git_op_counter)
    // tags each op so cleanup can tell whether the slot still belongs to us
    // or has been claimed by a later operation.
    git_op_pid: Arc<Mutex<Option<GitOpSlot>>>,
    git_op_counter: Arc<AtomicU64>,
    // Canonicalized project path → Tauri window label. Used by focus_project_window
    // to focus an existing window instead of opening a duplicate. Stale entries are
    // cleaned lazily on focus attempt (when get_webview_window returns None).
    project_windows: Arc<Mutex<HashMap<String, String>>>,
    // Serializes read→mutate→write of ~/.launchpad/projects.json so concurrent
    // commands (e.g. two windows both hitting add_project) can't lose updates.
    projects_file_lock: Arc<Mutex<()>>,
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
        let _ = handle2.emit("pty-exit", PtyExit { tab_id });
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
                        let _ = handle.emit("pty-output", PtyOutput { tab_id, data: output });
                        leftover = leftover[valid_up_to..].to_vec();
                    } else if leftover.len() > 64 * 1024 {
                        // A valid UTF-8 continuation is at most 3 bytes; if we
                        // have 64KB of invalid bytes with no valid prefix, the
                        // stream is corrupted (binary garbage, crashed shell,
                        // wrong encoding). Flush as replacement characters so
                        // the buffer can't grow without bound.
                        let output = String::from_utf8_lossy(&leftover).into_owned();
                        let _ = handle.emit("pty-output", PtyOutput { tab_id, data: output });
                        leftover.clear();
                    }
                }
                Err(_) => break,
            }
        }
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
    let home = dirs_home().ok_or_else(|| "no HOME".to_string())?;
    let dir = home.join(".launchpad");
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
fn read_directory(path: String) -> Result<Vec<FileEntry>, String> {
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

#[tauri::command]
fn get_git_status(path: String) -> Result<GitInfo, String> {
    let repo = match Repository::discover(&path) {
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
fn list_branches(path: String) -> Result<Vec<BranchInfo>, String> {
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
}

#[tauri::command]
fn get_commits(path: String, count: Option<usize>) -> Result<Vec<CommitInfo>, String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let head = repo.head().map_err(|e| e.to_string())?;
    let head_oid = head.target().ok_or("No HEAD target")?;

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
}

#[tauri::command]
fn checkout_branch(path: String, branch_name: String) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    checkout_branch_inner(&repo, &branch_name)
}

#[tauri::command]
fn create_branch(path: String, branch_name: String, checkout: Option<bool>) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

    let head = repo.head().map_err(|e| e.to_string())?;
    let commit = head.peel_to_commit().map_err(|e| e.to_string())?;

    repo.branch(&branch_name, &commit, false)
        .map_err(|e| e.to_string())?;

    if checkout.unwrap_or(true) {
        checkout_branch_inner(&repo, &branch_name)?;
    }

    Ok(())
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

// Settings
fn config_path() -> std::path::PathBuf {
    let home = dirs_home().unwrap_or_else(|| "/tmp".into());
    home.join(".launchpad").join("config.json")
}

#[tauri::command]
fn load_settings() -> Result<String, String> {
    let path = config_path();
    if path.exists() {
        fs::read_to_string(&path).map_err(|e| e.to_string())
    } else {
        Ok("{}".to_string())
    }
}

#[tauri::command]
fn save_settings(data: String) -> Result<(), String> {
    // Validate JSON before writing to prevent config corruption
    let parsed: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("Invalid JSON: {}", e))?;
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
    let home = dirs_home().unwrap_or_else(|| "/tmp".into());
    home.join(".launchpad").join("file-settings.json")
}

#[tauri::command]
fn load_file_settings() -> Result<String, String> {
    let path = file_settings_path();
    if path.exists() {
        fs::read_to_string(&path).map_err(|e| e.to_string())
    } else {
        Ok("{}".to_string())
    }
}

#[tauri::command]
fn save_file_settings(data: String) -> Result<(), String> {
    let parsed: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| format!("Invalid JSON: {}", e))?;
    let normalized = serde_json::to_string_pretty(&parsed)
        .map_err(|e| format!("JSON serialization error: {}", e))?;
    let path = file_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    atomic_write(&path, normalized.as_bytes()).map_err(|e| e.to_string())?;
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
    let home = dirs_home().unwrap_or_else(|| "/tmp".into());
    home.join(".launchpad").join("projects.json")
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
    // Atomic write: temp + rename so a crash can't corrupt the file.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
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
    let _ = forget_project_env(path);
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
    let home = dirs_home().unwrap_or_else(|| "/tmp".into());
    home.join(".launchpad").join("project-env.json")
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
    // Atomic write: temp + rename so a crash can't corrupt the file.
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body.as_bytes()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    // Restrict to owner read/write — this file holds user secrets.
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;
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

#[tauri::command]
fn save_project_env_vars(path: String, vars: Vec<ProjectEnvVar>) -> Result<(), String> {
    for v in &vars {
        if !is_valid_env_key(&v.key) {
            return Err(format!(
                "Invalid env var name: {:?} (must match [A-Za-z_][A-Za-z0-9_]*)",
                v.key
            ));
        }
    }
    let key = normalize_project_path(&path);
    let mut store = read_project_env_file().unwrap_or_default();
    if vars.is_empty() {
        store.remove(&key);
    } else {
        store.insert(key, vars);
    }
    write_project_env_file(&store)
}

#[tauri::command]
fn forget_project_env(path: String) -> Result<(), String> {
    let key = normalize_project_path(&path);
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

// Recursive file search for quick open
#[tauri::command]
fn search_files(root: String, query: String, max_results: Option<usize>) -> Result<Vec<String>, String> {
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
fn search_in_files(
    root: String,
    query: String,
    case_sensitive: bool,
    is_regex: bool,
    max_results: Option<usize>,
) -> Result<Vec<SearchHit>, String> {
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

#[tauri::command]
fn read_file_preview(path: String, max_bytes: Option<usize>) -> Result<String, String> {
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
    // above the 1% threshold.
    if !data.is_empty() {
        let null_count = data.iter().filter(|&&b| b == 0).count();
        if null_count * 100 > data.len() {
            return Err("Binary file — cannot display".into());
        }
    }

    Ok(String::from_utf8_lossy(&data).to_string())
}

// Write `data` to `dest` atomically: write to a sibling temp file, fsync,
// then rename onto the destination. A crash / kill / ENOSPC mid-write
// leaves the old file intact; a successful rename replaces it in one step.
// Preserves the existing file's mode when overwriting.
fn atomic_write(dest: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    let file_name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("launchpad-write");
    let tmp = dest.with_file_name(format!(".{}.lp-tmp-{}", file_name, std::process::id()));

    let preserved_mode = fs::metadata(dest).ok().map(|m| m.permissions().mode());

    let result = (|| -> std::io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        use std::io::Write;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        if let Some(mode) = preserved_mode {
            fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
        }
        fs::rename(&tmp, dest)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[tauri::command]
fn write_file(path: String, content: String) -> Result<(), String> {
    // Size limit to prevent accidental large writes
    if content.len() > 10 * 1024 * 1024 {
        return Err("File content exceeds 10MB limit".into());
    }
    atomic_write(std::path::Path::new(&path), content.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn create_file(path: String) -> Result<(), String> {
    if std::path::Path::new(&path).exists() {
        return Err("File already exists".into());
    }
    std::fs::write(&path, "").map_err(|e| e.to_string())
}

#[tauri::command]
fn create_directory(path: String) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_path(path: String, project_root: Option<String>) -> Result<(), String> {
    // When a project_root is supplied, refuse deletions outside it. The
    // context menu is the only caller today and always passes this, so in
    // practice nothing unguarded reaches remove_dir_all. Cheap defense
    // against a future caller handing an arbitrary path.
    if let Some(root) = project_root {
        let target = fs::canonicalize(&path).map_err(|e| e.to_string())?;
        let root_canon = fs::canonicalize(&root).map_err(|e| e.to_string())?;
        if !target.starts_with(&root_canon) {
            return Err("Refusing to delete path outside project root".into());
        }
    }
    let p = std::path::Path::new(&path);
    if p.is_dir() {
        std::fs::remove_dir_all(&path).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(&path).map_err(|e| e.to_string())
    }
}

#[tauri::command]
fn rename_path(old_path: String, new_path: String) -> Result<(), String> {
    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())
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

#[tauri::command]
fn get_file_diff(path: String, file_path: String, staged: Option<bool>) -> Result<FileDiff, String> {
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
}

#[tauri::command]
fn git_stage_all(path: String) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let mut index = repo.index().map_err(|e| e.to_string())?;
    index
        .add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)
        .map_err(|e| e.to_string())?;
    index.write().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn git_stage_file(path: String, file_path: String) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let mut index = repo.index().map_err(|e| e.to_string())?;
    index
        .add_path(std::path::Path::new(&file_path))
        .map_err(|e| e.to_string())?;
    index.write().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn git_commit(path: String, message: String) -> Result<String, String> {
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
}

struct GitOpSlot {
    op_id: u64,
    pid: Option<u32>,
    cancelled: bool,
}

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
    let mut guard = SSH_AUTH_SOCK_CACHE.lock().unwrap();
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
// reservation and the final cleanup). Records the PID on the slot, honors a
// cancel that arrived during the spawn window by killing immediately, and
// on return clears the PID on the slot so a subsequent call under the same
// reservation gets a fresh pid field and can't accidentally target the
// wrong child. The cancelled flag persists across calls on the same slot.
fn spawn_git_under_slot(
    args: &[&str],
    path: &str,
    pid_store: &Arc<Mutex<Option<GitOpSlot>>>,
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
        let mut guard = pid_store.lock().unwrap();
        match guard.as_mut() {
            Some(s) if s.op_id == op_id => {
                s.pid = Some(pid);
                s.cancelled
            }
            // Slot was claimed by a later op (concurrent push/pull from two
            // clicks). Treat as not-ours; the other op owns cancellation now.
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
        let mut guard = pid_store.lock().unwrap();
        if let Some(s) = guard.as_mut() {
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

// Reserve a slot, spawn a single git command, clean up. For multi-step ops
// (e.g. push-then-retry-with-upstream) that need a cancel to span the full
// sequence, reserve a slot manually and call spawn_git_under_slot directly.
fn run_git_cancellable(
    args: &[&str],
    path: &str,
    pid_store: &Arc<Mutex<Option<GitOpSlot>>>,
    counter: &Arc<AtomicU64>,
) -> Result<String, String> {
    // Reserve the slot BEFORE spawn. If cancel arrives in the window between
    // spawn() and PID-record, it sets `cancelled=true` on our slot, which
    // spawn_git_under_slot observes and acts on by killing the child.
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    *pid_store.lock().unwrap() = Some(GitOpSlot {
        op_id,
        pid: None,
        cancelled: false,
    });

    let result = spawn_git_under_slot(args, path, pid_store, op_id);

    // Only clear the slot if it still matches our op. A subsequent operation
    // may already have replaced it — in that case leave it alone.
    {
        let mut guard = pid_store.lock().unwrap();
        if let Some(ref s) = *guard {
            if s.op_id == op_id {
                *guard = None;
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
fn git_push(path: String, state: State<AppState>) -> Result<String, String> {
    let pid_store = Arc::clone(&state.git_op_pid);
    let counter = Arc::clone(&state.git_op_counter);

    let (has_upstream, branch) = git_upstream_status(&path);

    // Reserve ONE slot that spans both the initial push and the optional
    // auto-upstream retry. A cancel arriving in the window between the two
    // attempts would otherwise find an empty pid_store and be lost — with
    // one shared reservation, the cancelled flag persists and the retry's
    // child is killed as soon as it's spawned.
    let op_id = counter.fetch_add(1, Ordering::SeqCst);
    *pid_store.lock().unwrap() = Some(GitOpSlot {
        op_id,
        pid: None,
        cancelled: false,
    });

    let result = if has_upstream {
        spawn_git_under_slot(&["push"], &path, &pid_store, op_id)
    } else {
        match branch {
            Some(b) if !b.is_empty() => spawn_git_under_slot(
                &["push", "--set-upstream", "origin", &b],
                &path,
                &pid_store,
                op_id,
            ),
            _ => Err("Could not determine current branch for upstream push".to_string()),
        }
    };

    // Release the reservation.
    {
        let mut guard = pid_store.lock().unwrap();
        if let Some(ref s) = *guard {
            if s.op_id == op_id {
                *guard = None;
            }
        }
    }

    result
}

#[tauri::command]
fn git_pull(path: String, state: State<AppState>) -> Result<String, String> {
    run_git_cancellable(
        &["pull"],
        &path,
        &Arc::clone(&state.git_op_pid),
        &Arc::clone(&state.git_op_counter),
    )
}

#[tauri::command]
fn git_fetch(path: String, state: State<AppState>) -> Result<String, String> {
    run_git_cancellable(
        &["fetch", "--all"],
        &path,
        &Arc::clone(&state.git_op_pid),
        &Arc::clone(&state.git_op_counter),
    )
}

#[tauri::command]
fn cancel_git_op(state: State<AppState>) -> Result<(), String> {
    let mut guard = state.git_op_pid.lock().unwrap();
    if let Some(slot) = guard.as_mut() {
        // Mark cancelled so run_git_cancellable can kill the child even if
        // we got here during the spawn window (PID not yet recorded).
        slot.cancelled = true;
        if let Some(pid) = slot.pid {
            kill_pid_hard(pid);
        }
        // Don't clear the slot here — run_git_cancellable's cleanup owns that
        // and only clears if the op_id still matches, so a stale cancel can't
        // accidentally wipe a newer op's registration.
    }
    Ok(())
}

#[tauri::command]
fn git_merge_branch(
    path: String,
    branch_name: String,
    state: State<AppState>,
) -> Result<String, String> {
    // Validate branch name to prevent option injection
    if !branch_name.chars().all(|c| c.is_alphanumeric() || "._/-".contains(c)) {
        return Err("Invalid branch name".into());
    }
    // Merge can block on network I/O if the user's git config pulls on merge
    // or the operation touches a remote tracking ref. Route it through
    // run_git_cancellable so cancel_git_op can kill it.
    run_git_cancellable(
        &["merge", &branch_name],
        &path,
        &Arc::clone(&state.git_op_pid),
        &Arc::clone(&state.git_op_counter),
    )
}

#[tauri::command]
fn git_resolve_ours(path: String, file_path: String) -> Result<(), String> {
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
}

#[tauri::command]
fn git_resolve_theirs(path: String, file_path: String) -> Result<(), String> {
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
}

#[tauri::command]
fn git_unstage_file(path: String, file_path: String) -> Result<(), String> {
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
}

#[tauri::command]
fn git_unstage_all(path: String) -> Result<(), String> {
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
}

#[tauri::command]
fn git_discard_file(path: String, file_path: String) -> Result<(), String> {
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
}

#[tauri::command]
fn git_stash_save(path: String) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let sig = repo.signature().map_err(|e| e.to_string())?;
    // stash_save requires &mut Repository
    let mut repo = repo;
    repo.stash_save(&sig, "Launchpad stash", None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn git_stash_pop(path: String) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let mut repo = repo;
    repo.stash_pop(0, None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Clone, Serialize)]
struct StashEntry {
    index: usize,
    message: String,
}

#[tauri::command]
fn git_stash_list(path: String) -> Result<Vec<StashEntry>, String> {
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
}

#[tauri::command]
fn git_stash_apply(path: String, index: usize) -> Result<(), String> {
    let mut repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    repo.stash_apply(index, None)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn git_stash_drop(path: String, index: usize) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let mut repo = repo;
    repo.stash_drop(index)
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn git_delete_branch(path: String, branch_name: String) -> Result<(), String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let mut branch = repo
        .find_branch(&branch_name, BranchType::Local)
        .map_err(|e| e.to_string())?;
    branch.delete().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn list_remote_branches(path: String) -> Result<Vec<BranchInfo>, String> {
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
}

#[tauri::command]
fn get_commit_details(path: String, oid: String) -> Result<CommitDetail, String> {
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
}

#[tauri::command]
fn get_remote_url(path: String) -> Result<String, String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;
    let remote = repo.find_remote("origin").map_err(|e| e.to_string())?;
    let url = remote.url().ok_or("No URL for origin")?.to_string();
    Ok(url)
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
    let handle = app.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(300),
        move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = events {
                if !events.is_empty() {
                    let _ = handle.emit(
                        "fs-changed",
                        FsChanged {
                            path: emit_path.clone(),
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

pub fn run() {
    let app_state = AppState {
        ptys: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(Mutex::new(0)),
        fs_watcher: Arc::new(Mutex::new(HashMap::new())),
        git_op_pid: Arc::new(Mutex::new(None)),
        git_op_counter: Arc::new(AtomicU64::new(0)),
        project_windows: Arc::new(Mutex::new(HashMap::new())),
        projects_file_lock: Arc::new(Mutex::new(())),
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
            read_file_preview,
            write_file,
            create_file,
            create_directory,
            delete_path,
            rename_path,
            get_file_diff,
            git_stage_all,
            git_stage_file,
            git_commit,
            git_push,
            git_pull,
            git_fetch,
            git_merge_branch,
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

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // ─── Helper: create a temp git repo with an initial commit ───────────────
    fn setup_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path()).unwrap();

        // Configure git user for commits
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@test.com").unwrap();

        // Create a file, stage it, and commit
        fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("hello.txt")).unwrap();
        index.write().unwrap();
        let tree_oid = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_oid).unwrap();
        let sig = repo.signature().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial commit", &tree, &[]).unwrap();

        dir
    }

    // ─── Helper: create a temp git repo with NO commits (unborn HEAD) ────────
    fn setup_empty_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();
        dir
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // write_file tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_write_file_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        let result = write_file(path.to_string_lossy().to_string(), "hello".into());
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn test_write_file_rejects_oversized_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        let content = "x".repeat(10 * 1024 * 1024 + 1); // just over 10MB
        let result = write_file(path.to_string_lossy().to_string(), content);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("10MB"));
        // File should not have been created
        assert!(!path.exists());
    }

    #[test]
    fn test_write_file_exactly_at_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("limit.txt");
        let content = "x".repeat(10 * 1024 * 1024); // exactly 10MB
        let result = write_file(path.to_string_lossy().to_string(), content);
        assert!(result.is_ok());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // read_file_preview tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_read_file_preview_basic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let result = read_file_preview(path.to_string_lossy().to_string(), None);
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn test_read_file_preview_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let result = read_file_preview(path.to_string_lossy().to_string(), Some(5));
        assert_eq!(result.unwrap(), "hello");
    }

    #[test]
    fn test_read_file_preview_missing_file() {
        let result = read_file_preview("/nonexistent/path/file.txt".into(), None);
        assert!(result.is_err());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // save_settings JSON validation tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_save_settings_rejects_invalid_json() {
        let result = save_settings("not valid json {{{".into());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid JSON"));
    }

    #[test]
    fn test_save_settings_accepts_valid_json() {
        // This writes to ~/.launchpad/config.json — we accept that in tests
        let result = save_settings(r#"{"termFontSize": 14}"#.into());
        assert!(result.is_ok());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // git_merge_branch validation tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_merge_branch_rejects_invalid_names() {
        let dir = setup_git_repo();
        let path = dir.path().to_string_lossy().to_string();

        // Names with spaces
        assert!(git_merge_branch(path.clone(), "branch name".into()).is_err());
        // Names with shell-special chars
        assert!(git_merge_branch(path.clone(), "branch;rm -rf".into()).is_err());
        // Names starting with --
        assert!(git_merge_branch(path.clone(), "--no-verify".into()).is_err());
        // Backtick injection
        assert!(git_merge_branch(path.clone(), "`whoami`".into()).is_err());
    }

    #[test]
    fn test_merge_branch_accepts_valid_names() {
        let dir = setup_git_repo();
        let path = dir.path().to_string_lossy().to_string();

        // Valid names should not fail the validation (may fail because branch doesn't exist,
        // but shouldn't fail with "Invalid branch name")
        let result = git_merge_branch(path.clone(), "feature/test-branch".into());
        // Will error because branch doesn't exist, but not due to validation
        if let Err(e) = result {
            assert!(!e.contains("Invalid branch name"), "Valid name was rejected: {}", e);
        }
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // git_unstage tests — including unborn HEAD
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_unstage_file_with_commits() {
        let dir = setup_git_repo();
        let path = dir.path().to_string_lossy().to_string();

        // Create and stage a new file
        fs::write(dir.path().join("new.txt"), "new file").unwrap();
        let repo = Repository::discover(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("new.txt")).unwrap();
        index.write().unwrap();

        // Unstage it
        let result = git_unstage_file(path, "new.txt".into());
        assert!(result.is_ok());

        // Verify it's no longer staged
        let repo = Repository::discover(dir.path()).unwrap();
        let statuses = repo.statuses(None).unwrap();
        for entry in statuses.iter() {
            if entry.path() == Some("new.txt") {
                assert!(!entry.status().contains(git2::Status::INDEX_NEW));
            }
        }
    }

    #[test]
    fn test_unstage_file_unborn_head() {
        let dir = setup_empty_git_repo();
        let path = dir.path().to_string_lossy().to_string();

        // Create and stage a file in a repo with no commits
        fs::write(dir.path().join("first.txt"), "first file").unwrap();
        let repo = Repository::discover(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("first.txt")).unwrap();
        index.write().unwrap();

        // Should not panic or error — this is the bug we fixed
        let result = git_unstage_file(path, "first.txt".into());
        assert!(result.is_ok());
    }

    #[test]
    fn test_unstage_all_unborn_head() {
        let dir = setup_empty_git_repo();
        let path = dir.path().to_string_lossy().to_string();

        // Stage multiple files in a repo with no commits
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("b.txt"), "b").unwrap();
        let repo = Repository::discover(dir.path()).unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(std::path::Path::new("a.txt")).unwrap();
        index.add_path(std::path::Path::new("b.txt")).unwrap();
        index.write().unwrap();

        // Should not panic or error
        let result = git_unstage_all(path);
        assert!(result.is_ok());

        // Verify index is empty
        let repo = Repository::discover(dir.path()).unwrap();
        let index = repo.index().unwrap();
        assert_eq!(index.len(), 0);
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // get_ahead_behind tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_ahead_behind_no_upstream() {
        let dir = setup_git_repo();
        let repo = Repository::discover(dir.path()).unwrap();

        // No remote configured — should return None gracefully
        let result = get_ahead_behind(&repo);
        assert!(result.is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════════
    // checkout_branch_inner tests
    // ═══════════════════════════════════════════════════════════════════════════

    #[test]
    fn test_checkout_nonexistent_branch() {
        let dir = setup_git_repo();
        let repo = Repository::discover(dir.path()).unwrap();

        let result = checkout_branch_inner(&repo, "nonexistent-branch");
        assert!(result.is_err());
    }

    #[test]
    fn test_checkout_existing_branch() {
        let dir = setup_git_repo();
        let repo = Repository::discover(dir.path()).unwrap();

        // Create a branch
        let head = repo.head().unwrap();
        let commit = head.peel_to_commit().unwrap();
        repo.branch("test-branch", &commit, false).unwrap();

        // Checkout should succeed
        let result = checkout_branch_inner(&repo, "test-branch");
        assert!(result.is_ok());

        // Verify HEAD points to new branch
        let head = repo.head().unwrap();
        assert_eq!(head.shorthand().unwrap(), "test-branch");
    }
}

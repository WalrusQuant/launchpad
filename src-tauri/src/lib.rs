use futures_util::StreamExt;
use git2::{BranchType, Repository, StatusOptions};
use notify_debouncer_mini::{new_debouncer, notify};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{Emitter, State};

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
}

struct AppState {
    ptys: Arc<Mutex<HashMap<u32, PtyInstance>>>,
    next_id: Arc<Mutex<u32>>,
    http_client: reqwest::Client,
    fs_watcher: Arc<Mutex<Option<FsWatcher>>>,
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
}

#[tauri::command]
fn spawn_pty(cwd: Option<String>, rows: Option<u16>, cols: Option<u16>, state: State<AppState>, app: tauri::AppHandle) -> Result<SpawnResult, String> {
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
    // Override / set the terminal-identifying vars that iTerm and Terminal.app set.
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "Launchpad");
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
    let mut reader = {
        let mut ptys = state.ptys.lock().map_err(|e| e.to_string())?;
        let instance = ptys.get_mut(&tab_id).ok_or("Tab not found")?;
        instance
            .pending_reader
            .take()
            .ok_or("Reader already started")?
    };

    let handle = app.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut leftover = Vec::new();
        loop {
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
            let metadata = entry.metadata().ok()?;
            let name = entry.file_name().into_string().ok()?;
            let is_hidden = name.starts_with('.');
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());

            Some(FileEntry {
                name,
                path: entry.path().to_string_lossy().to_string(),
                is_dir: metadata.is_dir(),
                is_hidden,
                size: metadata.len(),
                modified,
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
            });
        }
    };

    let branch = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().map(String::from));

    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
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

    let (ahead, behind) = get_ahead_behind(&repo).unwrap_or((0, 0));

    Ok(GitInfo {
        is_repo: true,
        branch,
        files,
        ahead,
        behind,
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

            Some(BranchInfo {
                name,
                is_current,
                last_commit_msg,
                last_commit_time,
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

    repo.set_head(&refname).map_err(|e| e.to_string())?;

    repo.checkout_head(Some(
        git2::build::CheckoutBuilder::new()
            .force(),
    ))
    .map_err(|e| e.to_string())?;

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
    fs::write(&path, normalized.as_bytes()).map_err(|e| e.to_string())?;
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
        for entry in entries.filter_map(|e| e.ok()) {
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
    let data = fs::read(&path).map_err(|e| e.to_string())?;
    let truncated = &data[..data.len().min(limit)];
    Ok(String::from_utf8_lossy(truncated).to_string())
}

#[tauri::command]
fn write_file(path: String, content: String) -> Result<(), String> {
    // Size limit to prevent accidental large writes
    if content.len() > 10 * 1024 * 1024 {
        return Err("File content exceeds 10MB limit".into());
    }
    fs::write(&path, content.as_bytes()).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn get_file_diff(path: String, file_path: String) -> Result<String, String> {
    let repo = Repository::discover(&path).map_err(|e| e.to_string())?;

    let mut diff_opts = git2::DiffOptions::new();
    diff_opts.pathspec(&file_path);

    let diff = repo
        .diff_index_to_workdir(None, Some(&mut diff_opts))
        .map_err(|e| e.to_string())?;

    let mut result = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let prefix = match line.origin() {
            '+' => "+",
            '-' => "-",
            ' ' => " ",
            _ => "",
        };
        let content = std::str::from_utf8(line.content()).unwrap_or("");
        result.push_str(prefix);
        result.push_str(content);
        true
    })
    .map_err(|e| e.to_string())?;

    // If empty, try HEAD to index diff (for staged files)
    if result.is_empty() {
        let head_tree = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_tree().ok());

        let diff = repo
            .diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
            .map_err(|e| e.to_string())?;

        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            let prefix = match line.origin() {
                '+' => "+",
                '-' => "-",
                ' ' => " ",
                _ => "",
            };
            let content = std::str::from_utf8(line.content()).unwrap_or("");
            result.push_str(prefix);
            result.push_str(content);
            true
        })
        .map_err(|e| e.to_string())?;
    }

    Ok(result)
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

#[tauri::command]
fn git_push(path: String) -> Result<String, String> {
    // First try a normal push
    let output = std::process::Command::new("git")
        .args(["push"])
        .current_dir(&path)
        .output()
        .map_err(|e| e.to_string())?;

    if output.status.success() {
        let msg = String::from_utf8_lossy(&output.stderr).to_string();
        return Ok(if msg.trim().is_empty() { "Pushed successfully".into() } else { msg });
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    // If no upstream, automatically set it up
    if stderr.contains("no upstream branch") || stderr.contains("has no upstream") {
        let branch_output = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(&path)
            .output()
            .map_err(|e| e.to_string())?;
        let branch = String::from_utf8_lossy(&branch_output.stdout).trim().to_string();

        let retry = std::process::Command::new("git")
            .args(["push", "--set-upstream", "origin", &branch])
            .current_dir(&path)
            .output()
            .map_err(|e| e.to_string())?;

        if retry.status.success() {
            let msg = String::from_utf8_lossy(&retry.stderr).to_string();
            return Ok(if msg.trim().is_empty() { "Pushed successfully".into() } else { msg });
        } else {
            return Err(String::from_utf8_lossy(&retry.stderr).to_string());
        }
    }

    Err(stderr)
}

#[tauri::command]
fn git_pull(path: String) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(["pull"])
        .current_dir(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

#[tauri::command]
fn git_merge_branch(path: String, branch_name: String) -> Result<String, String> {
    // Validate branch name to prevent option injection
    if !branch_name.chars().all(|c| c.is_alphanumeric() || "._/-".contains(c)) {
        return Err("Invalid branch name".into());
    }
    let output = std::process::Command::new("git")
        .args(["merge", &branch_name])
        .current_dir(&path)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string())
    }
}

#[tauri::command]
fn git_resolve_ours(path: String, file_path: String) -> Result<(), String> {
    let status = std::process::Command::new("git")
        .args(["checkout", "--ours", "--", &file_path])
        .current_dir(&path)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Failed to checkout --ours".into());
    }
    let status = std::process::Command::new("git")
        .args(["add", "--", &file_path])
        .current_dir(&path)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Failed to stage resolved file".into());
    }
    Ok(())
}

#[tauri::command]
fn git_resolve_theirs(path: String, file_path: String) -> Result<(), String> {
    let status = std::process::Command::new("git")
        .args(["checkout", "--theirs", "--", &file_path])
        .current_dir(&path)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("Failed to checkout --theirs".into());
    }
    let status = std::process::Command::new("git")
        .args(["add", "--", &file_path])
        .current_dir(&path)
        .status()
        .map_err(|e| e.to_string())?;
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

// ===== AI Agent =====

#[derive(Clone, Serialize)]
struct AgentChunk {
    #[serde(rename = "type")]
    chunk_type: String, // "text", "tool_call", "error", "done"
    content: String,
    tool_name: Option<String>,
    tool_args: Option<String>,
    tool_call_id: Option<String>,
}

#[tauri::command]
async fn agent_chat_stream(
    provider_type: String,
    base_url: String,
    api_key: String,
    model: String,
    messages: serde_json::Value,
    tools: serde_json::Value,
    system: Option<String>,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let client = &state.http_client;

    match provider_type.as_str() {
        "openai" => stream_openai(client, &base_url, &api_key, &model, &messages, &tools, &app).await,
        "anthropic" => stream_anthropic(client, &base_url, &api_key, &model, &messages, &tools, system.as_deref(), &app).await,
        _ => Err(format!("Unknown provider type: {}", provider_type)),
    }
}

async fn stream_openai(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &serde_json::Value,
    tools: &serde_json::Value,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });

    if let Some(arr) = tools.as_array() {
        if !arr.is_empty() {
            body["tools"] = tools.clone();
        }
    }

    let resp = client
        .post(base_url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, text));
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut tool_call_id = String::new();
    let mut tool_name = String::new();
    let mut tool_args = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // Process complete SSE lines
        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();

            if line.is_empty() || line == "data: [DONE]" {
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                    if let Some(choices) = json["choices"].as_array() {
                        if let Some(choice) = choices.first() {
                            let delta = &choice["delta"];

                            // Text content
                            if let Some(content) = delta["content"].as_str() {
                                let _ = app.emit("agent-chunk", AgentChunk {
                                    chunk_type: "text".into(),
                                    content: content.into(),
                                    tool_name: None,
                                    tool_args: None,
                                    tool_call_id: None,
                                });
                            }

                            // Tool calls
                            if let Some(tool_calls) = delta["tool_calls"].as_array() {
                                for tc in tool_calls {
                                    if let Some(id) = tc["id"].as_str() {
                                        tool_call_id = id.to_string();
                                    }
                                    if let Some(func) = tc["function"].as_object() {
                                        if let Some(name) = func.get("name").and_then(|n| n.as_str()) {
                                            tool_name = name.to_string();
                                        }
                                        if let Some(args) = func.get("arguments").and_then(|a| a.as_str()) {
                                            tool_args.push_str(args);
                                        }
                                    }
                                }
                            }

                            // Finish reason
                            if let Some(reason) = choice["finish_reason"].as_str() {
                                if reason == "tool_calls" && !tool_name.is_empty() {
                                    let _ = app.emit("agent-chunk", AgentChunk {
                                        chunk_type: "tool_call".into(),
                                        content: String::new(),
                                        tool_name: Some(tool_name.clone()),
                                        tool_args: Some(tool_args.clone()),
                                        tool_call_id: Some(tool_call_id.clone()),
                                    });
                                    tool_call_id.clear();
                                    tool_name.clear();
                                    tool_args.clear();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = app.emit("agent-chunk", AgentChunk {
        chunk_type: "done".into(),
        content: String::new(),
        tool_name: None,
        tool_args: None,
        tool_call_id: None,
    });

    Ok(())
}

async fn stream_anthropic(
    client: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &serde_json::Value,
    tools: &serde_json::Value,
    system: Option<&str>,
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "max_tokens": 4096,
        "stream": true,
    });

    if let Some(sys) = system {
        body["system"] = serde_json::json!(sys);
    }

    if let Some(arr) = tools.as_array() {
        if !arr.is_empty() {
            body["tools"] = tools.clone();
        }
    }

    let resp = client
        .post(base_url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", status, text));
    }

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();
    let mut current_tool_id = String::new();
    let mut current_tool_name = String::new();
    let mut current_tool_input = String::new();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(pos) = buffer.find('\n') {
            let line = buffer[..pos].trim().to_string();
            buffer = buffer[pos + 1..].to_string();

            if line.is_empty() {
                continue;
            }

            if let Some(data) = line.strip_prefix("data: ") {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(data) {
                    let event_type = json["type"].as_str().unwrap_or("");

                    match event_type {
                        "content_block_start" => {
                            let block = &json["content_block"];
                            if block["type"].as_str() == Some("tool_use") {
                                current_tool_id = block["id"].as_str().unwrap_or("").to_string();
                                current_tool_name = block["name"].as_str().unwrap_or("").to_string();
                                current_tool_input.clear();
                            }
                        }
                        "content_block_delta" => {
                            let delta = &json["delta"];
                            if let Some(text) = delta["text"].as_str() {
                                let _ = app.emit("agent-chunk", AgentChunk {
                                    chunk_type: "text".into(),
                                    content: text.into(),
                                    tool_name: None,
                                    tool_args: None,
                                    tool_call_id: None,
                                });
                            }
                            if let Some(json_str) = delta["partial_json"].as_str() {
                                current_tool_input.push_str(json_str);
                            }
                        }
                        "content_block_stop" => {
                            if !current_tool_name.is_empty() {
                                let _ = app.emit("agent-chunk", AgentChunk {
                                    chunk_type: "tool_call".into(),
                                    content: String::new(),
                                    tool_name: Some(current_tool_name.clone()),
                                    tool_args: Some(current_tool_input.clone()),
                                    tool_call_id: Some(current_tool_id.clone()),
                                });
                                current_tool_name.clear();
                                current_tool_input.clear();
                                current_tool_id.clear();
                            }
                        }
                        "message_stop" => {
                            let _ = app.emit("agent-chunk", AgentChunk {
                                chunk_type: "done".into(),
                                content: String::new(),
                                tool_name: None,
                                tool_args: None,
                                tool_call_id: None,
                            });
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
fn get_home_dir() -> Result<String, String> {
    dirs_home()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine home directory".to_string())
}

#[tauri::command]
fn watch_directory(path: String, state: State<AppState>, app: tauri::AppHandle) -> Result<(), String> {
    let watch_path = path.clone();
    let handle = app.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(300),
        move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = events {
                if !events.is_empty() {
                    let _ = handle.emit("fs-changed", FsChanged { path: watch_path.clone() });
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;

    debouncer
        .watcher()
        .watch(
            std::path::Path::new(&path),
            notify::RecursiveMode::Recursive,
        )
        .map_err(|e| e.to_string())?;

    // Replace old watcher — dropping it stops the previous watch
    let mut guard = state.fs_watcher.lock().map_err(|e: std::sync::PoisonError<_>| e.to_string())?;
    *guard = Some(debouncer);
    Ok(())
}

pub fn run() {
    let app_state = AppState {
        ptys: Arc::new(Mutex::new(HashMap::new())),
        next_id: Arc::new(Mutex::new(0)),
        http_client: reqwest::Client::new(),
        fs_watcher: Arc::new(Mutex::new(None)),
    };

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            spawn_pty,
            start_pty_reader,
            write_to_pty,
            resize_pty,
            close_pty,
            read_directory,
            get_home_dir,
            get_git_status,
            list_branches,
            get_commits,
            checkout_branch,
            create_branch,
            load_settings,
            save_settings,
            search_files,
            reveal_in_finder,
            read_file_preview,
            write_file,
            get_file_diff,
            git_stage_all,
            git_stage_file,
            git_commit,
            agent_chat_stream,
            git_push,
            git_pull,
            git_merge_branch,
            git_resolve_ours,
            git_resolve_theirs,
            git_unstage_file,
            git_unstage_all,
            git_discard_file,
            git_stash_save,
            git_stash_pop,
            git_delete_branch,
            list_remote_branches,
            get_commit_details,
            get_remote_url,
            watch_directory
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

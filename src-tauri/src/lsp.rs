// Language Server host: spawns external language servers (typescript-language-
// server, etc.) as child processes and bridges them to the frontend over Tauri
// IPC. The frontend runs @codemirror/lsp-client and talks JSON-RPC; this module
// is just the process manager + byte pump, modeled on pty.rs:
//
//   frontend  --invoke lsp_send-->  child stdin   (we add the Content-Length frame)
//   child stdout  --reader thread-->  emit("lsp-message")  -->  frontend
//
// One server per "{language}:{project_path}". The frontend decides when to start
// one (lazily, on opening a file of that language) and owns the LSP protocol;
// we never parse message bodies, only their framing.
use crate::*;
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use tauri::{AppHandle, Emitter, Manager, State};

// Monotonic id stamped on each spawned server so the reader thread can tell
// whether the map entry under its key is still *its* instance (vs a successor
// spawned after it died) before removing it.
static LSP_GENERATION: AtomicU64 = AtomicU64::new(1);

// A running language server. stdout was moved into the reader thread; we keep
// the child handle (to kill/reap on stop) and stdin (to write requests).
pub(crate) struct LspInstance {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
    pub(crate) generation: u64,
}

#[derive(Clone, Serialize)]
pub(crate) struct LspMessage {
    pub(crate) server_id: String,
    pub(crate) message: String,
}

// Map a logical language id to the server binary + args. Servers are user-
// installed and resolved on PATH; None means "no server configured", which the
// frontend treats as "LSP unavailable for this file" and degrades silently.
fn server_command(language: &str) -> Option<(&'static str, &'static [&'static str])> {
    match language {
        "typescript" | "javascript" => Some(("typescript-language-server", &["--stdio"])),
        "rust" => Some(("rust-analyzer", &[])),
        "python" => Some(("pyright-langserver", &["--stdio"])),
        _ => None,
    }
}

// Common locations dev tools install to that a Finder/Dock-launched `.app` does
// NOT inherit on its PATH — GUI bundles get a minimal PATH, unlike a process
// started from a login shell. Without this, a server the user already has from
// VS Code / their toolchain (rust-analyzer in ~/.cargo/bin, an nvm- or
// Homebrew-installed typescript-language-server, etc.) is invisible to the app
// when it's opened from Finder. Mirrors the SSH_AUTH_SOCK workaround in git.rs.
fn extra_path_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(home) = std::env::var("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".cargo/bin"));
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".npm-global/bin"));
        dirs.push(home.join(".volta/bin"));
        dirs.push(home.join(".bun/bin"));
        // nvm puts node + global npm bins under a per-version dir; add every
        // installed version's bin so an nvm user's global server is found.
        if let Ok(entries) = std::fs::read_dir(home.join(".nvm/versions/node")) {
            for e in entries.flatten() {
                dirs.push(e.path().join("bin"));
            }
        }
    }
    dirs.push(PathBuf::from("/opt/homebrew/bin"));
    dirs.push(PathBuf::from("/opt/homebrew/sbin"));
    dirs.push(PathBuf::from("/usr/local/bin"));
    dirs
}

// The PATH language servers run under: the inherited PATH plus the common dirs
// above, deduped with inherited entries kept first so the user's own ordering
// wins. Computed once — install locations don't move at runtime. Also used as
// the search space for resolve_bin, so detection and spawning always agree.
fn server_path_env() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut seen = std::collections::HashSet::new();
            let mut parts: Vec<String> = Vec::new();
            let inherited = std::env::var("PATH").unwrap_or_default();
            for p in inherited.split(':') {
                if !p.is_empty() && seen.insert(p.to_string()) {
                    parts.push(p.to_string());
                }
            }
            for d in extra_path_dirs() {
                let s = d.to_string_lossy().into_owned();
                if !s.is_empty() && seen.insert(s.clone()) {
                    parts.push(s);
                }
            }
            parts.join(":")
        })
        .clone()
}

// Resolve `bin` to an absolute path by scanning server_path_env(). Returns the
// first match that exists as a regular file (symlinks are followed, so Homebrew
// shims resolve). None means "not installed in any known location".
fn resolve_bin(bin: &str) -> Option<PathBuf> {
    for dir in server_path_env().split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// Frame a JSON-RPC body with the LSP `Content-Length` header. Length is in
// bytes, not chars — matters for non-ASCII payloads.
pub(crate) fn encode_lsp_message(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

// Read exactly one framed LSP message from `reader`. Returns Ok(None) on a clean
// EOF (server exited). Headers are CRLF-delimited and terminated by a blank
// line; only Content-Length is significant (Content-Type is ignored).
pub(crate) fn read_lsp_message<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            return Ok(None); // EOF before any complete message
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // blank line ends the header block
        }
        if let Some(v) = trimmed.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().ok();
        }
    }
    let len = content_length.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "LSP message missing Content-Length")
    })?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    let s = String::from_utf8(buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(s))
}

// Start (or reuse) a language server for the given project + language. Returns a
// stable server id the frontend uses for lsp_send and to filter lsp-message
// events. Idempotent: a second call for the same (language, project) returns the
// existing id without spawning again.
#[tauri::command]
pub(crate) async fn lsp_start(
    app: AppHandle,
    state: State<'_, AppState>,
    project_path: String,
    language: String,
) -> Result<String, String> {
    let server_id = format!("{language}:{project_path}");

    // Fast path: already running.
    {
        let servers = state.lsp_servers.lock_safe();
        if servers.contains_key(&server_id) {
            return Ok(server_id);
        }
    }

    let (cmd, args) = server_command(&language)
        .ok_or_else(|| format!("No language server configured for {language}"))?;

    // Resolve to an absolute path against our augmented search path so a server
    // the user already has (but that a Finder-launched bundle's PATH can't see)
    // is found; fall back to the bare name so the NotFound message still fires.
    // Run the child under the same augmented PATH so node-based servers locate
    // `node` and their own helpers.
    let program: std::ffi::OsString = resolve_bin(cmd)
        .map(|p| p.into_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from(cmd));

    // Spawn outside the lock (spawn does I/O). A concurrent lsp_start for the
    // same key may also reach here; the re-check under the lock below resolves
    // the race by killing whichever child loses.
    let mut child = Command::new(&program)
        .args(args)
        .current_dir(&project_path)
        .env("PATH", server_path_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{cmd} not found — install it to enable language features (Settings → Editor shows how)")
            } else {
                format!("Failed to start {cmd}: {e}")
            }
        })?;

    let stdin = child.stdin.take().ok_or("Failed to capture server stdin")?;
    let stdout = child.stdout.take().ok_or("Failed to capture server stdout")?;
    let generation = LSP_GENERATION.fetch_add(1, Ordering::Relaxed);

    // Insert under the lock, re-checking for a concurrent winner. Insert BEFORE
    // spawning the reader thread so the entry is guaranteed present when the
    // reader later tries to remove it on exit.
    {
        let mut servers = state.lsp_servers.lock_safe();
        if servers.contains_key(&server_id) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(server_id);
        }
        servers.insert(server_id.clone(), LspInstance { child, stdin, generation });
    }

    // Reader thread: forward each framed message as an event; on EOF/error the
    // server has exited — drop our entry (only if it's still ours, i.e. the
    // generation matches, so we don't evict a successor spawned after a crash),
    // reap the child, then notify the frontend.
    let app_handle = app.clone();
    let sid = server_id.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            match read_lsp_message(&mut reader) {
                Ok(Some(message)) => {
                    let _ = app_handle.emit(
                        events::LSP_MESSAGE,
                        LspMessage { server_id: sid.clone(), message },
                    );
                }
                Ok(None) | Err(_) => break,
            }
        }
        let removed = {
            let st = app_handle.state::<AppState>();
            let mut servers = st.lsp_servers.lock_safe();
            if servers.get(&sid).map(|i| i.generation) == Some(generation) {
                servers.remove(&sid)
            } else {
                None
            }
        };
        if let Some(mut inst) = removed {
            let _ = inst.child.wait();
        }
        let _ = app_handle.emit(events::LSP_EXIT, sid.clone());
    });

    Ok(server_id)
}

// Write a JSON-RPC message to the server's stdin, adding the Content-Length
// frame. Errors if the server isn't running or the pipe is broken.
#[tauri::command]
pub(crate) async fn lsp_send(
    state: State<'_, AppState>,
    server_id: String,
    message: String,
) -> Result<(), String> {
    let mut servers = state.lsp_servers.lock_safe();
    let inst = servers
        .get_mut(&server_id)
        .ok_or("Language server is not running")?;
    let framed = encode_lsp_message(&message);
    inst.stdin.write_all(&framed).map_err(|e| e.to_string())?;
    inst.stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

// Detection status for each supported language server, for the Settings UI.
// `found`/`path` come from resolve_bin so they reflect exactly what lsp_start
// would launch (same augmented search path).
#[derive(Clone, Serialize)]
pub(crate) struct LspServerStatus {
    pub(crate) label: String,
    pub(crate) binary: String,
    pub(crate) found: bool,
    pub(crate) path: Option<String>,
    pub(crate) install: String,
}

#[tauri::command]
pub(crate) fn lsp_server_status() -> Vec<LspServerStatus> {
    const SERVERS: &[(&str, &str, &str)] = &[
        ("Rust", "rust-analyzer", "rustup component add rust-analyzer"),
        (
            "JavaScript / TypeScript",
            "typescript-language-server",
            "npm i -g typescript-language-server",
        ),
        ("Python", "pyright-langserver", "npm i -g pyright"),
    ];
    SERVERS
        .iter()
        .map(|(label, binary, install)| {
            let resolved = resolve_bin(binary);
            LspServerStatus {
                label: label.to_string(),
                binary: binary.to_string(),
                found: resolved.is_some(),
                path: resolved.map(|p| p.to_string_lossy().into_owned()),
                install: install.to_string(),
            }
        })
        .collect()
}

// Stop a language server and drop its slot. Killing the child closes its stdout,
// which ends the reader thread. Missing id is a no-op.
#[tauri::command]
pub(crate) async fn lsp_stop(state: State<'_, AppState>, server_id: String) -> Result<(), String> {
    if let Some(mut inst) = state.lsp_servers.lock_safe().remove(&server_id) {
        let _ = inst.child.kill();
        let _ = inst.child.wait(); // reap so we don't leave a zombie
    }
    Ok(())
}

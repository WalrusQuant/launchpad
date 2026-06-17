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
use std::process::{Child, ChildStdin, Command, Stdio};
use tauri::{AppHandle, Emitter, State};

// A running language server. stdout was moved into the reader thread; we keep
// the child handle (to kill on stop) and stdin (to write requests).
pub(crate) struct LspInstance {
    pub(crate) child: Child,
    pub(crate) stdin: ChildStdin,
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

    {
        let servers = state.lsp_servers.lock_safe();
        if servers.contains_key(&server_id) {
            return Ok(server_id);
        }
    }

    let (cmd, args) = server_command(&language)
        .ok_or_else(|| format!("No language server configured for {language}"))?;

    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(&project_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("{cmd} not found on PATH — install it to enable language features")
            } else {
                format!("Failed to start {cmd}: {e}")
            }
        })?;

    let stdin = child.stdin.take().ok_or("Failed to capture server stdin")?;
    let stdout = child.stdout.take().ok_or("Failed to capture server stdout")?;

    // Reader thread: forward each framed message as an event; exit (and notify)
    // when the server closes its stdout or emits a malformed frame.
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
        let _ = app_handle.emit(events::LSP_EXIT, sid.clone());
    });

    state
        .lsp_servers
        .lock_safe()
        .insert(server_id.clone(), LspInstance { child, stdin });
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

// Stop a language server and drop its slot. Killing the child closes its stdout,
// which ends the reader thread. Missing id is a no-op.
#[tauri::command]
pub(crate) async fn lsp_stop(state: State<'_, AppState>, server_id: String) -> Result<(), String> {
    if let Some(mut inst) = state.lsp_servers.lock_safe().remove(&server_id) {
        let _ = inst.child.kill();
    }
    Ok(())
}

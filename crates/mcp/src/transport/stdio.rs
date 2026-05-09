//! NDJSON-over-stdio transport.
//!
//! Spawns a child process and pumps JSON-RPC frames over its stdio streams.
//! Framing is newline-delimited JSON with tolerant buffering — if a single
//! line fails to parse as JSON, the transport accumulates it and retries once
//! more bytes arrive. This handles real-world servers that embed newlines
//! in JSON string values even though the spec says one-object-per-line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

use crate::protocol::{IncomingMessage, parse_message};

use super::{Transport, TransportError};

/// Cap on the tolerant-buffering accumulator. If the buffer grows past this
/// without producing a valid JSON object, we drop it and warn. This prevents
/// a misbehaving server from exhausting memory.
const MAX_PARSE_BUFFER_BYTES: usize = 1 << 20; // 1 MiB

/// NDJSON-over-stdio transport owning a child process and its pipes.
pub struct StdioTransport {
    child: Mutex<Option<Child>>,
    writer_tx: mpsc::UnboundedSender<Vec<u8>>,
    inbound_rx: Mutex<Option<mpsc::UnboundedReceiver<IncomingMessage>>>,
    cancel: CancellationToken,
}

impl StdioTransport {
    /// Spawns a new stdio-backed transport.
    ///
    /// Starts three background tasks tied to the returned `CancellationToken`:
    /// a writer that drains outbound frames into the child's stdin, a reader
    /// that parses NDJSON from stdout, and a drain that forwards stderr lines
    /// to `tracing::debug!`.
    pub fn spawn(
        command: &[String],
        cwd: Option<&Path>,
        env: &BTreeMap<String, String>,
    ) -> Result<Self, TransportError> {
        let (program, args) = command
            .split_first()
            .ok_or_else(|| TransportError::WriteFailed("empty command".to_owned()))?;

        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| TransportError::WriteFailed("child stdin missing".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| TransportError::WriteFailed("child stdout missing".to_owned()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| TransportError::WriteFailed("child stderr missing".to_owned()))?;

        let cancel = CancellationToken::new();
        let (writer_tx, writer_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<IncomingMessage>();

        spawn_writer_task(stdin, writer_rx, cancel.clone());
        spawn_reader_task(stdout, inbound_tx, cancel.clone());
        spawn_stderr_task(stderr, cancel.clone(), PathBuf::from(program.as_str()));

        Ok(Self {
            child: Mutex::new(Some(child)),
            writer_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
            cancel,
        })
    }
}

fn spawn_writer_task(
    mut stdin: ChildStdin,
    mut writer_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                maybe_frame = writer_rx.recv() => {
                    match maybe_frame {
                        Some(mut frame) => {
                            frame.push(b'\n');
                            if let Err(err) = stdin.write_all(&frame).await {
                                error!(error = %err, "mcp stdio writer: failed to write frame");
                                break;
                            }
                            if let Err(err) = stdin.flush().await {
                                error!(error = %err, "mcp stdio writer: flush failed");
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });
}

fn spawn_reader_task(
    stdout: ChildStdout,
    inbound_tx: mpsc::UnboundedSender<IncomingMessage>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut pending = String::new();
        loop {
            let mut line = String::new();
            let read_fut = reader.read_line(&mut line);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = read_fut => {
                    match result {
                        Ok(0) => {
                            debug!("mcp stdio reader: stdout closed");
                            break;
                        }
                        Ok(_) => handle_line(&mut pending, &line, &inbound_tx),
                        Err(err) => {
                            error!(error = %err, "mcp stdio reader: read failed");
                            break;
                        }
                    }
                }
            }
        }
    });
}

fn handle_line(
    pending: &mut String,
    line: &str,
    inbound_tx: &mpsc::UnboundedSender<IncomingMessage>,
) {
    let trimmed = line.trim();
    if trimmed.is_empty() && pending.is_empty() {
        return;
    }

    let candidate: &str = if pending.is_empty() {
        trimmed
    } else {
        pending.push_str(line);
        pending.as_str()
    };

    match parse_message(candidate.as_bytes()) {
        Ok(msg) => {
            if inbound_tx.send(msg).is_err() {
                debug!("mcp stdio reader: inbound receiver dropped");
            }
            pending.clear();
        }
        Err(err) => {
            if pending.is_empty() {
                pending.push_str(line);
            }
            if pending.len() > MAX_PARSE_BUFFER_BYTES {
                warn!(
                    error = %err,
                    bytes = pending.len(),
                    "mcp stdio reader: parse buffer overflow, dropping",
                );
                pending.clear();
            }
        }
    }
}

fn spawn_stderr_task(
    stderr: tokio::process::ChildStderr,
    cancel: CancellationToken,
    program: PathBuf,
) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        loop {
            let mut line = String::new();
            let read_fut = reader.read_line(&mut line);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                result = read_fut => {
                    match result {
                        Ok(0) => break,
                        Ok(_) => {
                            let trimmed = line.trim_end();
                            if !trimmed.is_empty() {
                                debug!(server = %program.display(), "mcp stderr: {trimmed}");
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
    });
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, frame: Vec<u8>) -> Result<(), TransportError> {
        self.writer_tx
            .send(frame)
            .map_err(|_| TransportError::Shutdown)
    }

    fn take_inbound(&self) -> Result<mpsc::UnboundedReceiver<IncomingMessage>, TransportError> {
        let mut guard = self
            .inbound_rx
            .lock()
            .map_err(|_| TransportError::Shutdown)?;
        guard.take().ok_or(TransportError::InboundAlreadyTaken)
    }

    async fn shutdown(&self) -> Result<(), TransportError> {
        self.cancel.cancel();
        let taken = {
            let mut guard = self.child.lock().map_err(|_| TransportError::Shutdown)?;
            guard.take()
        };
        if let Some(mut child) = taken {
            // Best-effort SIGTERM on unix; Windows goes straight to kill.
            #[cfg(unix)]
            {
                if let Some(pid) = child.id() {
                    let _ = unsafe { libc_kill(pid as i32, libc_sigterm()) };
                }
            }
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
            let _ = child.start_kill();
        }
        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(unix)]
#[allow(non_snake_case)]
unsafe fn libc_kill(pid: i32, sig: i32) -> i32 {
    // SAFETY: calling into libc; both args are POD.
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, sig) }
}

#[cfg(unix)]
fn libc_sigterm() -> i32 {
    15
}

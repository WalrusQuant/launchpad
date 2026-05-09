//! Transport layer — abstract over stdio / HTTP and pump `IncomingMessage`s.

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::mpsc;

use crate::protocol::IncomingMessage;

pub mod stdio;

pub use stdio::StdioTransport;

/// Failure modes exposed by a `Transport`.
#[derive(Debug, Error)]
pub enum TransportError {
    /// Failed to spawn the child process or otherwise initialize the transport.
    #[error("spawn failed: {0}")]
    SpawnFailed(#[from] std::io::Error),
    /// Writing to the transport failed.
    #[error("write failed: {0}")]
    WriteFailed(String),
    /// The transport is already shut down.
    #[error("transport is shut down")]
    Shutdown,
    /// The inbound stream has already been taken by another consumer.
    #[error("inbound stream already taken")]
    InboundAlreadyTaken,
}

/// Transports ferry encoded frames between the runtime and one MCP server.
#[async_trait]
pub trait Transport: Send + Sync {
    /// Sends one encoded JSON-RPC frame. Implementations add any framing
    /// (e.g. NDJSON appends `\n`).
    async fn send(&self, frame: Vec<u8>) -> Result<(), TransportError>;

    /// Takes the inbound receiver. Called exactly once per transport — subsequent
    /// calls return `InboundAlreadyTaken`.
    fn take_inbound(&self) -> Result<mpsc::UnboundedReceiver<IncomingMessage>, TransportError>;

    /// Begins orderly shutdown. Implementations should kill any owned child process.
    async fn shutdown(&self) -> Result<(), TransportError>;
}

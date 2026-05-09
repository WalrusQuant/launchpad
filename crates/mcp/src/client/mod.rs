//! JSON-RPC client used to talk to one MCP server.

// The inner `client` submodule keeps the concrete client type and its errors
// isolated from the `pending`-requests helper; re-exports below give callers
// a flat `mcp::client::*` surface so the nested naming stays internal.
#[allow(clippy::module_inception)]
pub mod client;
pub mod pending;

pub use client::{DEFAULT_REQUEST_TIMEOUT, McpClient, McpClientError};
pub use pending::PendingRequests;

#[cfg(test)]
mod tests;

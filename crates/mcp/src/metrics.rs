//! Structured tracing emitters for MCP lifecycle + tool-call observability.
//!
//! v1 uses plain `tracing::info!` events so existing log-aggregation setups
//! pick them up without new dependencies. Names follow `mcp.<subject>.<event>`
//! and are stable identifiers rather than free-form messages.

use tracing::info;

use crate::McpServerId;

/// Emitted when a supervisor begins the `initialize` handshake.
pub fn server_start(server_id: &McpServerId) {
    info!(event = "mcp.server.start", server = %server_id);
}

/// Emitted when a supervisor finishes startup and catalog refresh.
pub fn server_ready(server_id: &McpServerId, tool_count: usize) {
    info!(event = "mcp.server.ready", server = %server_id, tool_count);
}

/// Emitted when a supervisor transitions to `Failed`.
pub fn server_failed(server_id: &McpServerId, reason: &str) {
    info!(event = "mcp.server.failed", server = %server_id, reason);
}

/// Emitted when a server reports `AuthRequired`. (v1: unused — reserved for
/// the HTTP transport follow-up.)
pub fn server_auth_required(server_id: &McpServerId) {
    info!(event = "mcp.server.auth_required", server = %server_id);
}

/// Emitted for every `tools/call` attempt.
pub fn tool_call(server_id: &McpServerId, tool_name: &str) {
    info!(event = "mcp.tool.call.count", server = %server_id, tool = tool_name);
}

/// Emitted for every `tools/call` that fails (transport, protocol, or `is_error`).
pub fn tool_call_failure(server_id: &McpServerId, tool_name: &str, reason: &str) {
    info!(event = "mcp.tool.call.failure.count", server = %server_id, tool = tool_name, reason);
}

/// Emitted when a supervisor completes a catalog refresh.
pub fn catalog_refresh(server_id: &McpServerId, duration_ms: u128) {
    info!(event = "mcp.catalog.refresh.duration_ms", server = %server_id, duration_ms);
}

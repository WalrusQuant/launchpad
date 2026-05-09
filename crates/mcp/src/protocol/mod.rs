//! MCP + JSON-RPC 2.0 protocol types.
//!
//! Pure data types only — no I/O. Used by the stdio transport (Phase 2+)
//! and eventually by the HTTP transport gated behind the `streamable-http`
//! feature.

pub mod errors;
pub mod jsonrpc;
pub mod messages;

pub use errors::ProtocolParseError;
pub use jsonrpc::{
    IncomingMessage, JSONRPC_VERSION, JsonRpcError, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, RequestId, encode_notification, encode_request, encode_response_success,
    parse_message,
};
pub use messages::{
    CallToolParams, CallToolResult, ClientCapabilities, ClientInfo, ContentBlock, InitializeParams,
    InitializeResult, ListToolsResult, MCP_PROTOCOL_VERSION, McpToolSpec, ServerInfo,
    ToolAnnotations,
};

#[cfg(test)]
mod tests;

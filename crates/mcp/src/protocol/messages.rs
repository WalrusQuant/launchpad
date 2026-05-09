//! MCP-specific request/response payload types.
//!
//! Only the slice of the spec used by the v1 stdio + tools-only runtime is
//! modeled here. Resources, prompts, sampling, elicitation, and roots are
//! deliberately omitted — see `wishlist.md` for the scope cuts.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The MCP protocol version this runtime negotiates.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

// ---------- initialize ----------

/// The `initialize` request params sent from client to server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// The MCP protocol version the client intends to speak.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// The client capabilities advertised during negotiation.
    pub capabilities: ClientCapabilities,
    /// The client identity advertised to the server.
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
}

/// Capabilities advertised by the client in `initialize`.
///
/// v1 advertises no client-side capabilities (no sampling, no roots,
/// no elicitation). This lets servers know not to send those requests.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    /// Reserved for future expansion (roots, sampling, elicitation).
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

/// Client identity sent during `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    /// The client product name.
    pub name: String,
    /// The client product version string.
    pub version: String,
}

/// The `initialize` result returned by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    /// The MCP protocol version the server will speak.
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    /// The server capability set.
    pub capabilities: Value,
    /// The server identity.
    #[serde(rename = "serverInfo")]
    pub server_info: ServerInfo,
    /// Optional instructions the server wants the host to surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

/// Server identity returned from `initialize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerInfo {
    /// The server product name.
    pub name: String,
    /// The server product version string.
    pub version: String,
}

// ---------- tools ----------

/// The `tools/list` result returned by the server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ListToolsResult {
    /// The discovered tool catalog.
    pub tools: Vec<McpToolSpec>,
    /// Optional continuation cursor (unused in v1 — v1 requests without one).
    #[serde(
        rename = "nextCursor",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub next_cursor: Option<String>,
}

/// One tool entry returned from `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolSpec {
    /// The tool identifier, stable across sessions.
    pub name: String,
    /// The tool description surfaced to the model.
    #[serde(default)]
    pub description: String,
    /// The JSON schema describing the tool's input payload.
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    /// Optional safety / semantic annotations (read-only hint, destructive hint, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
}

/// Optional MCP tool annotations (spec ≥ 2025-03-26).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    /// Human-readable annotation title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Hints the tool is read-only — safe to skip approval.
    #[serde(
        rename = "readOnlyHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
    /// Hints the tool performs destructive operations.
    #[serde(
        rename = "destructiveHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive_hint: Option<bool>,
    /// Hints repeated calls with the same inputs are idempotent.
    #[serde(
        rename = "idempotentHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotent_hint: Option<bool>,
    /// Hints the tool interacts with an open-world environment.
    #[serde(
        rename = "openWorldHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub open_world_hint: Option<bool>,
}

/// The `tools/call` request params.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallToolParams {
    /// The name of the tool to invoke.
    pub name: String,
    /// The JSON payload forwarded to the tool implementation.
    #[serde(default)]
    pub arguments: Value,
}

/// The `tools/call` result returned by the server.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CallToolResult {
    /// The content blocks produced by the tool.
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    /// Whether the tool reports its own failure despite returning a successful RPC.
    #[serde(rename = "isError", default)]
    pub is_error: bool,
}

/// One output block returned from a tool invocation.
///
/// The `Other` variant intentionally swallows unknown block types so new
/// spec additions don't crash the parser.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// A textual content block.
    Text {
        /// The UTF-8 text payload.
        text: String,
    },
    /// An image content block (base64-encoded payload + mime type).
    Image {
        /// Base64-encoded image bytes.
        data: String,
        /// IANA media type (e.g. `image/png`).
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    /// A resource-embedded content block.
    Resource {
        /// The embedded resource payload (opaque in v1 — passed through as JSON).
        resource: Value,
    },
    /// Any other block type the current runtime does not recognize.
    #[serde(untagged)]
    Other(Value),
}

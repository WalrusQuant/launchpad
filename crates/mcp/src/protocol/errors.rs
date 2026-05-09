//! Protocol parse errors surfaced while decoding JSON-RPC / MCP frames.

use thiserror::Error;

/// Enumerates the failure modes encountered while parsing a JSON-RPC or MCP frame.
#[derive(Debug, Error)]
pub enum ProtocolParseError {
    /// The byte slice was not valid JSON.
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// The frame was missing a required field.
    #[error("missing required field: {name}")]
    MissingField {
        /// The name of the missing field.
        name: &'static str,
    },
    /// The frame advertised an unsupported JSON-RPC version.
    #[error("unsupported jsonrpc version: got {got:?}")]
    UnsupportedVersion {
        /// The version string the server advertised.
        got: String,
    },
    /// The frame used an unknown method identifier.
    #[error("unknown method: {method}")]
    UnknownMethod {
        /// The unknown method identifier.
        method: String,
    },
}

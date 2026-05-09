//! Per-server catalog of discovered capabilities.

use chrono::{DateTime, Utc};

use crate::{McpResourceDescriptor, McpResourceTemplateDescriptor, McpToolDescriptor};

/// Holds the most recently discovered capability catalog for one server.
#[derive(Debug, Clone, Default)]
pub struct ServerCatalog {
    /// Discovered tools, as surfaced from the most recent `tools/list` call.
    pub tools: Vec<McpToolDescriptor>,
    /// Discovered resources (unused in v1 — always empty).
    pub resources: Vec<McpResourceDescriptor>,
    /// Discovered resource templates (unused in v1 — always empty).
    pub resource_templates: Vec<McpResourceTemplateDescriptor>,
    /// Timestamp of the most recent successful refresh.
    pub last_refreshed_at: Option<DateTime<Utc>>,
}

impl ServerCatalog {
    /// Returns a freshly populated catalog with `last_refreshed_at = now()`.
    pub fn with_tools(tools: Vec<McpToolDescriptor>) -> Self {
        Self {
            tools,
            resources: Vec::new(),
            resource_templates: Vec::new(),
            last_refreshed_at: Some(Utc::now()),
        }
    }
}

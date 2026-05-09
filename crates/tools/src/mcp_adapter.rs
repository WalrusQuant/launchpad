//! Bridges MCP-discovered tools into the built-in `ToolRegistry`.
//!
//! Each discovered `McpToolDescriptor` is wrapped in one `McpToolAdapter`
//! instance implementing the local `Tool` trait. Invocations flow
//! `ToolRegistry → McpToolAdapter → McpManager → ServerSupervisor →
//! McpClient → child process`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tracing::{debug, error};

use lpa_mcp::protocol::{CallToolResult, ContentBlock, ToolAnnotations};
use lpa_mcp::{McpError, McpManager, McpServerId, McpServerStatus, McpToolDescriptor};

use crate::ToolContext;
use crate::registry::ToolRegistry;
use crate::tool::{Tool, ToolOutput};

/// Prefix applied to every MCP-exposed tool name to disambiguate from built-ins.
pub const MCP_TOOL_PREFIX: &str = "mcp__";

/// Adapter that dispatches one MCP tool through the manager.
pub struct McpToolAdapter {
    server_id: McpServerId,
    descriptor: McpToolDescriptor,
    exposed_name: String,
    manager: Arc<dyn McpManager>,
    annotations: Option<ToolAnnotations>,
}

impl McpToolAdapter {
    /// Builds a new adapter. `exposed_name` is `mcp__<server_id>__<tool_name>`.
    pub fn new(
        manager: Arc<dyn McpManager>,
        server_id: McpServerId,
        descriptor: McpToolDescriptor,
        annotations: Option<ToolAnnotations>,
    ) -> Self {
        let exposed_name = format!("{MCP_TOOL_PREFIX}{}__{}", server_id, descriptor.name);
        Self {
            server_id,
            descriptor,
            exposed_name,
            manager,
            annotations,
        }
    }

    /// Returns the exposed (namespaced) tool name.
    pub fn exposed_name(&self) -> &str {
        &self.exposed_name
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.exposed_name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }

    fn input_schema(&self) -> Value {
        self.descriptor.input_schema.clone()
    }

    async fn execute(&self, _ctx: &ToolContext, input: Value) -> anyhow::Result<ToolOutput> {
        match self
            .manager
            .invoke_tool(&self.server_id, &self.descriptor.name, input)
            .await
        {
            Ok(value) => Ok(flatten_call_tool_result_value(value)),
            Err(McpError::McpToolInvocationFailed { message, .. }) => {
                Ok(ToolOutput::error(message))
            }
            Err(err) => Ok(ToolOutput::error(err.to_string())),
        }
    }

    fn is_read_only(&self) -> bool {
        self.annotations
            .as_ref()
            .and_then(|a| a.read_only_hint)
            .unwrap_or(false)
    }
}

fn flatten_call_tool_result_value(value: Value) -> ToolOutput {
    match serde_json::from_value::<CallToolResult>(value.clone()) {
        Ok(result) => flatten_call_tool_result(result, Some(value)),
        Err(_) => ToolOutput {
            content: value.to_string(),
            is_error: false,
            metadata: Some(value),
        },
    }
}

/// Flattens a `CallToolResult` into a `ToolOutput`.
///
/// - Text blocks are concatenated with `\n`.
/// - Image/resource blocks are surfaced as single-line placeholders so the
///   model sees something informative without spamming base64 into the prompt.
/// - The raw JSON is always preserved in `metadata`.
pub fn flatten_call_tool_result(result: CallToolResult, raw: Option<Value>) -> ToolOutput {
    let mut parts: Vec<String> = Vec::new();
    for block in &result.content {
        match block {
            ContentBlock::Text { text } => parts.push(text.clone()),
            ContentBlock::Image { mime_type, data } => {
                parts.push(format!("[image/{mime_type}: {} bytes]", data.len()));
            }
            ContentBlock::Resource { resource } => {
                parts.push(format!("[resource: {resource}]"));
            }
            ContentBlock::Other(value) => {
                parts.push(format!("[unsupported: {value}]"));
            }
        }
    }
    let content = if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n")
    };
    ToolOutput {
        content,
        is_error: result.is_error,
        metadata: raw,
    }
}

/// Registers one `McpToolAdapter` per `(server, tool)` pair into the registry.
///
/// Collisions are logged + skipped rather than fatal. The `mcp__<id>__<tool>`
/// prefix makes a collision nearly impossible in practice, but two MCP servers
/// with identical ids would trip it — in that case the first wins.
pub fn register_mcp_tools(
    registry: &mut ToolRegistry,
    manager: Arc<dyn McpManager>,
    statuses: &[McpServerStatus],
) {
    for status in statuses {
        for descriptor in &status.tools {
            let annotations = descriptor.annotations.clone();
            let adapter = McpToolAdapter::new(
                Arc::clone(&manager),
                status.server_id.clone(),
                descriptor.clone(),
                annotations,
            );
            let exposed = adapter.exposed_name().to_owned();
            if registry.get(&exposed).is_some() {
                error!(
                    exposed,
                    server = %status.server_id,
                    tool = %descriptor.name,
                    "mcp tool name collides with existing registration, skipping",
                );
                continue;
            }
            debug!(exposed, server = %status.server_id, tool = %descriptor.name, "registered mcp tool");
            registry.register(Arc::new(adapter));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use lpa_mcp::protocol::CallToolResult;
    use lpa_mcp::{McpError, McpManager, McpServerId, McpServerStatus, McpToolDescriptor};
    use lpa_safety::legacy_permissions::{PermissionMode, RuleBasedPolicy};
    use serde_json::json;

    #[derive(Default)]
    struct MockManager {
        responses: Mutex<Vec<Result<Value, McpError>>>,
    }

    impl MockManager {
        fn with_response(resp: Result<Value, McpError>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(vec![resp]),
            })
        }
    }

    #[async_trait]
    impl McpManager for MockManager {
        async fn statuses(&self) -> Result<Vec<McpServerStatus>, McpError> {
            Ok(Vec::new())
        }
        async fn refresh(&self, server_id: &McpServerId) -> Result<McpServerStatus, McpError> {
            Err(McpError::McpServerUnavailable {
                server_id: server_id.clone(),
            })
        }
        async fn invoke_tool(
            &self,
            _server_id: &McpServerId,
            _tool_name: &str,
            _input: Value,
        ) -> Result<Value, McpError> {
            self.responses
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| Ok(Value::Null))
        }
        async fn read_resource(
            &self,
            server_id: &McpServerId,
            uri: &str,
        ) -> Result<Value, McpError> {
            Err(McpError::McpResourceReadFailed {
                server_id: server_id.clone(),
                uri: uri.to_owned(),
                message: "mock".into(),
            })
        }
    }

    fn descriptor(name: &str) -> McpToolDescriptor {
        McpToolDescriptor {
            server_id: McpServerId("docs".into()),
            name: name.to_owned(),
            description: "mock tool".into(),
            input_schema: json!({"type": "object"}),
            annotations: None,
        }
    }

    fn fake_ctx() -> ToolContext {
        ToolContext {
            cwd: PathBuf::from("."),
            permissions: Arc::new(RuleBasedPolicy::new(PermissionMode::AutoApprove)),
            session_id: "test".into(),
        }
    }

    #[tokio::test]
    async fn adapter_returns_text_blocks_joined() {
        let result = CallToolResult {
            content: vec![
                ContentBlock::Text {
                    text: "hello".into(),
                },
                ContentBlock::Text {
                    text: "world".into(),
                },
            ],
            is_error: false,
        };
        let manager = MockManager::with_response(Ok(serde_json::to_value(&result).unwrap()));
        let adapter = McpToolAdapter::new(
            manager,
            McpServerId("docs".into()),
            descriptor("search"),
            None,
        );
        let out = adapter.execute(&fake_ctx(), json!({})).await.unwrap();
        assert_eq!(out.content, "hello\nworld");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn adapter_preserves_is_error_true_from_result() {
        let result = CallToolResult {
            content: vec![ContentBlock::Text {
                text: "boom".into(),
            }],
            is_error: true,
        };
        let manager = MockManager::with_response(Ok(serde_json::to_value(&result).unwrap()));
        let adapter = McpToolAdapter::new(
            manager,
            McpServerId("docs".into()),
            descriptor("search"),
            None,
        );
        let out = adapter.execute(&fake_ctx(), json!({})).await.unwrap();
        assert_eq!(out.content, "boom");
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn adapter_namespaces_exposed_name() {
        let manager = MockManager::with_response(Ok(Value::Null));
        let adapter = McpToolAdapter::new(
            manager,
            McpServerId("docs".into()),
            descriptor("search"),
            None,
        );
        assert_eq!(adapter.exposed_name(), "mcp__docs__search");
    }

    #[tokio::test]
    async fn adapter_maps_manager_error_to_tool_error() {
        let manager = MockManager::with_response(Err(McpError::McpServerUnavailable {
            server_id: McpServerId("docs".into()),
        }));
        let adapter = McpToolAdapter::new(
            manager,
            McpServerId("docs".into()),
            descriptor("search"),
            None,
        );
        let out = adapter.execute(&fake_ctx(), json!({})).await.unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn adapter_honors_read_only_hint() {
        let manager = MockManager::with_response(Ok(Value::Null));
        let annotations = ToolAnnotations {
            read_only_hint: Some(true),
            ..Default::default()
        };
        let adapter = McpToolAdapter::new(
            manager,
            McpServerId("docs".into()),
            descriptor("search"),
            Some(annotations),
        );
        assert!(adapter.is_read_only());
    }
}

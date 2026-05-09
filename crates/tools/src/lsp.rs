use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("lsp.txt");

pub struct LspTool;

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {"type": "string"},
                "filePath": {"type": "string"},
                "line": {"type": "integer"},
                "character": {"type": "integer"}
            },
            "required": ["operation", "filePath", "line", "character"]
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let operation = input["operation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'operation' field"))?;
        Ok(ToolOutput::success(format!(
            "LSP request received for {operation}"
        )))
    }
}

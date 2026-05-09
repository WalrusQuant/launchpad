use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

pub struct InvalidTool;

#[async_trait]
impl Tool for InvalidTool {
    fn name(&self) -> &str {
        "invalid"
    }

    fn description(&self) -> &str {
        "Do not use"
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "tool": {"type": "string"},
                "error": {"type": "string"}
            },
            "required": ["tool", "error"]
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let error = input["error"].as_str().unwrap_or("invalid tool arguments");
        Ok(ToolOutput::error(format!(
            "The arguments provided to the tool are invalid: {error}"
        )))
    }
}

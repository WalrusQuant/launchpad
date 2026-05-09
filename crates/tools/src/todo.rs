use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("todowrite.txt");

pub struct TodoWriteTool;

#[async_trait]
impl Tool for TodoWriteTool {
    fn name(&self) -> &str {
        "todowrite"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {"type": "array"}
            },
            "required": ["todos"]
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let todos = input["todos"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("missing 'todos' field"))?
            .clone();
        Ok(ToolOutput::success(serde_json::to_string_pretty(&todos)?))
    }
}

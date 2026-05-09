use async_trait::async_trait;
use serde_json::json;
use uuid::Uuid;

use crate::{Tool, ToolContext, ToolOutput};

const DESCRIPTION: &str = include_str!("task.txt");

pub struct TaskTool;

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self) -> &str {
        DESCRIPTION
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "description": {"type": "string"},
                "prompt": {"type": "string"},
                "subagent_type": {"type": "string"},
                "task_id": {"type": "string"},
                "command": {"type": "string"}
            },
            "required": ["description", "prompt", "subagent_type"]
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let description = input["description"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'description' field"))?;
        let task_id = input["task_id"]
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let prompt = input["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'prompt' field"))?;
        Ok(ToolOutput::success(format!(
            "task_id: {task_id} (for resuming to continue this task if needed)\n\n<task_result>\nTask requested: {description}\n{prompt}\n</task_result>"
        )))
    }
}

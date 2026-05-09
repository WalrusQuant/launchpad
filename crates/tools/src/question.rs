use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

pub struct QuestionTool;

#[async_trait]
impl Tool for QuestionTool {
    fn name(&self) -> &str {
        "question"
    }

    fn description(&self) -> &str {
        "Ask the user a clarifying question."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "question": {"type": "string"}
            },
            "required": ["question"]
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let question = input["question"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing 'question' field"))?;
        Ok(ToolOutput::success(format!(
            "Question for user: {question}"
        )))
    }
}

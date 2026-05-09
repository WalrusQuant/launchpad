use async_trait::async_trait;
use serde_json::json;

use crate::{Tool, ToolContext, ToolOutput};

pub struct PlanTool;

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }

    fn description(&self) -> &str {
        "Updates the task plan.\nProvide an optional explanation and a list of plan items, each with a step and status.\nAt most one step can be in_progress at a time."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "explanation": {
                    "type": "string"
                },
                "plan": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "step": {
                                "type": "string"
                            },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed"]
                            }
                        },
                        "required": ["step", "status"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["plan"],
            "additionalProperties": false
        })
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        let explanation = input
            .get("explanation")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let plan = input
            .get("plan")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("missing 'plan' field"))?;

        let in_progress_count = plan
            .iter()
            .filter(|item| {
                item.get("status").and_then(serde_json::Value::as_str) == Some("in_progress")
            })
            .count();
        if in_progress_count > 1 {
            return Ok(ToolOutput::error(
                "At most one step can be in_progress at a time.".to_string(),
            ));
        }

        let plan_text = serde_json::to_string_pretty(plan)?;
        let content = if explanation.trim().is_empty() {
            plan_text.clone()
        } else {
            format!("{explanation}\n\n{plan_text}")
        };

        Ok(ToolOutput {
            content,
            is_error: false,
            metadata: Some(json!({
                "explanation": explanation,
                "plan": plan,
            })),
        })
    }
}

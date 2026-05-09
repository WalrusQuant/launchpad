//! Summarization prompt contract for context compaction.
//!
//! Phase 2 (`LlmContextCompactor`) calls the summarization model with the
//! system prompt defined here and a user message built from
//! [`build_compaction_user_prompt`]. The model is expected to return a single
//! JSON object matching [`crate::ContextSummaryPayload`] (minus the
//! `covered_turn_sequences` and `generated_by_model` fields, which the
//! compactor fills in).

use lpa_protocol::{ContentBlock, Message, Role};

/// System instructions for the summarization model.
///
/// The prompt is intentionally terse: it names the role, lists the hard
/// content preservation rules from `docs/spec-context-management.md`, and pins
/// the output shape to a JSON object so downstream parsing stays deterministic.
pub const COMPACTION_SYSTEM_PROMPT: &str = "You are condensing an earlier portion of a coding-agent conversation into a compact summary that will replace it in the model's context window.\n\nYour summary must:\n- preserve the user's stated goals and active task\n- preserve concrete decisions, chosen approaches, and file paths touched\n- preserve unresolved questions, blockers, and open loops\n- omit verbose tool output, chain-of-thought, and resolved detours\n\nRespond with a single JSON object and nothing else. The object must match this shape exactly:\n{\n  \"summary_text\": string,\n  \"preserved_facts\": string[],\n  \"open_loops\": string[]\n}\n\n- summary_text: a paragraph-style narrative the model can read as context.\n- preserved_facts: short, concrete facts (file paths, decisions, names) worth keeping verbatim.\n- open_loops: unresolved TODOs or questions the next turn may need.\n\nEmit no prose before or after the JSON.";

/// Inputs required to render the user-facing compaction prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPromptInput<'a> {
    /// The serialized transcript covering the eligible message range.
    pub transcript: &'a str,
    /// Optional prior-summary text, included so re-compaction remains coherent.
    pub prior_summary: Option<&'a str>,
}

/// Renders the user message sent to the summarization model.
pub fn build_compaction_user_prompt(input: CompactionPromptInput<'_>) -> String {
    let mut out = String::new();
    if let Some(prior) = input.prior_summary {
        out.push_str("<prior_summary>\n");
        out.push_str(prior);
        out.push_str("\n</prior_summary>\n\n");
    }
    out.push_str("Summarize the following earlier conversation segment:\n\n");
    out.push_str("<transcript>\n");
    out.push_str(input.transcript);
    out.push_str("\n</transcript>\n\nReturn JSON only.");
    out
}

/// Renders a slice of messages as a compact transcript suitable for the
/// summarizer. Tool inputs and outputs are rendered inline but clearly
/// labeled so the model can distinguish them from conversation text.
pub fn serialize_transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        for block in &message.content {
            render_block(&mut out, message.role, block);
        }
    }
    out
}

fn render_block(out: &mut String, role: Role, block: &ContentBlock) {
    match block {
        ContentBlock::Text { text } => {
            let label = match role {
                Role::User => "[user]",
                Role::Assistant => "[assistant]",
            };
            out.push_str(label);
            out.push(' ');
            out.push_str(text.trim());
            out.push('\n');
        }
        ContentBlock::ToolUse { name, input, .. } => {
            out.push_str("[tool_call ");
            out.push_str(name);
            out.push_str("] ");
            out.push_str(&input.to_string());
            out.push('\n');
        }
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            if *is_error {
                out.push_str("[tool_error] ");
            } else {
                out.push_str("[tool_result] ");
            }
            out.push_str(content.trim());
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input,
            }],
        }
    }

    fn tool_result(tool_use_id: &str, content: &str, is_error: bool) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error,
            }],
        }
    }

    #[test]
    fn system_prompt_pins_json_shape_fields() {
        assert!(COMPACTION_SYSTEM_PROMPT.contains("\"summary_text\""));
        assert!(COMPACTION_SYSTEM_PROMPT.contains("\"preserved_facts\""));
        assert!(COMPACTION_SYSTEM_PROMPT.contains("\"open_loops\""));
    }

    #[test]
    fn user_prompt_wraps_transcript_in_tags() {
        let prompt = build_compaction_user_prompt(CompactionPromptInput {
            transcript: "[user] hi\n[assistant] hello\n",
            prior_summary: None,
        });
        assert_eq!(
            prompt,
            "Summarize the following earlier conversation segment:\n\n<transcript>\n[user] hi\n[assistant] hello\n\n</transcript>\n\nReturn JSON only.",
        );
    }

    #[test]
    fn user_prompt_includes_prior_summary_when_present() {
        let prompt = build_compaction_user_prompt(CompactionPromptInput {
            transcript: "[user] new\n",
            prior_summary: Some("earlier summary text"),
        });
        assert_eq!(
            prompt,
            "<prior_summary>\nearlier summary text\n</prior_summary>\n\nSummarize the following earlier conversation segment:\n\n<transcript>\n[user] new\n\n</transcript>\n\nReturn JSON only.",
        );
    }

    #[test]
    fn transcript_renders_text_tool_call_and_result() {
        let messages = vec![
            Message::user("do a thing"),
            tool_use("t1", "bash", serde_json::json!({"command":"ls"})),
            tool_result("t1", "file.txt\n", false),
            Message::assistant_text("found one file"),
        ];
        assert_eq!(
            serialize_transcript(&messages),
            "[user] do a thing\n[tool_call bash] {\"command\":\"ls\"}\n[tool_result] file.txt\n[assistant] found one file\n",
        );
    }

    #[test]
    fn transcript_labels_error_tool_results() {
        let messages = vec![tool_result("t1", "boom", true)];
        assert_eq!(serialize_transcript(&messages), "[tool_error] boom\n");
    }

    #[test]
    fn transcript_is_empty_for_empty_input() {
        assert_eq!(serialize_transcript(&[]), "");
    }
}

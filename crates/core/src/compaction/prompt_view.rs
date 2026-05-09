//! Prompt view reconstruction after compaction.
//!
//! Once compaction produces a summary, the prompt sent to the model replaces
//! the compacted prefix with a single user-role message carrying the summary
//! text, and then appends the uncompacted recent messages and current input.
//! This module owns that rebuild so the query loop (Phase 5) stays focused on
//! transport and streaming concerns.

use lpa_protocol::{ContentBlock, Message, Role};

use crate::ContextSummaryPayload;

/// Header prefixed to the summary message so the model recognizes the block
/// as compacted context rather than user input.
pub const COMPACTED_SUMMARY_HEADER: &str = "<compacted_context>\nThe following summary replaces earlier conversation turns that were dropped from the live context window. Treat it as authoritative background.\n";

/// Footer closing the compacted-context block.
pub const COMPACTED_SUMMARY_FOOTER: &str = "\n</compacted_context>";

/// Active-compaction state used to rebuild a prompt view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCompaction {
    /// The payload produced by the summarizer.
    pub summary: ContextSummaryPayload,
    /// Number of leading messages represented by the summary; messages at
    /// indices `0..replaced_prefix_len` must be replaced in the prompt view.
    pub replaced_prefix_len: usize,
}

/// Rebuilds the prompt view presented to the model.
///
/// When `compaction` is `None`, returns a clone of `messages` unchanged. When
/// compaction is active, returns
/// `[summary_message, ...messages[replaced_prefix_len..]]`.
pub fn rebuild_prompt_view(
    messages: &[Message],
    compaction: Option<&ActiveCompaction>,
) -> Vec<Message> {
    let Some(compaction) = compaction else {
        return messages.to_vec();
    };

    let replaced = compaction.replaced_prefix_len.min(messages.len());
    let mut view = Vec::with_capacity(messages.len().saturating_sub(replaced) + 1);
    view.push(summary_message(&compaction.summary));
    view.extend(messages[replaced..].iter().cloned());
    view
}

/// Renders a compaction summary as a single user-role message.
pub fn summary_message(summary: &ContextSummaryPayload) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: render_summary_text(summary),
        }],
    }
}

fn render_summary_text(summary: &ContextSummaryPayload) -> String {
    let mut out = String::new();
    out.push_str(COMPACTED_SUMMARY_HEADER);
    out.push_str(summary.summary_text.trim());
    if !summary.preserved_facts.is_empty() {
        out.push_str("\n\nPreserved facts:");
        for fact in &summary.preserved_facts {
            out.push_str("\n- ");
            out.push_str(fact);
        }
    }
    if !summary.open_loops.is_empty() {
        out.push_str("\n\nOpen loops:");
        for loop_item in &summary.open_loops {
            out.push_str("\n- ");
            out.push_str(loop_item);
        }
    }
    out.push_str(COMPACTED_SUMMARY_FOOTER);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn summary(text: &str) -> ContextSummaryPayload {
        ContextSummaryPayload {
            summary_text: text.into(),
            covered_turn_sequences: vec![1, 2],
            preserved_facts: vec!["kept decision".into()],
            open_loops: vec!["outstanding TODO".into()],
            generated_by_model: "summary-model".into(),
        }
    }

    #[test]
    fn rebuild_returns_messages_unchanged_when_no_compaction() {
        let messages = vec![Message::user("hello"), Message::assistant_text("hi")];
        assert_eq!(rebuild_prompt_view(&messages, None), messages);
    }

    #[test]
    fn rebuild_replaces_prefix_with_summary_message() {
        let messages = vec![
            Message::user("old turn 1"),
            Message::assistant_text("r1"),
            Message::user("old turn 2"),
            Message::assistant_text("r2"),
            Message::user("recent"),
            Message::assistant_text("now"),
        ];
        let compaction = ActiveCompaction {
            summary: summary("replaced prior work"),
            replaced_prefix_len: 4,
        };

        let view = rebuild_prompt_view(&messages, Some(&compaction));
        assert_eq!(view.len(), 3);
        match &view[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("replaced prior work"));
                assert!(text.contains("Preserved facts"));
                assert!(text.contains("Open loops"));
            }
            other => panic!("expected text block, got {other:?}"),
        }
        assert_eq!(view[1..].to_vec(), messages[4..].to_vec());
    }

    #[test]
    fn rebuild_clamps_prefix_that_exceeds_history() {
        let messages = vec![Message::user("only"), Message::assistant_text("one")];
        let compaction = ActiveCompaction {
            summary: summary("something"),
            replaced_prefix_len: 99,
        };
        let view = rebuild_prompt_view(&messages, Some(&compaction));
        assert_eq!(view.len(), 1);
    }

    #[test]
    fn summary_message_is_user_role_with_single_text_block() {
        let message = summary_message(&summary("s"));
        assert_eq!(message.role, Role::User);
        assert_eq!(message.content.len(), 1);
    }

    #[test]
    fn render_omits_empty_fact_and_loop_sections() {
        let payload = ContextSummaryPayload {
            summary_text: "bare".into(),
            covered_turn_sequences: Vec::new(),
            preserved_facts: Vec::new(),
            open_loops: Vec::new(),
            generated_by_model: "m".into(),
        };
        let text = render_summary_text(&payload);
        assert!(text.contains("bare"));
        assert!(!text.contains("Preserved facts"));
        assert!(!text.contains("Open loops"));
    }
}

//! Eligibility selection for context compaction.
//!
//! The selector scans a conversation's `Vec<Message>` journal and returns the
//! prefix range that may be replaced by a summary. It preserves the contract
//! defined in `docs/spec-context-management.md` §Eligibility Rules:
//!
//! - the current and last `K` complete turns are protected
//! - partial (in-progress) turns are never summarized
//! - tool call and tool result pairs stay bonded within the eligible range

use lpa_protocol::{ContentBlock, Message, Role};

/// Default number of recent complete turns to preserve when not specified.
pub const DEFAULT_PRESERVE_RECENT_TURNS: usize = 3;

/// A contiguous half-open range of message indices eligible for summarization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EligibleRange {
    /// First eligible message index (inclusive).
    pub start: usize,
    /// One past the last eligible message index (exclusive).
    pub end: usize,
}

impl EligibleRange {
    /// Number of messages in the range.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the range contains zero messages.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Selects which prefix of a message journal is eligible for summarization.
///
/// Callers that want a different preservation window construct the selector with
/// [`EligibilitySelector::new`]. The selector treats turn boundaries as the
/// index of each "real" user message: a `Role::User` message that carries at
/// least one non-`ToolResult` content block. Messages composed purely of
/// `ToolResult` blocks are tool continuations, not new turns, so they stay
/// bonded to the preceding assistant message.
pub struct EligibilitySelector {
    preserve_recent_turns: usize,
}

impl Default for EligibilitySelector {
    fn default() -> Self {
        Self::new(DEFAULT_PRESERVE_RECENT_TURNS)
    }
}

impl EligibilitySelector {
    /// Creates a selector that preserves at least one recent turn.
    pub fn new(preserve_recent_turns: usize) -> Self {
        Self {
            preserve_recent_turns: preserve_recent_turns.max(1),
        }
    }

    /// Returns the preservation window.
    pub fn preserve_recent_turns(&self) -> usize {
        self.preserve_recent_turns
    }

    /// Returns the prefix of `messages` eligible for summarization, or `None`
    /// when there is not enough completed history to compact safely.
    pub fn select(&self, messages: &[Message]) -> Option<EligibleRange> {
        let turn_starts = collect_turn_starts(messages);
        if turn_starts.len() <= self.preserve_recent_turns {
            return None;
        }

        let mut boundary_idx = turn_starts.len() - self.preserve_recent_turns;
        while boundary_idx > 0 {
            let end = turn_starts[boundary_idx];
            if has_paired_tools(&messages[..end]) {
                return Some(EligibleRange { start: 0, end });
            }
            boundary_idx -= 1;
        }
        None
    }
}

/// Indices of messages that start a new turn (a user message with at least one
/// non-`ToolResult` content block).
fn collect_turn_starts(messages: &[Message]) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| is_turn_start(message).then_some(index))
        .collect()
}

fn is_turn_start(message: &Message) -> bool {
    match message.role {
        Role::User => message
            .content
            .iter()
            .any(|block| !matches!(block, ContentBlock::ToolResult { .. })),
        Role::Assistant => false,
    }
}

/// Returns true when every `ToolUse` in `prefix` is matched by a `ToolResult`
/// also contained in `prefix`.
fn has_paired_tools(prefix: &[Message]) -> bool {
    let mut open: Vec<&str> = Vec::new();
    for message in prefix {
        for block in &message.content {
            match block {
                ContentBlock::ToolUse { id, .. } => open.push(id),
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    if let Some(position) = open.iter().position(|id| *id == tool_use_id) {
                        open.swap_remove(position);
                    }
                }
                ContentBlock::Text { .. } => {}
            }
        }
    }
    open.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn user_text(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant_text(text: &str) -> Message {
        Message::assistant_text(text)
    }

    fn assistant_tool_use(id: &str, name: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: serde_json::json!({}),
            }],
        }
    }

    fn tool_result(tool_use_id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: tool_use_id.to_string(),
                content: content.to_string(),
                is_error: false,
            }],
        }
    }

    #[test]
    fn returns_none_when_history_is_empty() {
        let selector = EligibilitySelector::default();
        assert_eq!(selector.select(&[]), None);
    }

    #[test]
    fn returns_none_when_fewer_turns_than_window() {
        let selector = EligibilitySelector::new(3);
        let messages = vec![
            user_text("hi"),
            assistant_text("hello"),
            user_text("do thing"),
            assistant_text("done"),
        ];
        assert_eq!(selector.select(&messages), None);
    }

    #[test]
    fn returns_none_when_only_window_turns_present() {
        let selector = EligibilitySelector::new(3);
        let messages = vec![
            user_text("one"),
            assistant_text("r1"),
            user_text("two"),
            assistant_text("r2"),
            user_text("three"),
            assistant_text("r3"),
        ];
        assert_eq!(selector.select(&messages), None);
    }

    #[test]
    fn eligible_range_excludes_last_k_turns() {
        let selector = EligibilitySelector::new(2);
        let messages = vec![
            user_text("one"),
            assistant_text("r1"),
            user_text("two"),
            assistant_text("r2"),
            user_text("three"),
            assistant_text("r3"),
            user_text("four"),
            assistant_text("r4"),
        ];
        assert_eq!(
            selector.select(&messages),
            Some(EligibleRange { start: 0, end: 4 }),
        );
    }

    #[test]
    fn tool_result_messages_do_not_start_new_turns() {
        let selector = EligibilitySelector::new(2);
        let messages = vec![
            user_text("turn-1 start"),
            assistant_tool_use("t1", "bash"),
            tool_result("t1", "ok"),
            assistant_text("turn-1 done"),
            user_text("turn-2 start"),
            assistant_text("turn-2 done"),
            user_text("turn-3 start"),
            assistant_text("turn-3 done"),
        ];
        assert_eq!(
            selector.select(&messages),
            Some(EligibleRange { start: 0, end: 4 }),
        );
    }

    #[test]
    fn partial_final_turn_does_not_break_selection() {
        let selector = EligibilitySelector::new(2);
        let messages = vec![
            user_text("turn-1"),
            assistant_text("r1"),
            user_text("turn-2"),
            assistant_text("r2"),
            user_text("turn-3 in progress"),
        ];
        assert_eq!(
            selector.select(&messages),
            Some(EligibleRange { start: 0, end: 2 }),
        );
    }

    #[test]
    fn unpaired_tool_use_in_eligible_range_is_trimmed_out() {
        let selector = EligibilitySelector::new(2);
        let messages = vec![
            user_text("turn-1"),
            assistant_text("r1"),
            user_text("turn-2"),
            assistant_tool_use("t1", "bash"),
            user_text("turn-3"),
            assistant_text("r3"),
            tool_result("t1", "late result"),
            user_text("turn-4"),
            assistant_text("r4"),
        ];
        assert_eq!(
            selector.select(&messages),
            Some(EligibleRange { start: 0, end: 2 }),
        );
    }

    #[test]
    fn selector_clamps_preserve_to_at_least_one() {
        let selector = EligibilitySelector::new(0);
        assert_eq!(selector.preserve_recent_turns(), 1);
        let messages = vec![
            user_text("one"),
            assistant_text("r1"),
            user_text("two"),
            assistant_text("r2"),
        ];
        assert_eq!(
            selector.select(&messages),
            Some(EligibleRange { start: 0, end: 2 }),
        );
    }

    #[test]
    fn eligible_range_len_and_is_empty() {
        let full = EligibleRange { start: 0, end: 4 };
        let empty = EligibleRange { start: 5, end: 5 };
        assert_eq!(full.len(), 4);
        assert!(!full.is_empty());
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }
}

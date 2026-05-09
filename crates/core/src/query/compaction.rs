use crate::SessionState;

pub(super) fn compact_session(session: &mut SessionState) -> usize {
    let msg_count = session.messages.len();
    if msg_count <= 2 {
        return 0;
    }

    let input_budget = session.config.token_budget.input_budget();
    let last_tokens = session.last_input_tokens;

    if last_tokens == 0 {
        let remove = msg_count / 2;
        session.messages.drain(..remove);
        return remove;
    }

    let avg_tokens_per_msg = last_tokens / msg_count;
    if avg_tokens_per_msg == 0 {
        let remove = msg_count / 2;
        session.messages.drain(..remove);
        return remove;
    }

    let target_tokens = (input_budget as f64 * 0.7) as usize;
    let keep_count = (target_tokens / avg_tokens_per_msg).max(2).min(msg_count);
    let remove_count = msg_count - keep_count;

    if remove_count > 0 {
        session.messages.drain(..remove_count);
    }
    remove_count
}

const MICRO_COMPACT_THRESHOLD: usize = 10_000;

pub(super) fn micro_compact(content: String) -> String {
    if content.len() > MICRO_COMPACT_THRESHOLD {
        let truncate_at = content
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= MICRO_COMPACT_THRESHOLD)
            .last()
            .unwrap_or(0);
        let mut truncated = content[..truncate_at].to_string();
        truncated.push_str("\n...[truncated]");
        truncated
    } else {
        content
    }
}

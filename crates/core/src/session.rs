use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use lpa_safety::legacy_permissions::PermissionMode;

use crate::{ActiveCompaction, Message, Model, TokenBudget, rebuild_prompt_view};

/// Configuration for a session.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub token_budget: TokenBudget,
    pub permission_mode: PermissionMode,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            token_budget: TokenBudget::default(),
            permission_mode: PermissionMode::AutoApprove,
        }
    }
}

/// Per-turn execution settings resolved before the query loop starts.
#[derive(Debug, Clone)]
pub struct TurnConfig {
    pub model: Model,
    pub thinking_selection: Option<String>,
}

/// Mutable state for one conversation session.
///
/// This corresponds to the session-level state in Claude Code's
/// `AppStateStore` and `QueryEngine`, but stripped of UI concerns.
pub struct SessionState {
    pub id: String,
    pub config: SessionConfig,
    pub messages: Vec<Message>,
    pub cwd: PathBuf,
    pub turn_count: usize,
    pub total_input_tokens: usize,
    pub total_output_tokens: usize,
    pub total_cache_creation_tokens: usize,
    pub total_cache_read_tokens: usize,
    /// Input tokens reported by the model for the most recent turn.
    /// Used by `TokenBudget::should_compact()` to decide when to compact.
    pub last_input_tokens: usize,
    /// User prompts queued while a turn is already running and injected before
    /// the next model request in the same query loop.
    pub pending_user_prompts: Arc<Mutex<VecDeque<String>>>,
    /// Active compaction summary that rewrites the prompt view.
    ///
    /// Raw history in [`Self::messages`] is never mutated by compaction; the
    /// summary is applied when building a prompt via
    /// [`Self::to_prompt_messages`]. `replaced_prefix_len` counts messages
    /// from the start of [`Self::messages`], so later appends stay outside the
    /// compacted prefix.
    pub active_compaction: Option<ActiveCompaction>,
    /// Concurrency lock enforcing "only one compaction per session at a time"
    /// per the context-management spec. Held as an `AtomicBool` so compaction
    /// can be gated without blocking async tasks on a mutex.
    compacting: Arc<AtomicBool>,
}

impl SessionState {
    pub fn new(config: SessionConfig, cwd: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            config,
            messages: Vec::new(),
            cwd,
            turn_count: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cache_creation_tokens: 0,
            total_cache_read_tokens: 0,
            last_input_tokens: 0,
            pending_user_prompts: Arc::new(Mutex::new(VecDeque::new())),
            active_compaction: None,
            compacting: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn push_message(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    /// Returns the raw message journal rendered as provider request messages,
    /// without applying any compaction. Retained for callers that need to
    /// inspect the raw history.
    pub fn to_request_messages(&self) -> Vec<lpa_protocol::RequestMessage> {
        self.messages
            .iter()
            .map(|m| m.to_request_message())
            .collect()
    }

    /// Returns the compaction-aware prompt view: when
    /// [`Self::active_compaction`] is set, the compacted prefix is replaced by
    /// a single summary-bearing user message followed by the uncompacted
    /// remainder; otherwise identical to [`Self::to_request_messages`].
    pub fn to_prompt_messages(&self) -> Vec<lpa_protocol::RequestMessage> {
        rebuild_prompt_view(&self.messages, self.active_compaction.as_ref())
            .iter()
            .map(|m| m.to_request_message())
            .collect()
    }

    pub fn enqueue_user_prompt(&self, prompt: String) {
        self.pending_user_prompts
            .lock()
            .expect("pending user prompts mutex should not be poisoned")
            .push_back(prompt);
    }

    pub fn drain_pending_user_prompts(&self) -> Vec<String> {
        let mut pending = self
            .pending_user_prompts
            .lock()
            .expect("pending user prompts mutex should not be poisoned");
        pending.drain(..).collect()
    }

    /// Attempts to acquire the per-session compaction lock. Returns `Some`
    /// guard when acquired; `None` when a compaction is already running.
    pub fn try_begin_compaction(&self) -> Option<CompactionGuard> {
        self.compacting
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| CompactionGuard {
                flag: self.compacting.clone(),
            })
    }
}

/// RAII guard released when the compaction flow completes or errors.
pub struct CompactionGuard {
    flag: Arc<AtomicBool>,
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_config_default_values() {
        let config = SessionConfig::default();
        assert_eq!(config.permission_mode, PermissionMode::AutoApprove);
    }

    #[test]
    fn session_state_new_initializes_correctly() {
        let config = SessionConfig::default();
        let cwd = PathBuf::from("/tmp");
        let state = SessionState::new(config, cwd.clone());

        assert!(!state.id.is_empty());
        assert!(state.messages.is_empty());
        assert_eq!(state.cwd, cwd);
        assert_eq!(state.turn_count, 0);
        assert_eq!(state.total_input_tokens, 0);
        assert_eq!(state.total_output_tokens, 0);
    }

    #[test]
    fn session_state_push_message() {
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.push_message(Message::user("hello"));
        state.push_message(Message::assistant_text("hi"));
        assert_eq!(state.messages.len(), 2);
    }

    #[test]
    fn session_state_to_request_messages() {
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.push_message(Message::user("hello"));
        state.push_message(Message::assistant_text("hi"));

        let req_msgs = state.to_request_messages();
        assert_eq!(req_msgs.len(), 2);
        assert_eq!(req_msgs[0].role, "user");
        assert_eq!(req_msgs[1].role, "assistant");
    }

    #[test]
    fn session_state_unique_ids() {
        let s1 = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        let s2 = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        assert_ne!(s1.id, s2.id);
    }

    #[test]
    fn session_state_drains_pending_user_prompts() {
        let state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.enqueue_user_prompt("first".to_string());
        state.enqueue_user_prompt("second".to_string());

        assert_eq!(
            state.drain_pending_user_prompts(),
            vec!["first".to_string(), "second".to_string()]
        );
        assert!(state.drain_pending_user_prompts().is_empty());
    }

    #[test]
    fn to_prompt_messages_matches_raw_when_no_compaction() {
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.push_message(Message::user("hello"));
        state.push_message(Message::assistant_text("hi"));
        assert_eq!(state.to_prompt_messages().len(), 2);
    }

    #[test]
    fn to_prompt_messages_applies_active_compaction() {
        use crate::ContextSummaryPayload;
        let mut state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        state.push_message(Message::user("old one"));
        state.push_message(Message::assistant_text("r1"));
        state.push_message(Message::user("old two"));
        state.push_message(Message::assistant_text("r2"));
        state.push_message(Message::user("recent"));
        state.active_compaction = Some(ActiveCompaction {
            summary: ContextSummaryPayload {
                summary_text: "earlier context".into(),
                covered_turn_sequences: vec![1, 2],
                preserved_facts: Vec::new(),
                open_loops: Vec::new(),
                generated_by_model: "m".into(),
            },
            replaced_prefix_len: 4,
        });
        let prompt = state.to_prompt_messages();
        assert_eq!(prompt.len(), 2);
        assert_eq!(prompt[1].role, "user");
    }

    #[test]
    fn compaction_guard_enforces_single_compaction() {
        let state = SessionState::new(SessionConfig::default(), PathBuf::from("/tmp"));
        let first = state.try_begin_compaction().expect("first acquires");
        assert!(state.try_begin_compaction().is_none());
        drop(first);
        assert!(state.try_begin_compaction().is_some());
    }
}

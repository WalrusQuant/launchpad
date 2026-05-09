//! End-to-end LLM compaction flow glue.
//!
//! [`run_llm_compaction`] strings the phase-1 selector, the phase-2 compactor,
//! and the phase-4 prompt-view state together for the query loop. It is the
//! one public entry point the rest of the codebase should use to trigger a
//! summarization; individual pieces remain available for tests.

use std::sync::Arc;

use lpa_provider::ModelProviderSDK;
use tracing::{info, warn};

use crate::{
    ActiveCompaction, CompactionError, ContextCompactor, ContextSummaryPayload, Message,
    PromptAssemblyInput, SessionState,
};

use super::COMPACTION_SYSTEM_PROMPT;
use super::llm_compactor::LlmContextCompactor;
use super::prompt::{CompactionPromptInput, build_compaction_user_prompt, serialize_transcript};
use super::selector::EligibilitySelector;

/// Reports the result of one compaction attempt so callers can persist the
/// snapshot, emit events, and drive UI updates without re-inspecting
/// [`SessionState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionOutcome {
    /// The summary the model produced.
    pub summary: ContextSummaryPayload,
    /// Count of leading messages the summary now represents.
    pub replaced_prefix_len: usize,
    /// Whether the outcome replaced a prior summary (i.e. recompaction).
    pub replaced_prior_summary: bool,
}

/// Runs the full compaction flow against `session`.
///
/// Returns `Ok(Some(outcome))` when a new summary was produced,
/// `Ok(None)` when the selector decided there was nothing to compact, and
/// `Err` when the summarizer provider failed. The session's
/// [`SessionState::active_compaction`] is only mutated on `Ok(Some(..))`, and
/// the per-session compaction lock is held for the duration of the call.
pub async fn run_llm_compaction(
    session: &mut SessionState,
    provider: Arc<dyn ModelProviderSDK>,
    model_slug: &str,
    selector: &EligibilitySelector,
) -> Result<Option<CompactionOutcome>, CompactionError> {
    let Some(_guard) = session.try_begin_compaction() else {
        info!("skipping compaction: another compaction is in flight for this session");
        return Ok(None);
    };

    let Some(range) = selector.select(&session.messages) else {
        return Ok(None);
    };
    if range.is_empty() {
        return Ok(None);
    }

    let eligible: Vec<Message> = session.messages[range.start..range.end].to_vec();
    let transcript = serialize_transcript(&eligible);
    let prior_summary = session
        .active_compaction
        .as_ref()
        .map(|compaction| compaction.summary.summary_text.clone());

    let user_prompt = build_compaction_user_prompt(CompactionPromptInput {
        transcript: &transcript,
        prior_summary: prior_summary.as_deref(),
    });

    let prompt_input = PromptAssemblyInput {
        base_instructions: COMPACTION_SYSTEM_PROMPT.to_string(),
        tool_definitions: Vec::new(),
        safety_constraints: Vec::new(),
        history_items: vec![user_prompt],
        current_input: Vec::new(),
    };

    let compactor = LlmContextCompactor::new(provider, model_slug.to_string());
    let budget = session.config.token_budget.clone();
    let summary = compactor.compact(prompt_input, budget).await?;

    info!(
        replaced_prefix_len = range.end,
        model = model_slug,
        summary_chars = summary.summary_text.len(),
        "context compaction succeeded"
    );

    let replaced_prior_summary = session.active_compaction.is_some();
    session.active_compaction = Some(ActiveCompaction {
        summary: summary.clone(),
        replaced_prefix_len: range.end,
    });

    Ok(Some(CompactionOutcome {
        summary,
        replaced_prefix_len: range.end,
        replaced_prior_summary,
    }))
}

/// Logs a compaction failure at warn level. Intended for the fallback path in
/// the query loop so the call site stays short.
pub fn warn_compaction_failed(error: &CompactionError) {
    warn!(%error, "llm compaction failed; falling back to naive drop");
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use lpa_protocol::{
        ModelRequest, ModelResponse, ResponseContent, ResponseMetadata, StopReason, StreamEvent,
        Usage,
    };
    use lpa_provider::ProviderError;
    use pretty_assertions::assert_eq;
    use std::pin::Pin;
    use std::sync::Mutex;

    struct StubProvider {
        response: Mutex<Option<Result<ModelResponse, lpa_provider::ProviderError>>>,
    }

    impl StubProvider {
        fn with_response(body: &str) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Some(Ok(ModelResponse {
                    id: "resp".into(),
                    content: vec![ResponseContent::Text(body.to_string())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: ResponseMetadata::default(),
                }))),
            })
        }

        fn failing(message: &str) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Some(Err(lpa_provider::ProviderError::Other {
                    message: message.to_string(),
                    source: None,
                }))),
            })
        }
    }

    #[async_trait]
    impl ModelProviderSDK for StubProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse, ProviderError> {
            self.response.lock().unwrap().take().expect("called once")
        }

        async fn completion_stream(
            &self,
            _request: ModelRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
            ProviderError,
        > {
            unimplemented!("not used")
        }

        fn name(&self) -> &str {
            "stub"
        }
    }

    fn session_with_history(messages: &[(bool, &str)]) -> SessionState {
        use std::path::PathBuf;
        let mut session = SessionState::new(crate::SessionConfig::default(), PathBuf::from("/tmp"));
        for (is_user, text) in messages {
            let message = if *is_user {
                Message::user(*text)
            } else {
                Message::assistant_text(*text)
            };
            session.push_message(message);
        }
        session
    }

    #[tokio::test]
    async fn returns_none_when_selector_has_nothing_to_compact() {
        let provider = StubProvider::with_response(
            r#"{"summary_text":"ok","preserved_facts":[],"open_loops":[]}"#,
        );
        let mut session = session_with_history(&[(true, "hi"), (false, "hello")]);
        let outcome = run_llm_compaction(&mut session, provider, "m", &EligibilitySelector::new(3))
            .await
            .expect("no error");
        assert_eq!(outcome, None);
        assert!(session.active_compaction.is_none());
    }

    #[tokio::test]
    async fn populates_active_compaction_on_success() {
        let provider = StubProvider::with_response(
            r#"{"summary_text":"earlier work","preserved_facts":["a.rs"],"open_loops":["finish b"]}"#,
        );
        let mut session = session_with_history(&[
            (true, "t1"),
            (false, "r1"),
            (true, "t2"),
            (false, "r2"),
            (true, "t3"),
            (false, "r3"),
            (true, "t4"),
            (false, "r4"),
        ]);
        let outcome = run_llm_compaction(
            &mut session,
            provider,
            "summary-model",
            &EligibilitySelector::new(2),
        )
        .await
        .expect("success")
        .expect("some outcome");

        assert_eq!(outcome.replaced_prefix_len, 4);
        assert!(!outcome.replaced_prior_summary);
        assert_eq!(outcome.summary.summary_text, "earlier work");
        let active = session
            .active_compaction
            .expect("compaction persisted on session");
        assert_eq!(active.replaced_prefix_len, 4);
    }

    #[tokio::test]
    async fn recompaction_marks_prior_summary_replaced() {
        let provider = StubProvider::with_response(
            r#"{"summary_text":"second","preserved_facts":[],"open_loops":[]}"#,
        );
        let mut session = session_with_history(&[
            (true, "t1"),
            (false, "r1"),
            (true, "t2"),
            (false, "r2"),
            (true, "t3"),
            (false, "r3"),
            (true, "t4"),
            (false, "r4"),
        ]);
        session.active_compaction = Some(ActiveCompaction {
            summary: ContextSummaryPayload {
                summary_text: "first".into(),
                covered_turn_sequences: Vec::new(),
                preserved_facts: Vec::new(),
                open_loops: Vec::new(),
                generated_by_model: "m".into(),
            },
            replaced_prefix_len: 2,
        });
        let outcome = run_llm_compaction(
            &mut session,
            provider,
            "summary-model",
            &EligibilitySelector::new(2),
        )
        .await
        .expect("ok")
        .expect("some");
        assert!(outcome.replaced_prior_summary);
    }

    #[tokio::test]
    async fn surfaces_provider_error_without_mutating_state() {
        let provider = StubProvider::failing("upstream 503");
        let mut session = session_with_history(&[
            (true, "t1"),
            (false, "r1"),
            (true, "t2"),
            (false, "r2"),
            (true, "t3"),
            (false, "r3"),
            (true, "t4"),
            (false, "r4"),
        ]);
        let result =
            run_llm_compaction(&mut session, provider, "m", &EligibilitySelector::new(2)).await;
        assert!(matches!(
            result,
            Err(CompactionError::SummaryProviderFailed { .. })
        ));
        assert!(session.active_compaction.is_none());
    }

    #[tokio::test]
    async fn respects_concurrency_lock() {
        let provider = StubProvider::with_response(
            r#"{"summary_text":"ok","preserved_facts":[],"open_loops":[]}"#,
        );
        let mut session = session_with_history(&[
            (true, "t1"),
            (false, "r1"),
            (true, "t2"),
            (false, "r2"),
            (true, "t3"),
            (false, "r3"),
            (true, "t4"),
            (false, "r4"),
        ]);
        let _guard = session
            .try_begin_compaction()
            .expect("test acquires the lock");
        let outcome = run_llm_compaction(&mut session, provider, "m", &EligibilitySelector::new(2))
            .await
            .expect("lock skip is not an error");
        assert_eq!(outcome, None);
    }
}

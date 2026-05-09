//! LLM-driven implementation of [`ContextCompactor`].
//!
//! The compactor takes a serialized eligible-history prefix (provided via
//! [`PromptAssemblyInput::history_items`]), invokes the configured summary
//! model with the prompt defined in [`super::prompt`], and parses the model's
//! JSON response into a [`ContextSummaryPayload`].
//!
//! The `covered_turn_sequences` field is left empty in this layer. The query
//! loop (Phase 5) owns turn identity and fills it in before persisting the
//! summary alongside its snapshot.

use std::sync::Arc;

use async_trait::async_trait;
use lpa_protocol::{
    ModelRequest, RequestContent, RequestMessage, ResponseContent, SamplingControls,
};
use lpa_provider::ModelProviderSDK;
#[cfg(test)]
use lpa_provider::ProviderError;
use serde::Deserialize;

use crate::{
    CompactionError, ContextCompactor, ContextSummaryPayload, PromptAssemblyInput, TokenBudget,
};

use super::prompt::{
    COMPACTION_SYSTEM_PROMPT, CompactionPromptInput, build_compaction_user_prompt,
};

/// Default cap on tokens allocated to the summary response itself. Summaries
/// should be terse; the cap also bounds cost when `UseTurnModel` is selected
/// and the turn model is large.
pub const DEFAULT_SUMMARY_MAX_OUTPUT_TOKENS: usize = 2_048;

/// Builds [`LlmContextCompactor`] instances with explicit dependencies.
pub struct LlmContextCompactor {
    provider: Arc<dyn ModelProviderSDK>,
    model_slug: String,
    max_output_tokens: usize,
}

impl LlmContextCompactor {
    /// Creates a compactor bound to a specific provider and summary model.
    pub fn new(provider: Arc<dyn ModelProviderSDK>, model_slug: impl Into<String>) -> Self {
        Self {
            provider,
            model_slug: model_slug.into(),
            max_output_tokens: DEFAULT_SUMMARY_MAX_OUTPUT_TOKENS,
        }
    }

    /// Overrides the maximum tokens the summary model may emit.
    pub fn with_max_output_tokens(mut self, max_output_tokens: usize) -> Self {
        self.max_output_tokens = max_output_tokens.max(128);
        self
    }

    /// Returns the resolved summary model slug.
    pub fn model_slug(&self) -> &str {
        &self.model_slug
    }
}

#[async_trait]
impl ContextCompactor for LlmContextCompactor {
    async fn compact(
        &self,
        prompt: PromptAssemblyInput,
        _budget: TokenBudget,
    ) -> Result<ContextSummaryPayload, CompactionError> {
        if prompt.history_items.is_empty() {
            return Err(CompactionError::CompactionNotPossible {
                message: "no eligible history items to summarize".into(),
            });
        }

        let transcript = prompt.history_items.join("");
        let user_prompt = build_compaction_user_prompt(CompactionPromptInput {
            transcript: &transcript,
            prior_summary: None,
        });

        let request = ModelRequest {
            model: self.model_slug.clone(),
            system: Some(COMPACTION_SYSTEM_PROMPT.to_string()),
            messages: vec![RequestMessage {
                role: "user".to_string(),
                content: vec![RequestContent::Text { text: user_prompt }],
            }],
            max_tokens: self.max_output_tokens,
            tools: None,
            sampling: SamplingControls::default(),
            thinking: None,
            extra_body: None,
        };

        let response = self.provider.completion(request).await.map_err(|err| {
            CompactionError::SummaryProviderFailed {
                message: err.to_string(),
            }
        })?;

        let raw_text = collect_text(&response.content);
        parse_summary(&raw_text, &self.model_slug)
    }
}

fn collect_text(blocks: &[ResponseContent]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ResponseContent::Text(text) => Some(text.as_str()),
            ResponseContent::ToolUse { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

#[derive(Debug, Deserialize)]
struct RawSummary {
    summary_text: String,
    #[serde(default)]
    preserved_facts: Vec<String>,
    #[serde(default)]
    open_loops: Vec<String>,
}

fn parse_summary(raw: &str, model_slug: &str) -> Result<ContextSummaryPayload, CompactionError> {
    let json_text =
        extract_json_object(raw).ok_or_else(|| CompactionError::SummaryProviderFailed {
            message: "summary response did not contain a JSON object".into(),
        })?;

    let raw: RawSummary =
        serde_json::from_str(json_text).map_err(|err| CompactionError::SummaryProviderFailed {
            message: format!("failed to parse summary JSON: {err}"),
        })?;

    if raw.summary_text.trim().is_empty() {
        return Err(CompactionError::SummaryProviderFailed {
            message: "summary_text was empty".into(),
        });
    }

    Ok(ContextSummaryPayload {
        summary_text: raw.summary_text,
        covered_turn_sequences: Vec::new(),
        preserved_facts: raw.preserved_facts,
        open_loops: raw.open_loops,
        generated_by_model: model_slug.to_string(),
    })
}

/// Extracts the first balanced JSON object substring. Handles responses that
/// wrap the object in Markdown code fences or other chatter.
fn extract_json_object(raw: &str) -> Option<&str> {
    let bytes = raw.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &byte) in bytes[start..].iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&raw[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::Stream;
    use lpa_protocol::{ModelResponse, ResponseMetadata, StopReason, StreamEvent, Usage};
    use pretty_assertions::assert_eq;
    use std::pin::Pin;
    use std::sync::Mutex;

    struct StubProvider {
        response: Mutex<Option<Result<ModelResponse, ProviderError>>>,
        last_request: Mutex<Option<ModelRequest>>,
    }

    impl StubProvider {
        fn with_response(response: Result<ModelResponse, ProviderError>) -> Arc<Self> {
            Arc::new(Self {
                response: Mutex::new(Some(response)),
                last_request: Mutex::new(None),
            })
        }

        fn last_request(&self) -> Option<ModelRequest> {
            self.last_request.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ModelProviderSDK for StubProvider {
        async fn completion(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
            *self.last_request.lock().unwrap() = Some(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("stub completion called more than once")
        }

        async fn completion_stream(
            &self,
            _request: ModelRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
            ProviderError,
        > {
            unimplemented!("streaming not exercised by compactor tests")
        }

        fn name(&self) -> &str {
            "stub"
        }
    }

    fn text_response(text: &str) -> ModelResponse {
        ModelResponse {
            id: "resp-1".into(),
            content: vec![ResponseContent::Text(text.to_string())],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
            metadata: ResponseMetadata::default(),
        }
    }

    fn history(messages: &[&str]) -> PromptAssemblyInput {
        PromptAssemblyInput {
            base_instructions: String::new(),
            tool_definitions: Vec::new(),
            safety_constraints: Vec::new(),
            history_items: messages.iter().map(|m| (*m).to_string()).collect(),
            current_input: Vec::new(),
        }
    }

    #[tokio::test]
    async fn compact_rejects_empty_history() {
        let provider = StubProvider::with_response(Ok(text_response("{}")));
        let compactor = LlmContextCompactor::new(provider.clone(), "summary-model");
        let result = compactor
            .compact(history(&[]), TokenBudget::default())
            .await;
        assert!(matches!(
            result,
            Err(CompactionError::CompactionNotPossible { .. })
        ));
        assert!(provider.last_request().is_none());
    }

    #[tokio::test]
    async fn compact_parses_well_formed_json() {
        let body = r#"{"summary_text":"did work","preserved_facts":["file a.rs"],"open_loops":["finish b.rs"]}"#;
        let provider = StubProvider::with_response(Ok(text_response(body)));
        let compactor = LlmContextCompactor::new(provider.clone(), "summary-model");

        let payload = compactor
            .compact(
                history(&["[user] hi\n", "[assistant] ok\n"]),
                TokenBudget::default(),
            )
            .await
            .expect("summary parses");

        assert_eq!(
            payload,
            ContextSummaryPayload {
                summary_text: "did work".into(),
                covered_turn_sequences: Vec::new(),
                preserved_facts: vec!["file a.rs".into()],
                open_loops: vec!["finish b.rs".into()],
                generated_by_model: "summary-model".into(),
            },
        );

        let request = provider.last_request().expect("request was sent");
        assert_eq!(request.model, "summary-model");
        assert!(request.system.unwrap().contains("\"summary_text\""));
    }

    #[tokio::test]
    async fn compact_strips_markdown_fences_around_json() {
        let body =
            "```json\n{\"summary_text\":\"ok\",\"preserved_facts\":[],\"open_loops\":[]}\n```";
        let provider = StubProvider::with_response(Ok(text_response(body)));
        let compactor = LlmContextCompactor::new(provider, "m");
        let payload = compactor
            .compact(history(&["[user] hi\n"]), TokenBudget::default())
            .await
            .expect("summary parses");
        assert_eq!(payload.summary_text, "ok");
    }

    #[tokio::test]
    async fn compact_surfaces_provider_errors() {
        let provider = StubProvider::with_response(Err(ProviderError::ServerError {
            message: "upstream 503".to_string(),
            status_code: Some(503),
        }));
        let compactor = LlmContextCompactor::new(provider, "m");
        let result = compactor
            .compact(history(&["[user] hi\n"]), TokenBudget::default())
            .await;
        match result {
            Err(CompactionError::SummaryProviderFailed { message }) => {
                assert!(message.contains("upstream 503"));
            }
            other => panic!("expected SummaryProviderFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_fails_when_response_has_no_json() {
        let provider = StubProvider::with_response(Ok(text_response("no json here")));
        let compactor = LlmContextCompactor::new(provider, "m");
        let result = compactor
            .compact(history(&["[user] hi\n"]), TokenBudget::default())
            .await;
        match result {
            Err(CompactionError::SummaryProviderFailed { message }) => {
                assert!(message.contains("JSON object"));
            }
            other => panic!("expected SummaryProviderFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn compact_fails_when_summary_text_empty() {
        let body = r#"{"summary_text":"   ","preserved_facts":[],"open_loops":[]}"#;
        let provider = StubProvider::with_response(Ok(text_response(body)));
        let compactor = LlmContextCompactor::new(provider, "m");
        let result = compactor
            .compact(history(&["[user] hi\n"]), TokenBudget::default())
            .await;
        match result {
            Err(CompactionError::SummaryProviderFailed { message }) => {
                assert!(message.contains("empty"));
            }
            other => panic!("expected SummaryProviderFailed, got {other:?}"),
        }
    }

    #[test]
    fn extract_json_handles_nested_braces_and_strings() {
        let raw = "chatter {\"summary_text\":\"a {b} c\",\"nested\":{\"k\":\"v\"}} trailing";
        let extracted = extract_json_object(raw).expect("extracts balanced object");
        assert_eq!(
            extracted,
            "{\"summary_text\":\"a {b} c\",\"nested\":{\"k\":\"v\"}}",
        );
    }

    #[test]
    fn extract_json_returns_none_when_unbalanced() {
        assert_eq!(extract_json_object("{\"summary_text\":\"oops"), None);
    }

    #[test]
    fn with_max_output_tokens_clamps_lower_bound() {
        let provider = StubProvider::with_response(Ok(text_response("{}")));
        let compactor = LlmContextCompactor::new(provider, "m").with_max_output_tokens(16);
        assert_eq!(compactor.max_output_tokens, 128);
    }
}

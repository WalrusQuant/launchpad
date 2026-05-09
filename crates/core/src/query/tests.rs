use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures::Stream;
use lpa_protocol::{
    ModelRequest, ModelResponse, ResponseContent, ResponseExtra, ResponseMetadata, StopReason,
    StreamEvent, Usage,
};
use lpa_provider::ProviderError;
use lpa_safety::legacy_permissions::PermissionMode;
use lpa_tools::{Tool, ToolOrchestrator, ToolOutput, ToolRegistry};
use pretty_assertions::assert_eq;
use serde_json::json;

use super::{QueryEvent, query, test_model_connection};
use crate::{
    ContentBlock, Message, Model, ProviderFamily, ReasoningEffort, Role, SessionConfig,
    SessionState, ThinkingCapability, ThinkingImplementation, ThinkingVariant,
    ThinkingVariantConfig, TruncationMode, TruncationPolicyConfig, TurnConfig,
};

struct SingleToolUseProvider {
    requests: AtomicUsize,
}

#[async_trait]
impl lpa_provider::ModelProviderSDK for SingleToolUseProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        unreachable!("tests stream responses only")
    }

    async fn completion_stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>, ProviderError>
    {
        let request_number = self.requests.fetch_add(1, Ordering::SeqCst);

        let events = if request_number == 0 {
            vec![
                Ok(StreamEvent::ToolCallStart {
                    index: 0,
                    id: "tool-1".into(),
                    name: "mutating_tool".into(),
                    input: json!({}),
                }),
                Ok(StreamEvent::ToolCallInputDelta {
                    index: 0,
                    partial_json: r#"{"value":1}"#.into(),
                }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-1".into(),
                        content: vec![ResponseContent::ToolUse {
                            id: "tool-1".into(),
                            name: "mutating_tool".into(),
                            input: json!({ "value": 1 }),
                        }],
                        stop_reason: Some(StopReason::ToolUse),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                }),
            ]
        } else {
            vec![
                Ok(StreamEvent::TextDelta {
                    index: 0,
                    text: "done".into(),
                }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-2".into(),
                        content: vec![ResponseContent::Text("done".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: Default::default(),
                    },
                }),
            ]
        };

        Ok(Box::pin(futures::stream::iter(events)))
    }

    fn name(&self) -> &str {
        "test-provider"
    }
}

struct MutatingTool;

struct CapturingProvider {
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

#[async_trait]
impl lpa_provider::ModelProviderSDK for CapturingProvider {
    async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        unreachable!("tests stream responses only")
    }

    async fn completion_stream(
        &self,
        request: ModelRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>, ProviderError>
    {
        self.requests.lock().expect("lock requests").push(request);
        Ok(Box::pin(futures::stream::iter(vec![Ok(
            StreamEvent::MessageDone {
                response: ModelResponse {
                    id: "resp".into(),
                    content: vec![ResponseContent::Text("done".into())],
                    stop_reason: Some(StopReason::EndTurn),
                    usage: Usage::default(),
                    metadata: Default::default(),
                },
            },
        )])))
    }

    fn name(&self) -> &str {
        "capturing-provider"
    }
}

#[async_trait]
impl Tool for MutatingTool {
    fn name(&self) -> &str {
        "mutating_tool"
    }

    fn description(&self) -> &str {
        "A test-only mutating tool."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "value": { "type": "integer" }
            },
            "required": ["value"]
        })
    }

    async fn execute(
        &self,
        _ctx: &lpa_tools::ToolContext,
        _input: serde_json::Value,
    ) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::success("ok"))
    }
}

#[tokio::test]
async fn query_uses_session_permission_mode_for_mutating_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let registry = Arc::new(registry);
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));

    let mut session = SessionState::new(
        SessionConfig {
            permission_mode: PermissionMode::Deny,
            ..Default::default()
        },
        std::env::temp_dir(),
    );
    session.push_message(Message::user("run the tool"));

    query(
        &mut session,
        &TurnConfig {
            model: Model::default(),
            thinking_selection: None,
        },
        Arc::new(SingleToolUseProvider {
            requests: AtomicUsize::new(0),
        }),
        registry,
        &orchestrator,
        None,
    )
    .await
    .expect("query should complete and append a tool_result");

    let tool_result_message = session
        .messages
        .iter()
        .find(|message| {
            message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        })
        .expect("tool_result message should be appended");
    let ContentBlock::ToolResult {
        tool_use_id,
        content,
        is_error,
    } = &tool_result_message.content[0]
    else {
        panic!("expected tool_result content block");
    };

    assert_eq!(tool_use_id, "tool-1");
    assert!(
        *is_error,
        "denied permission should surface as a tool error"
    );
    assert!(
        content.contains("permission denied"),
        "expected tool_result to mention permission denial, got: {content}"
    );
}

#[tokio::test]
async fn query_resolves_model_variant_thinking_before_building_request() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Arc::new(CapturingProvider {
        requests: Arc::clone(&requests),
    });
    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let model = Model {
        slug: "kimi-k2.5".into(),
        display_name: "Kimi K2.5".into(),
        provider: ProviderFamily::openai(),
        description: None,
        thinking_capability: ThinkingCapability::Toggle,
        default_reasoning_effort: Some(ReasoningEffort::Medium),
        thinking_implementation: Some(ThinkingImplementation::ModelVariant(
            ThinkingVariantConfig {
                variants: vec![
                    ThinkingVariant {
                        selection_value: "disabled".into(),
                        model_slug: "kimi-k2.5".into(),
                        reasoning_effort: None,
                        label: "Off".into(),
                        description: "Use the standard model".into(),
                    },
                    ThinkingVariant {
                        selection_value: "enabled".into(),
                        model_slug: "kimi-k2.5-thinking".into(),
                        reasoning_effort: Some(ReasoningEffort::Medium),
                        label: "On".into(),
                        description: "Use the thinking model".into(),
                    },
                ],
            },
        )),
        base_instructions: String::new(),
        context_window: 200_000,
        effective_context_window_percent: None,
        truncation_policy: TruncationPolicyConfig {
            mode: TruncationMode::Tokens,
            limit: 10_000,
        },
        input_modalities: vec![],
        supports_image_detail_original: false,
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
    };
    let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
    session.push_message(Message::user("hello"));

    query(
        &mut session,
        &TurnConfig {
            model,
            thinking_selection: Some("enabled".into()),
        },
        provider,
        registry,
        &orchestrator,
        None,
    )
    .await
    .expect("query should succeed");

    let captured = requests.lock().expect("lock requests");
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].model, "kimi-k2.5-thinking");
    assert_eq!(captured[0].thinking, None);
}

#[tokio::test]
async fn test_model_connection_sends_minimal_request() {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = CapturingProvider {
        requests: Arc::clone(&requests),
    };
    let model = Model {
        slug: "glm-4.5".into(),
        top_p: Some(0.95),
        ..Model::default()
    };
    let preview = test_model_connection(&provider, &model, "Reply with OK only.")
        .await
        .expect("probe request should succeed");

    let captured = requests.lock().expect("lock requests");
    assert_eq!(preview, "done");
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].system, None);
    assert!(captured[0].tools.is_none());
    assert_eq!(captured[0].messages.len(), 1);
    assert_eq!(captured[0].sampling.top_p, Some(0.95));
}

#[tokio::test]
async fn query_emits_reasoning_without_polluting_assistant_message_content() {
    struct ReasoningProvider;

    #[async_trait]
    impl lpa_provider::ModelProviderSDK for ReasoningProvider {
        async fn completion(&self, _request: ModelRequest) -> Result<ModelResponse, ProviderError> {
            unreachable!("tests stream responses only")
        }

        async fn completion_stream(
            &self,
            _request: ModelRequest,
        ) -> Result<
            Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
            ProviderError,
        > {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::ReasoningStart { index: 0 }),
                Ok(StreamEvent::ReasoningDelta {
                    index: 0,
                    text: "plan".into(),
                }),
                Ok(StreamEvent::TextStart { index: 1 }),
                Ok(StreamEvent::TextDelta {
                    index: 1,
                    text: "final".into(),
                }),
                Ok(StreamEvent::MessageDone {
                    response: ModelResponse {
                        id: "resp-3".into(),
                        content: vec![ResponseContent::Text("final".into())],
                        stop_reason: Some(StopReason::EndTurn),
                        usage: Usage::default(),
                        metadata: ResponseMetadata {
                            extras: vec![ResponseExtra::ReasoningText {
                                text: "plan".into(),
                            }],
                        },
                    },
                }),
            ])))
        }

        fn name(&self) -> &str {
            "reasoning-provider"
        }
    }

    let registry = Arc::new(ToolRegistry::new());
    let orchestrator = ToolOrchestrator::new(Arc::clone(&registry));
    let mut session = SessionState::new(SessionConfig::default(), std::env::temp_dir());
    session.push_message(Message::user("hello"));
    let seen_events = Arc::new(Mutex::new(Vec::new()));
    let callback_events = Arc::clone(&seen_events);
    let callback = Arc::new(move |event: QueryEvent| {
        callback_events.lock().expect("lock callback").push(event);
    });

    query(
        &mut session,
        &TurnConfig {
            model: Model::default(),
            thinking_selection: None,
        },
        Arc::new(ReasoningProvider),
        registry,
        &orchestrator,
        Some(callback),
    )
    .await
    .expect("query should succeed");

    let events = seen_events.lock().expect("lock events");
    assert!(events.iter().any(|event| matches!(
        event,
        QueryEvent::ReasoningDelta(text) if text == "plan"
    )));
    drop(events);

    let assistant_message = session
        .messages
        .iter()
        .find(|message| matches!(message.role, Role::Assistant))
        .expect("assistant message");
    assert_eq!(
        assistant_message,
        &Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "final".into(),
            }],
        }
    );
}

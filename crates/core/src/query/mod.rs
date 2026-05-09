mod compaction;
mod connection_test;
mod prefetch;

pub use connection_test::test_model_connection;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use lpa_protocol::{
    ModelRequest, ResolvedThinkingRequest, ResponseContent, ResponseExtra, SamplingControls,
    StopReason, StreamEvent,
};
use tokio::time::sleep;
use tracing::{debug, info, info_span, warn};

use lpa_provider::{ModelProviderSDK, ProviderError};
use lpa_tools::{ToolCall, ToolContext, ToolOrchestrator, ToolRegistry};

use crate::{
    AgentError, CompactionOutcome, ContentBlock, EligibilitySelector, Message, Role, SessionState,
    SummaryModelSelection, TurnConfig, run_llm_compaction, warn_compaction_failed,
};

use compaction::{compact_session, micro_compact};
use prefetch::{append_prefetched_user_inputs, build_prefetched_user_inputs, build_system_prompt};

/// Controls how summary-model calls are resolved during compaction.
///
/// The query loop owns model resolution but needs a clean way to tell the
/// compaction runner which slug to pass to the summarizer. Today both arms
/// resolve to the turn's model; `UseAxiliaryModel` is reserved for the Phase 3
/// auxiliary-model selection spec and falls back to the turn model until the
/// runtime wires auxiliary model resolution in.
fn resolve_summary_model_slug(
    turn_config: &TurnConfig,
    selection: &SummaryModelSelection,
) -> String {
    match selection {
        SummaryModelSelection::UseTurnModel => turn_config.model.slug.clone(),
        SummaryModelSelection::UseAxiliaryModel => turn_config.model.slug.clone(),
    }
}

/// Events emitted during a query for the caller (CLI/UI) to observe.
#[derive(Debug, Clone)]
pub enum QueryEvent {
    /// Incremental text from the assistant.
    TextDelta(String),
    /// Incremental reasoning text from the assistant.
    ReasoningDelta(String),
    /// Incremental token usage update from the provider stream.
    UsageDelta {
        input_tokens: usize,
        output_tokens: usize,
        cache_creation_input_tokens: Option<usize>,
        cache_read_input_tokens: Option<usize>,
    },
    /// The assistant started a tool call.
    ToolUseStart {
        /// Stable provider-issued tool use identifier.
        id: String,
        /// Tool name selected by the model.
        name: String,
        /// Fully decoded tool input payload, when available.
        input: serde_json::Value,
    },
    /// A tool call completed.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// A turn is complete (model stopped generating).
    TurnComplete { stop_reason: StopReason },
    /// Token usage update.
    Usage {
        input_tokens: usize,
        output_tokens: usize,
        cache_creation_input_tokens: Option<usize>,
        cache_read_input_tokens: Option<usize>,
    },
    /// An approval request has been registered and is awaiting user response.
    ApprovalRequest {
        approval_id: String,
        action_summary: String,
        justification: String,
    },
    /// Emitted after a successful LLM compaction so the server can persist the
    /// snapshot JSON and a `RolloutLine::CompactionSnapshot` journal entry.
    ContextCompacted(CompactionOutcome),
}

/// Callback for streaming query events to the UI layer.
pub type EventCallback = Arc<dyn Fn(QueryEvent) + Send + Sync>;

const MAX_RETRIES: usize = 5;
const INITIAL_RETRY_BACKOFF_MS: u64 = 250;

/// The recursive agent loop — the beating heart of the runtime.
///
/// The implementation refers to Claude Code's `query.ts`. It drives
/// multi-turn conversations by:
///
/// 1. Building the model request from session state
/// 2. Streaming the model response
/// 3. Collecting assistant text and tool_use blocks
/// 4. Executing tool calls via the orchestrator
/// 5. Appending tool_result messages
/// 6. Recursing if the model wants to continue
///
/// The loop terminates when:
/// - The model emits `end_turn` with no tool calls
/// - An unrecoverable error occurs
pub async fn query(
    session: &mut SessionState,
    turn_config: &TurnConfig,
    provider: Arc<dyn ModelProviderSDK>,
    registry: Arc<ToolRegistry>,
    orchestrator: &ToolOrchestrator,
    on_event: Option<EventCallback>,
) -> Result<(), AgentError> {
    let emit = |event: QueryEvent| {
        if let Some(ref cb) = on_event {
            cb(event);
        }
    };

    let prefetched_user_inputs = build_prefetched_user_inputs(&session.cwd);

    let mut retry_count: usize = 0;
    let mut context_compacted = false;

    loop {
        for prompt in session.drain_pending_user_prompts() {
            session.push_message(Message::user(prompt));
        }

        if session.last_input_tokens > 0
            && session
                .config
                .token_budget
                .should_compact(session.last_input_tokens)
        {
            info!("token budget threshold exceeded — compacting session");
            let summary_slug =
                resolve_summary_model_slug(turn_config, &SummaryModelSelection::UseTurnModel);
            match run_llm_compaction(
                session,
                Arc::clone(&provider),
                &summary_slug,
                &EligibilitySelector::default(),
            )
            .await
            {
                Ok(Some(outcome)) => emit(QueryEvent::ContextCompacted(outcome)),
                Ok(None) => {
                    compact_session(session);
                }
                Err(error) => {
                    warn_compaction_failed(&error);
                    compact_session(session);
                }
            }
        }

        session.turn_count += 1;
        let turn_span = info_span!(
            "turn",
            turn = session.turn_count,
            session_id = %session.id,
            model = %turn_config.model.slug,
            cwd = %session.cwd.display()
        );
        let _turn_guard = turn_span.enter();
        info!("starting turn");

        let system = build_system_prompt(&turn_config.model.base_instructions);

        let ResolvedThinkingRequest {
            request_model,
            request_thinking,
            extra_body,
            effective_reasoning_effort: _,
        } = turn_config
            .model
            .resolve_thinking_selection(turn_config.thinking_selection.as_deref());

        let mut messages = session.to_prompt_messages();
        append_prefetched_user_inputs(&mut messages, &prefetched_user_inputs);

        let request = ModelRequest {
            model: request_model,
            system: if system.is_empty() {
                None
            } else {
                Some(system)
            },
            messages,
            max_tokens: turn_config
                .model
                .max_tokens
                .map_or(session.config.token_budget.max_output_tokens, |value| {
                    value as usize
                }),
            tools: Some(registry.tool_definitions()),
            sampling: SamplingControls {
                temperature: turn_config.model.temperature,
                top_p: turn_config.model.top_p,
                top_k: turn_config.model.top_k.map(|value| value as u32),
            },
            thinking: request_thinking,
            extra_body,
        };
        debug!(
            messages = request.messages.len(),
            tools = request.tools.as_ref().map_or(0, Vec::len),
            max_tokens = request.max_tokens,
            has_system = request.system.is_some(),
            "built model request"
        );

        let stream_result = provider.completion_stream(request).await;

        let mut stream = match stream_result {
            Ok(s) => {
                retry_count = 0;
                context_compacted = false;
                s
            }
            Err(e) => {
                warn!(
                    provider = provider.name(),
                    model = %turn_config.model.slug,
                    turn = session.turn_count,
                    error = ?e,
                    "failed to create provider stream"
                );
                match &e {
                    ProviderError::ContextTooLong { .. } => {
                        if context_compacted {
                            return Err(AgentError::ContextTooLong);
                        }
                        warn!("context_too_long — compacting and retrying");
                        let summary_slug = resolve_summary_model_slug(
                            turn_config,
                            &SummaryModelSelection::UseTurnModel,
                        );
                        match run_llm_compaction(
                            session,
                            Arc::clone(&provider),
                            &summary_slug,
                            &EligibilitySelector::default(),
                        )
                        .await
                        {
                            Ok(Some(outcome)) => emit(QueryEvent::ContextCompacted(outcome)),
                            Ok(None) => {
                                compact_session(session);
                            }
                            Err(error) => {
                                warn_compaction_failed(&error);
                                compact_session(session);
                            }
                        }
                        context_compacted = true;
                        session.turn_count -= 1;
                        continue;
                    }
                    ProviderError::RateLimited { .. } | ProviderError::ServerError { .. } => {
                        if retry_count < MAX_RETRIES {
                            retry_count += 1;
                            let backoff = retry_backoff_duration(retry_count);
                            warn!(
                                attempt = retry_count,
                                backoff_ms = backoff.as_millis(),
                                "transient error — retrying with exponential backoff"
                            );
                            sleep(backoff).await;
                            session.turn_count -= 1;
                            continue;
                        }
                        return Err(AgentError::Provider(e));
                    }
                    _ => {
                        return Err(AgentError::Provider(e));
                    }
                }
            }
        };

        let mut assistant_text = String::new();
        let mut reasoning_text = String::new();
        let mut tool_uses: Vec<(String, String, serde_json::Value, String, bool)> = Vec::new();
        let mut final_response = None;
        let mut stop_reason = None;

        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::TextStart { .. }) => {}
                Ok(StreamEvent::TextDelta { text, .. }) => {
                    assistant_text.push_str(&text);
                    emit(QueryEvent::TextDelta(text));
                }
                Ok(StreamEvent::ReasoningStart { .. }) => {}
                Ok(StreamEvent::ReasoningDelta { text, .. }) => {
                    reasoning_text.push_str(&text);
                    emit(QueryEvent::ReasoningDelta(text));
                }
                Ok(StreamEvent::ToolCallStart {
                    id, name, input, ..
                }) => {
                    tool_uses.push((id, name, input, String::new(), false));
                }
                Ok(StreamEvent::ToolCallInputDelta { partial_json, .. }) => {
                    if let Some(last) = tool_uses.last_mut() {
                        last.3.push_str(&partial_json);
                        last.4 = true;
                    }
                }
                Ok(StreamEvent::MessageDone { response }) => {
                    stop_reason = response.stop_reason.clone();
                    final_response = Some(response.clone());

                    session.total_input_tokens += response.usage.input_tokens;
                    session.total_output_tokens += response.usage.output_tokens;
                    session.total_cache_creation_tokens +=
                        response.usage.cache_creation_input_tokens.unwrap_or(0);
                    session.total_cache_read_tokens +=
                        response.usage.cache_read_input_tokens.unwrap_or(0);
                    session.last_input_tokens = response.usage.input_tokens;

                    emit(QueryEvent::Usage {
                        input_tokens: response.usage.input_tokens,
                        output_tokens: response.usage.output_tokens,
                        cache_creation_input_tokens: response.usage.cache_creation_input_tokens,
                        cache_read_input_tokens: response.usage.cache_read_input_tokens,
                    });
                }
                Ok(StreamEvent::UsageDelta(usage)) => {
                    emit(QueryEvent::UsageDelta {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                    });
                }
                Err(e) => {
                    warn!(
                        provider = provider.name(),
                        model = %turn_config.model.slug,
                        turn = session.turn_count,
                        error = ?e,
                        "stream error"
                    );
                    return Err(AgentError::Provider(e));
                }
            }
        }

        if let Some(response) = &final_response {
            if assistant_text.is_empty() {
                assistant_text = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ResponseContent::Text(text) => Some(text.as_str()),
                        ResponseContent::ToolUse { .. } => None,
                    })
                    .collect();
            }
            if tool_uses.is_empty() {
                tool_uses = response
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ResponseContent::ToolUse { id, name, input } => Some((
                            id.clone(),
                            name.clone(),
                            input.clone(),
                            String::new(),
                            false,
                        )),
                        ResponseContent::Text(_) => None,
                    })
                    .collect();
            }
            if reasoning_text.is_empty() {
                let final_reasoning = response
                    .metadata
                    .extras
                    .iter()
                    .filter_map(|extra| match extra {
                        ResponseExtra::ReasoningText { text } => Some(text.as_str()),
                        ResponseExtra::ProviderSpecific { .. } => None,
                    })
                    .collect::<String>();
                if !final_reasoning.is_empty() {
                    emit(QueryEvent::ReasoningDelta(final_reasoning.clone()));
                    reasoning_text = final_reasoning;
                }
            }
        }

        let mut assistant_content: Vec<ContentBlock> = Vec::new();

        if !assistant_text.is_empty() {
            assistant_content.push(ContentBlock::Text {
                text: assistant_text,
            });
        }

        let tool_calls: Vec<ToolCall> = tool_uses
            .into_iter()
            .map(|(id, name, initial_input, json_str, saw_delta)| {
                let input = if saw_delta {
                    serde_json::from_str(&json_str).unwrap_or(initial_input)
                } else {
                    initial_input
                };
                emit(QueryEvent::ToolUseStart {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                assistant_content.push(ContentBlock::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                });
                ToolCall { id, name, input }
            })
            .collect();

        session.push_message(Message {
            role: Role::Assistant,
            content: assistant_content,
        });

        if tool_calls.is_empty() {
            if stop_reason == Some(StopReason::MaxTokens) {
                debug!("max_tokens reached — injecting continuation prompt");
                session.push_message(Message::user("Please continue from where you left off."));
                continue;
            }

            if let Some(sr) = stop_reason {
                emit(QueryEvent::TurnComplete { stop_reason: sr });
            }
            debug!("no tool calls, ending query loop");
            return Ok(());
        }

        let tool_ctx = ToolContext {
            cwd: session.cwd.clone(),
            permissions: Arc::new(lpa_safety::legacy_permissions::RuleBasedPolicy::new(
                session.config.permission_mode,
            )),
            session_id: session.id.clone(),
        };

        let results = orchestrator.execute_batch(&tool_calls, &tool_ctx).await;

        let result_content: Vec<ContentBlock> = results
            .into_iter()
            .map(|r| {
                let compacted_content = micro_compact(r.output.content.clone());
                emit(QueryEvent::ToolResult {
                    tool_use_id: r.tool_use_id.clone(),
                    content: compacted_content.clone(),
                    is_error: r.output.is_error,
                });
                ContentBlock::ToolResult {
                    tool_use_id: r.tool_use_id,
                    content: compacted_content,
                    is_error: r.output.is_error,
                }
            })
            .collect();

        session.push_message(Message {
            role: Role::User,
            content: result_content,
        });
    }
}

fn retry_backoff_duration(attempt: usize) -> Duration {
    let exponent = attempt.saturating_sub(1).min(10) as u32;
    let multiplier = 2u64.pow(exponent);
    Duration::from_millis(INITIAL_RETRY_BACKOFF_MS.saturating_mul(multiplier))
}

#[cfg(test)]
mod tests;

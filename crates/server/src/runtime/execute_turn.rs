use std::sync::Arc;

use chrono::Utc;
use lpa_tools::ToolOrchestrator;

use lpa_core::{
    ApprovalRequestItem, CompactionSnapshot, ItemId, Message, QueryEvent, SessionId,
    SnapshotBackendKind, SummaryModelSelection, TextItem, ToolCallItem, ToolResultItem, TurnConfig,
    TurnItem, TurnStatus, TurnUsage, query,
};

use crate::{
    EventContext, ItemDeltaKind, ItemDeltaPayload, ItemKind, ServerEvent, SessionRuntimeStatus,
    SessionStatusChangedPayload, TurnEventPayload, TurnSummary, TurnUsageUpdatedPayload,
    approval_channel::ServerApprovalChannel, persistence::build_turn_record,
};

use super::ServerRuntime;

impl ServerRuntime {
    pub(super) async fn execute_turn(
        self: Arc<Self>,
        session_id: SessionId,
        turn: TurnSummary,
        turn_config: TurnConfig,
        display_input: String,
        input: String,
    ) {
        self.emit_text_item(
            session_id,
            turn.turn_id,
            ItemKind::UserMessage,
            TurnItem::UserMessage(TextItem {
                text: display_input.clone(),
            }),
            "You",
            display_input.clone(),
        )
        .await;

        let Some(session_arc) = self.sessions.lock().await.get(&session_id).cloned() else {
            return;
        };
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<QueryEvent>();
        let runtime = Arc::clone(&self);
        let turn_for_events = turn.clone();
        let event_session_arc = Arc::clone(&session_arc);
        let event_task = tokio::spawn(async move {
            let mut assistant_item_id = None;
            let mut assistant_item_seq = None;
            let mut assistant_text = String::new();
            let mut reasoning_item_id = None;
            let mut reasoning_item_seq = None;
            let mut reasoning_text = String::new();
            let mut latest_usage: Option<TurnUsage> = None;
            let mut usage_base: Option<(usize, usize)> = None;
            while let Some(event) = event_rx.recv().await {
                match event {
                    QueryEvent::TextDelta(text) => {
                        let (item_id, item_seq) = match (assistant_item_id, assistant_item_seq) {
                            (Some(item_id), Some(item_seq)) => (item_id, item_seq),
                            (None, None) => {
                                let (item_id, item_seq) = runtime
                                    .start_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        ItemKind::AgentMessage,
                                        serde_json::json!({ "title": "Assistant", "text": "" }),
                                    )
                                    .await;
                                assistant_item_id = Some(item_id);
                                assistant_item_seq = Some(item_seq);
                                (item_id, item_seq)
                            }
                            _ => continue,
                        };
                        assistant_text.push_str(&text);
                        runtime
                            .broadcast_event(ServerEvent::ItemDelta {
                                delta_kind: ItemDeltaKind::AgentMessageDelta,
                                payload: ItemDeltaPayload {
                                    context: EventContext {
                                        session_id,
                                        turn_id: Some(turn_for_events.turn_id),
                                        item_id: Some(item_id),
                                        seq: 0,
                                    },
                                    delta: text,
                                    stream_index: None,
                                    channel: None,
                                },
                            })
                            .await;
                        let _ = item_seq;
                    }
                    QueryEvent::ReasoningDelta(text) => {
                        let (item_id, item_seq) = match (reasoning_item_id, reasoning_item_seq) {
                            (Some(item_id), Some(item_seq)) => (item_id, item_seq),
                            (None, None) => {
                                let (item_id, item_seq) = runtime
                                    .start_item(
                                        session_id,
                                        turn_for_events.turn_id,
                                        ItemKind::Reasoning,
                                        serde_json::json!({ "title": "Reasoning", "text": "" }),
                                    )
                                    .await;
                                reasoning_item_id = Some(item_id);
                                reasoning_item_seq = Some(item_seq);
                                (item_id, item_seq)
                            }
                            _ => continue,
                        };
                        reasoning_text.push_str(&text);
                        runtime
                            .broadcast_event(ServerEvent::ItemDelta {
                                delta_kind: ItemDeltaKind::ReasoningTextDelta,
                                payload: ItemDeltaPayload {
                                    context: EventContext {
                                        session_id,
                                        turn_id: Some(turn_for_events.turn_id),
                                        item_id: Some(item_id),
                                        seq: 0,
                                    },
                                    delta: text,
                                    stream_index: None,
                                    channel: None,
                                },
                            })
                            .await;
                        let _ = item_seq;
                    }
                    QueryEvent::ToolUseStart { id, name, input } => {
                        if let (Some(item_id), Some(item_seq)) =
                            (assistant_item_id.take(), assistant_item_seq.take())
                        {
                            runtime
                                .complete_item(
                                    session_id,
                                    turn_for_events.turn_id,
                                    item_id,
                                    item_seq,
                                    ItemKind::AgentMessage,
                                    TurnItem::AgentMessage(TextItem {
                                        text: assistant_text.clone(),
                                    }),
                                    serde_json::json!({
                                        "title": "Assistant",
                                        "text": assistant_text,
                                    }),
                                )
                                .await;
                            assistant_text.clear();
                        }
                        if let (Some(item_id), Some(item_seq)) =
                            (reasoning_item_id.take(), reasoning_item_seq.take())
                        {
                            runtime
                                .complete_item(
                                    session_id,
                                    turn_for_events.turn_id,
                                    item_id,
                                    item_seq,
                                    ItemKind::Reasoning,
                                    TurnItem::Reasoning(TextItem {
                                        text: reasoning_text.clone(),
                                    }),
                                    serde_json::json!({
                                        "title": "Reasoning",
                                        "text": reasoning_text,
                                    }),
                                )
                                .await;
                            reasoning_text.clear();
                        }
                        runtime
                            .emit_turn_item(
                                session_id,
                                turn_for_events.turn_id,
                                ItemKind::ToolCall,
                                TurnItem::ToolCall(ToolCallItem {
                                    tool_call_id: id.clone(),
                                    tool_name: name.clone(),
                                    input: input.clone(),
                                }),
                                serde_json::json!({
                                    "tool_use_id": id,
                                    "tool_name": name,
                                    "input": input,
                                }),
                            )
                            .await;
                    }
                    QueryEvent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => {
                        runtime
                            .emit_turn_item(
                                session_id,
                                turn_for_events.turn_id,
                                ItemKind::ToolResult,
                                TurnItem::ToolResult(ToolResultItem {
                                    tool_call_id: tool_use_id.clone(),
                                    output: serde_json::Value::String(content.clone()),
                                    is_error,
                                }),
                                serde_json::json!({
                                    "tool_use_id": tool_use_id,
                                    "content": content,
                                    "is_error": is_error,
                                }),
                            )
                            .await;
                    }
                    QueryEvent::UsageDelta {
                        input_tokens,
                        output_tokens,
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
                    }
                    | QueryEvent::Usage {
                        input_tokens,
                        output_tokens,
                        cache_creation_input_tokens,
                        cache_read_input_tokens,
                    } => {
                        let usage = TurnUsage {
                            input_tokens: input_tokens as u32,
                            output_tokens: output_tokens as u32,
                            cache_creation_input_tokens: cache_creation_input_tokens
                                .map(|value| value as u32),
                            cache_read_input_tokens: cache_read_input_tokens
                                .map(|value| value as u32),
                        };
                        latest_usage = Some(usage.clone());

                        let base = if let Some(base) = usage_base {
                            base
                        } else {
                            let base = {
                                let session = event_session_arc.lock().await;
                                (
                                    session.summary.total_input_tokens,
                                    session.summary.total_output_tokens,
                                )
                            };
                            usage_base = Some(base);
                            base
                        };
                        {
                            let mut session = event_session_arc.lock().await;
                            session.summary.total_input_tokens =
                                base.0 + usage.input_tokens as usize;
                            session.summary.total_output_tokens =
                                base.1 + usage.output_tokens as usize;
                        }
                        let _ = runtime
                            .broadcast_event(ServerEvent::TurnUsageUpdated(
                                TurnUsageUpdatedPayload {
                                    session_id,
                                    turn_id: turn_for_events.turn_id,
                                    usage,
                                    total_input_tokens: base.0 + input_tokens,
                                    total_output_tokens: base.1 + output_tokens,
                                },
                            ))
                            .await;
                    }
                    QueryEvent::TurnComplete { .. } => {}
                    QueryEvent::ApprovalRequest {
                        approval_id,
                        action_summary,
                        justification,
                    } => {
                        let approval_id_smol: smol_str::SmolStr = approval_id.clone().into();
                        let request_context = crate::PendingServerRequestContext {
                            request_id: approval_id_smol.clone(),
                            request_kind: crate::ServerRequestKind::ItemPermissionsRequestApproval,
                            session_id,
                            turn_id: Some(turn_for_events.turn_id),
                            item_id: None,
                        };
                        let payload = crate::ApprovalRequestPayload {
                            request: request_context,
                            approval_id: approval_id_smol,
                            action_summary: action_summary.clone(),
                            justification: justification.clone(),
                        };
                        runtime
                            .emit_turn_item(
                                session_id,
                                turn_for_events.turn_id,
                                ItemKind::ApprovalRequest,
                                TurnItem::ApprovalRequest(ApprovalRequestItem {
                                    approval_id,
                                    action_summary,
                                    justification,
                                }),
                                serde_json::to_value(&payload).unwrap_or_default(),
                            )
                            .await;
                        runtime
                            .broadcast_event(ServerEvent::ApprovalRequested(payload))
                            .await;
                    }
                    QueryEvent::ContextCompacted(outcome) => {
                        let summary_text = outcome.summary.summary_text.clone();
                        let payload_value = serde_json::json!({
                            "title": "Context compacted",
                            "summary_text": summary_text,
                            "replaced_prefix_len": outcome.replaced_prefix_len,
                            "preserved_facts": outcome.summary.preserved_facts,
                            "open_loops": outcome.summary.open_loops,
                            "replaced_prior_summary": outcome.replaced_prior_summary,
                        });
                        let (item_id, item_seq) = runtime
                            .start_item(
                                session_id,
                                turn_for_events.turn_id,
                                ItemKind::ContextCompaction,
                                payload_value.clone(),
                            )
                            .await;
                        runtime
                            .complete_item(
                                session_id,
                                turn_for_events.turn_id,
                                item_id,
                                item_seq,
                                ItemKind::ContextCompaction,
                                TurnItem::ContextCompaction(TextItem { text: summary_text }),
                                payload_value,
                            )
                            .await;

                        let snapshot = CompactionSnapshot {
                            session_id,
                            turn_id: turn_for_events.turn_id,
                            replaced_from_item_id: ItemId::new(),
                            replaced_to_item_id: ItemId::new(),
                            summary_item_id: item_id,
                            model_slug: outcome.summary.generated_by_model.clone(),
                            summary_model_selection: SummaryModelSelection::UseTurnModel,
                            prompt_segment_order: Vec::new(),
                            workspace_root: None,
                            repo_root: None,
                            snapshot_backend: SnapshotBackendKind::JsonOnly,
                        };
                        let record = {
                            let session = event_session_arc.lock().await;
                            session.record.clone()
                        };
                        if let Some(record) = record
                            && let Err(error) = runtime
                                .rollout_store
                                .append_compaction_snapshot(&record, &snapshot)
                        {
                            tracing::warn!(
                                session_id = %session_id,
                                error = %error,
                                "failed to persist compaction snapshot",
                            );
                        }
                    }
                }
            }
            if let (Some(item_id), Some(item_seq)) = (assistant_item_id, assistant_item_seq) {
                runtime
                    .complete_item(
                        session_id,
                        turn_for_events.turn_id,
                        item_id,
                        item_seq,
                        ItemKind::AgentMessage,
                        TurnItem::AgentMessage(TextItem {
                            text: assistant_text.clone(),
                        }),
                        serde_json::json!({ "title": "Assistant", "text": assistant_text }),
                    )
                    .await;
            }
            if let (Some(item_id), Some(item_seq)) = (reasoning_item_id, reasoning_item_seq) {
                runtime
                    .complete_item(
                        session_id,
                        turn_for_events.turn_id,
                        item_id,
                        item_seq,
                        ItemKind::Reasoning,
                        TurnItem::Reasoning(TextItem {
                            text: reasoning_text.clone(),
                        }),
                        serde_json::json!({ "title": "Reasoning", "text": reasoning_text }),
                    )
                    .await;
            }
            latest_usage
        });

        let (
            result,
            first_assistant_reply,
            session_total_input_tokens,
            session_total_output_tokens,
        ) = {
            let (core_session, session_approval_cache) = {
                let session = session_arc.lock().await;
                (
                    Arc::clone(&session.core_session),
                    Arc::clone(&session.approval_cache),
                )
            };
            let mut core_session = core_session.lock().await;
            core_session.push_message(Message::user(input.clone()));
            let event_callback_tx = event_tx.clone();
            let callback = std::sync::Arc::new(move |event: QueryEvent| {
                let _ = event_callback_tx.send(event);
            });
            let registry = Arc::clone(&self.deps.registry);
            let approval_channel = ServerApprovalChannel::new(
                Arc::clone(&self.approval_manager),
                session_id,
                turn.turn_id,
                event_tx.clone(),
            );
            let orchestrator = ToolOrchestrator::new(Arc::clone(&registry))
                .with_approval_channel(std::sync::Arc::new(approval_channel))
                .with_approval_cache(session_approval_cache);
            let result = query(
                &mut core_session,
                &turn_config,
                Arc::clone(&self.deps.provider),
                registry,
                &orchestrator,
                Some(callback),
            )
            .await;
            let first_assistant_reply = core_session.messages.iter().find_map(|message| {
                if !matches!(message.role, lpa_core::Role::Assistant) {
                    return None;
                }
                let text = message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        lpa_core::ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>();
                (!text.trim().is_empty()).then_some(text)
            });
            (
                result,
                first_assistant_reply,
                core_session.total_input_tokens,
                core_session.total_output_tokens,
            )
        };
        drop(event_tx);
        let latest_usage = event_task.await.ok().flatten();
        self.active_tasks.lock().await.remove(&session_id);

        let final_turn = {
            let mut session = session_arc.lock().await;
            let mut final_turn = turn.clone();
            final_turn.completed_at = Some(Utc::now());
            final_turn.status = if result.is_ok() {
                TurnStatus::Completed
            } else {
                TurnStatus::Failed
            };
            final_turn.usage = latest_usage.clone();
            session.latest_turn = Some(final_turn.clone());
            session.active_turn = None;
            session.active_task = None;
            session.summary.status = SessionRuntimeStatus::Idle;
            session.summary.updated_at = Utc::now();
            session.summary.total_input_tokens = session_total_input_tokens;
            session.summary.total_output_tokens = session_total_output_tokens;
            final_turn
        };
        if let Some(record) = session_arc.lock().await.record.clone()
            && let Err(error) = self
                .rollout_store
                .append_turn(&record, build_turn_record(&final_turn))
        {
            tracing::warn!(session_id = %session_id, error = %error, "failed to persist terminal turn line");
        }
        if final_turn.status == TurnStatus::Completed
            && let Some(first_assistant_reply) = first_assistant_reply
        {
            let runtime = Arc::clone(&self);
            let input_for_title = display_input.clone();
            tokio::spawn(async move {
                runtime
                    .maybe_generate_final_title(
                        session_id,
                        &input_for_title,
                        &first_assistant_reply,
                    )
                    .await;
            });
        }

        if let Err(error) = result {
            tracing::warn!(
                session_id = %session_id,
                turn_id = %final_turn.turn_id,
                status = ?final_turn.status,
                error = %error,
                "turn execution failed"
            );
            self.emit_text_item(
                session_id,
                final_turn.turn_id,
                ItemKind::AgentMessage,
                TurnItem::AgentMessage(TextItem {
                    text: error.to_string(),
                }),
                "Error",
                error.to_string(),
            )
            .await;
            self.broadcast_event(ServerEvent::TurnFailed(TurnEventPayload {
                session_id,
                turn: final_turn.clone(),
            }))
            .await;
        } else {
            tracing::info!(
                session_id = %session_id,
                turn_id = %final_turn.turn_id,
                status = ?final_turn.status,
                total_input_tokens = final_turn.usage.as_ref().map(|usage| usage.input_tokens),
                total_output_tokens = final_turn.usage.as_ref().map(|usage| usage.output_tokens),
                "turn execution completed"
            );
        }
        self.broadcast_event(ServerEvent::TurnCompleted(TurnEventPayload {
            session_id,
            turn: final_turn,
        }))
        .await;
        self.broadcast_event(ServerEvent::SessionStatusChanged(
            SessionStatusChangedPayload {
                session_id,
                status: SessionRuntimeStatus::Idle,
            },
        ))
        .await;
    }
}

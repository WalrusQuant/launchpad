use std::sync::Arc;

use chrono::Utc;

use lpa_core::{TextItem, TurnId, TurnItem, TurnStatus};

use crate::{
    ItemKind, ProtocolErrorCode, ServerEvent, SessionRuntimeStatus, SessionStatusChangedPayload,
    SuccessResponse, TurnEventPayload, TurnInterruptParams, TurnInterruptResult, TurnStartParams,
    TurnStartResult, TurnSteerParams, TurnSteerResult, TurnSummary, persistence::build_turn_record,
};

use super::ServerRuntime;

fn render_input_items(input: &[crate::InputItem]) -> Option<String> {
    let parts = input
        .iter()
        .map(|item| match item {
            crate::InputItem::Text { text } => text.trim().to_string(),
            crate::InputItem::Skill { id } => format!("[skill:{id}]"),
            crate::InputItem::LocalImage { path } => format!("[image:{}]", path.display()),
            crate::InputItem::Mention { path, name } => {
                format!("[mention:{}]", name.as_deref().unwrap_or(path.as_str()))
            }
        })
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

impl ServerRuntime {
    pub(super) async fn handle_turn_start(
        self: &Arc<Self>,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: TurnStartParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid turn/start params: {error}"),
                );
            }
        };
        if params.input.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        }
        let Some(display_input) = render_input_items(&params.input) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let workspace_root = {
            let session = session_arc.lock().await;
            params
                .cwd
                .clone()
                .unwrap_or_else(|| session.summary.cwd.clone())
        };
        let Some(input_text) = (match self
            .deps
            .resolve_input_items(&params.input, Some(workspace_root.as_path()))
        {
            Ok(input_text) => input_text,
            Err(error) => {
                let code = match error {
                    lpa_core::SkillError::SkillNotFound { .. }
                    | lpa_core::SkillError::SkillDisabled { .. } => {
                        ProtocolErrorCode::InvalidParams
                    }
                    lpa_core::SkillError::SkillParseFailed { .. }
                    | lpa_core::SkillError::SkillRootUnavailable { .. }
                    | lpa_core::SkillError::DuplicateSkillId { .. } => {
                        ProtocolErrorCode::InternalError
                    }
                };
                return self.error_response(
                    request_id,
                    code,
                    format!("failed to resolve turn input: {error}"),
                );
            }
        }) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn input is empty",
            );
        };

        let now = Utc::now();
        let turn = {
            let mut session = session_arc.lock().await;
            if session.active_turn.is_some() {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::TurnAlreadyRunning,
                    "session already has an active turn",
                );
            }
            if let Some(cwd) = params.cwd.clone() {
                session.summary.cwd = cwd.clone();
                session.core_session.lock().await.cwd = cwd;
            }
            let requested_model = params
                .model
                .as_deref()
                .or(session.summary.resolved_model.as_deref());
            let turn_config = self
                .deps
                .resolve_turn_config(requested_model, params.thinking.clone());
            let resolved_request = turn_config
                .model
                .resolve_thinking_selection(turn_config.thinking_selection.as_deref());
            session.summary.resolved_model = Some(turn_config.model.slug.clone());
            let turn = TurnSummary {
                turn_id: TurnId::new(),
                session_id: params.session_id,
                sequence: session
                    .latest_turn
                    .as_ref()
                    .map_or(1, |turn| turn.sequence + 1),
                status: TurnStatus::Running,
                model_slug: resolved_request.request_model,
                started_at: now,
                completed_at: None,
                usage: None,
            };
            session.summary.status = SessionRuntimeStatus::ActiveTurn;
            session.summary.updated_at = now;
            session.active_turn = Some(turn.clone());
            session
                .steering_queue
                .lock()
                .expect("steering queue mutex should not be poisoned")
                .clear();
            let runtime = Arc::clone(self);
            let turn_for_task = turn.clone();
            let display_input_for_task = display_input.clone();
            let input_for_task = input_text.clone();
            let turn_config_for_task = turn_config.clone();
            let task = tokio::spawn(async move {
                runtime
                    .execute_turn(
                        params.session_id,
                        turn_for_task,
                        turn_config_for_task,
                        display_input_for_task,
                        input_for_task,
                    )
                    .await;
            });
            self.active_tasks
                .lock()
                .await
                .insert(params.session_id, task.abort_handle());
            turn
        };
        self.maybe_assign_provisional_title(params.session_id, &display_input)
            .await;
        if let Some(record) = session_arc.lock().await.record.clone()
            && let Err(error) = self
                .rollout_store
                .append_turn(&record, build_turn_record(&turn))
        {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to persist turn start: {error}"),
            );
        }

        tracing::info!(
            session_id = %params.session_id,
            turn_id = %turn.turn_id,
            sequence = turn.sequence,
            model_slug = %turn.model_slug,
            input_chars = input_text.len(),
            "started turn"
        );
        self.broadcast_event(ServerEvent::SessionStatusChanged(
            SessionStatusChangedPayload {
                session_id: params.session_id,
                status: SessionRuntimeStatus::ActiveTurn,
            },
        ))
        .await;
        self.broadcast_event(ServerEvent::TurnStarted(TurnEventPayload {
            session_id: params.session_id,
            turn: turn.clone(),
        }))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnStartResult {
                turn_id: turn.turn_id,
                status: turn.status.clone(),
                accepted_at: now,
            },
        })
        .expect("serialize turn/start response")
    }

    pub(super) async fn handle_turn_interrupt(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: TurnInterruptParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid turn/interrupt params: {error}"),
                );
            }
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        if let Some(task) = self.active_tasks.lock().await.remove(&params.session_id) {
            task.abort();
        }
        let interrupted_turn = {
            let mut session = session_arc.lock().await;
            let Some(mut turn) = session.active_turn.take() else {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::TurnNotFound,
                    "turn is not active",
                );
            };
            if turn.turn_id != params.turn_id {
                session.active_turn = Some(turn);
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::TurnNotFound,
                    "turn does not exist",
                );
            }
            turn.status = TurnStatus::Interrupted;
            turn.completed_at = Some(Utc::now());
            session.latest_turn = Some(turn.clone());
            session.summary.status = SessionRuntimeStatus::Idle;
            session.summary.updated_at = Utc::now();
            let totals = session.core_session.try_lock().ok().map(|core_session| {
                (
                    core_session.total_input_tokens,
                    core_session.total_output_tokens,
                )
            });
            if let Some((total_input_tokens, total_output_tokens)) = totals {
                session.summary.total_input_tokens = total_input_tokens;
                session.summary.total_output_tokens = total_output_tokens;
            }
            turn
        };
        self.approval_manager
            .lock()
            .await
            .cancel_for_turn(&interrupted_turn.turn_id);
        if let Some(record) = session_arc.lock().await.record.clone()
            && let Err(error) = self
                .rollout_store
                .append_turn(&record, build_turn_record(&interrupted_turn))
        {
            return self.error_response(
                request_id,
                ProtocolErrorCode::InternalError,
                format!("failed to persist interrupted turn: {error}"),
            );
        }

        tracing::info!(
            session_id = %params.session_id,
            turn_id = %interrupted_turn.turn_id,
            status = ?interrupted_turn.status,
            "interrupted turn"
        );
        self.broadcast_event(ServerEvent::TurnInterrupted(TurnEventPayload {
            session_id: params.session_id,
            turn: interrupted_turn.clone(),
        }))
        .await;
        self.broadcast_event(ServerEvent::TurnCompleted(TurnEventPayload {
            session_id: params.session_id,
            turn: interrupted_turn.clone(),
        }))
        .await;
        self.broadcast_event(ServerEvent::SessionStatusChanged(
            SessionStatusChangedPayload {
                session_id: params.session_id,
                status: SessionRuntimeStatus::Idle,
            },
        ))
        .await;

        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnInterruptResult {
                turn_id: interrupted_turn.turn_id,
                status: interrupted_turn.status,
            },
        })
        .expect("serialize turn/interrupt response")
    }

    pub(super) async fn handle_turn_steer(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: TurnSteerParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid turn/steer params: {error}"),
                );
            }
        };
        if params.input.is_empty() {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn steer input is empty",
            );
        }
        let Some(display_input) = render_input_items(&params.input) else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::EmptyInput,
                "turn steer input is empty",
            );
        };
        let Some(session_arc) = self.sessions.lock().await.get(&params.session_id).cloned() else {
            return self.error_response(
                request_id,
                ProtocolErrorCode::SessionNotFound,
                "session does not exist",
            );
        };
        let (turn_id, workspace_root, steering_queue) = {
            let session = session_arc.lock().await;
            let Some(turn_id) = session.active_turn.as_ref().map(|turn| turn.turn_id) else {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::NoActiveTurn,
                    "no active turn exists",
                );
            };
            if turn_id != params.expected_turn_id {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::ExpectedTurnMismatch,
                    "active turn did not match expectedTurnId",
                );
            }
            (
                turn_id,
                session.summary.cwd.clone(),
                Arc::clone(&session.steering_queue),
            )
        };
        let prompt_text = match self
            .deps
            .resolve_input_items(&params.input, Some(workspace_root.as_path()))
        {
            Ok(Some(input_text)) => input_text,
            Ok(None) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::EmptyInput,
                    "turn steer input is empty",
                );
            }
            Err(error) => {
                let code = match error {
                    lpa_core::SkillError::SkillNotFound { .. }
                    | lpa_core::SkillError::SkillDisabled { .. } => {
                        ProtocolErrorCode::InvalidParams
                    }
                    lpa_core::SkillError::SkillParseFailed { .. }
                    | lpa_core::SkillError::SkillRootUnavailable { .. }
                    | lpa_core::SkillError::DuplicateSkillId { .. } => {
                        ProtocolErrorCode::InternalError
                    }
                };
                return self.error_response(
                    request_id,
                    code,
                    format!("failed to resolve turn steer input: {error}"),
                );
            }
        };

        self.emit_turn_item(
            params.session_id,
            turn_id,
            ItemKind::UserMessage,
            TurnItem::SteerInput(TextItem {
                text: display_input.clone(),
            }),
            serde_json::json!({ "title": "You", "text": display_input }),
        )
        .await;
        steering_queue
            .lock()
            .expect("steering queue mutex should not be poisoned")
            .push_back(prompt_text);

        self.emit_to_connection(
            connection_id,
            "serverRequest/resolved",
            ServerEvent::ServerRequestResolved(crate::ServerRequestResolvedPayload {
                session_id: params.session_id,
                request_id: "steer-accepted".into(),
                turn_id: Some(turn_id),
            }),
        )
        .await;
        tracing::info!(
            connection_id,
            session_id = %params.session_id,
            turn_id = %turn_id,
            input_items = params.input.len(),
            "accepted turn steer request"
        );
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: TurnSteerResult { turn_id },
        })
        .expect("serialize turn/steer response")
    }
}

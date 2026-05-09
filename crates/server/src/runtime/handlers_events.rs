use std::sync::Arc;

use lpa_core::{ApprovalDecisionItem, TurnItem};

use crate::{
    EventsSubscribeParams, EventsSubscribeResult, ItemKind, ProtocolErrorCode, ServerEvent,
    ServerRequestResolvedPayload, SuccessResponse, approval::ApprovalRespondParams,
};

use super::ServerRuntime;
use super::connection_runtime::SubscriptionFilter;

impl ServerRuntime {
    pub(super) async fn handle_events_subscribe(
        &self,
        connection_id: u64,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: EventsSubscribeParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid events/subscribe params: {error}"),
                );
            }
        };
        if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
            connection.subscriptions.push(SubscriptionFilter {
                session_id: params.session_id,
                event_types: params.event_types.unwrap_or_default().into_iter().collect(),
            });
        }
        serde_json::to_value(SuccessResponse {
            id: request_id,
            result: EventsSubscribeResult {
                subscription_id: format!("sub-{connection_id}-1").into(),
            },
        })
        .expect("serialize events/subscribe response")
    }

    pub(super) async fn handle_approval_respond(
        &self,
        request_id: serde_json::Value,
        params: serde_json::Value,
    ) -> serde_json::Value {
        let params: ApprovalRespondParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(error) => {
                return self.error_response(
                    request_id,
                    ProtocolErrorCode::InvalidParams,
                    format!("invalid approval/respond params: {error}"),
                );
            }
        };

        let resolved = self.approval_manager.lock().await.respond(
            &params.approval_id,
            params.decision.clone(),
            params.scope.clone(),
        );

        match resolved {
            Ok(approval) => {
                tracing::info!(
                    approval_id = %params.approval_id,
                    decision = ?params.decision,
                    session_id = %approval.session_id,
                    turn_id = %approval.turn_id,
                    "resolved approval request"
                );
                if matches!(&params.decision, crate::ApprovalDecisionValue::Approve)
                    && matches!(
                        &params.scope,
                        crate::ApprovalScopeValue::Session | crate::ApprovalScopeValue::Tool
                    )
                {
                    let cache_arc = if let Some(session_arc) = self
                        .sessions
                        .lock()
                        .await
                        .get(&approval.session_id)
                        .cloned()
                    {
                        let session = session_arc.lock().await;
                        Some(Arc::clone(&session.approval_cache))
                    } else {
                        None
                    };
                    if let Some(cache_arc) = cache_arc {
                        let mut cache = cache_arc.lock().await;
                        cache.tool_scopes.insert(approval.tool_name.clone());
                    }
                }
                let decision_str = match &params.decision {
                    crate::ApprovalDecisionValue::Approve => "approve",
                    crate::ApprovalDecisionValue::Deny => "deny",
                    crate::ApprovalDecisionValue::Cancel => "cancel",
                };
                let scope_str = match &params.scope {
                    crate::ApprovalScopeValue::Once => "once",
                    crate::ApprovalScopeValue::Turn => "turn",
                    crate::ApprovalScopeValue::Session => "session",
                    crate::ApprovalScopeValue::PathPrefix => "path_prefix",
                    crate::ApprovalScopeValue::Host => "host",
                    crate::ApprovalScopeValue::Tool => "tool",
                };
                self.emit_turn_item(
                    approval.session_id,
                    approval.turn_id,
                    ItemKind::ApprovalDecision,
                    TurnItem::ApprovalDecision(ApprovalDecisionItem {
                        approval_id: params.approval_id.to_string(),
                        decision: decision_str.to_string(),
                        scope: scope_str.to_string(),
                    }),
                    serde_json::json!({
                        "approval_id": params.approval_id,
                        "decision": decision_str,
                        "scope": scope_str,
                    }),
                )
                .await;
                self.broadcast_event(ServerEvent::ServerRequestResolved(
                    ServerRequestResolvedPayload {
                        session_id: approval.session_id,
                        request_id: params.approval_id.clone(),
                        turn_id: Some(approval.turn_id),
                    },
                ))
                .await;
                serde_json::to_value(SuccessResponse {
                    id: request_id,
                    result: serde_json::json!({ "resolved": true }),
                })
                .expect("serialize approval/respond response")
            }
            Err(()) => self.error_response(
                request_id,
                ProtocolErrorCode::ApprovalNotFound,
                "no pending approval request exists with that approval_id",
            ),
        }
    }
}

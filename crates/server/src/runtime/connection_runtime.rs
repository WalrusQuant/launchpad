use std::collections::HashSet;

use tokio::sync::mpsc;

use lpa_core::SessionId;

use crate::{ClientTransportKind, ConnectionState, NotificationEnvelope, ServerEvent};

pub(super) struct ConnectionRuntime {
    pub(super) transport: ClientTransportKind,
    pub(super) state: ConnectionState,
    pub(super) sender: mpsc::UnboundedSender<serde_json::Value>,
    pub(super) opt_out_notification_methods: HashSet<String>,
    pub(super) subscriptions: Vec<SubscriptionFilter>,
    pub(super) next_event_seq: u64,
}

impl ConnectionRuntime {
    pub(super) fn should_deliver(&self, method: &str, session_id: Option<SessionId>) -> bool {
        if self.opt_out_notification_methods.contains(method) {
            return false;
        }
        if self.transport == ClientTransportKind::Stdio {
            return true;
        }
        if self.subscriptions.is_empty() {
            return false;
        }
        self.subscriptions.iter().any(|subscription| {
            let session_matches = subscription
                .session_id
                .is_none_or(|expected| session_id == Some(expected));
            let event_matches =
                subscription.event_types.is_empty() || subscription.event_types.contains(method);
            session_matches && event_matches
        })
    }

    pub(super) fn next_seq(&mut self) -> u64 {
        let seq = self.next_event_seq;
        self.next_event_seq += 1;
        seq
    }
}

pub(super) struct SubscriptionFilter {
    pub(super) session_id: Option<SessionId>,
    pub(super) event_types: HashSet<String>,
}

use super::ServerRuntime;

impl ServerRuntime {
    pub(super) async fn subscribe_connection_to_session(
        &self,
        connection_id: u64,
        session_id: SessionId,
        event_types: Option<HashSet<String>>,
    ) {
        if let Some(connection) = self.connections.lock().await.get_mut(&connection_id) {
            let desired = event_types.unwrap_or_default();
            let already = connection.subscriptions.iter().any(|subscription| {
                subscription.session_id == Some(session_id) && subscription.event_types == desired
            });
            if already {
                return;
            }
            connection.subscriptions.push(SubscriptionFilter {
                session_id: Some(session_id),
                event_types: desired,
            });
        }
    }

    pub(super) async fn connection_ready(&self, connection_id: u64) -> bool {
        self.connections
            .lock()
            .await
            .get(&connection_id)
            .is_some_and(|connection| connection.state == ConnectionState::Ready)
    }

    pub(super) async fn emit_to_connection(
        &self,
        connection_id: u64,
        method: &str,
        event: ServerEvent,
    ) {
        let session_id = event.session_id();
        let mut connections = self.connections.lock().await;
        if let Some(connection) = connections.get_mut(&connection_id) {
            if !connection.should_deliver(method, session_id) {
                return;
            }
            let value = serde_json::to_value(NotificationEnvelope {
                method: method.to_string(),
                params: event.with_seq(connection.next_seq()),
            })
            .expect("serialize notification");
            let _ = connection.sender.send(value);
        }
    }

    pub(super) async fn broadcast_event(&self, event: ServerEvent) {
        let method = event.method_name();
        let session_id = event.session_id();
        let mut connections = self.connections.lock().await;
        for connection in connections.values_mut() {
            if !connection.should_deliver(method, session_id) {
                continue;
            }
            let value = serde_json::to_value(NotificationEnvelope {
                method: method.to_string(),
                params: event.clone().with_seq(connection.next_seq()),
            })
            .expect("serialize notification");
            let _ = connection.sender.send(value);
        }
    }

    pub(super) fn error_response(
        &self,
        request_id: serde_json::Value,
        code: crate::ProtocolErrorCode,
        message: impl Into<String>,
    ) -> serde_json::Value {
        let message = message.into();
        tracing::warn!(
            request_id = %request_id,
            code = ?code,
            error_message = %message,
            "returning protocol error"
        );
        serde_json::to_value(crate::ErrorResponse {
            id: request_id,
            error: crate::ProtocolError {
                code,
                message,
                data: serde_json::json!({}),
            },
        })
        .expect("serialize error response")
    }
}

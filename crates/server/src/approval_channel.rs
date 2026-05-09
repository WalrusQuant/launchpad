use smol_str::SmolStr;
use tokio::sync::{mpsc, oneshot};

use lpa_core::QueryEvent;
use lpa_protocol::{SessionId, TurnId};
use lpa_tools::{ApprovalChannel, ApprovalResult};

use crate::SharedApprovalManager;

pub struct ServerApprovalChannel {
    approval_manager: SharedApprovalManager,
    session_id: SessionId,
    turn_id: TurnId,
    event_tx: mpsc::UnboundedSender<QueryEvent>,
}

impl ServerApprovalChannel {
    pub fn new(
        approval_manager: SharedApprovalManager,
        session_id: SessionId,
        turn_id: TurnId,
        event_tx: mpsc::UnboundedSender<QueryEvent>,
    ) -> Self {
        Self {
            approval_manager,
            session_id,
            turn_id,
            event_tx,
        }
    }
}

#[async_trait::async_trait]
impl ApprovalChannel for ServerApprovalChannel {
    async fn request_approval(
        &self,
        approval_id: SmolStr,
        tool_name: String,
        action_summary: String,
        justification: String,
    ) -> oneshot::Receiver<ApprovalResult> {
        let rx = self.approval_manager.lock().await.register(
            approval_id.clone(),
            self.session_id,
            self.turn_id,
            tool_name,
            action_summary.clone(),
            justification.clone(),
        );

        let _ = self.event_tx.send(QueryEvent::ApprovalRequest {
            approval_id: approval_id.to_string(),
            action_summary,
            justification,
        });

        rx
    }
}

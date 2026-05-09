use std::collections::HashMap;
use std::sync::Arc;

use smol_str::SmolStr;
use tokio::sync::{Mutex, oneshot};

use lpa_protocol::{SessionId, TurnId};
use lpa_tools::ApprovalResult;

pub use lpa_protocol::{
    ApprovalDecisionValue, ApprovalRespondParams, ApprovalScopeValue, EventsSubscribeParams,
    EventsSubscribeResult,
};

/// One pending approval waiting for a client response.
struct PendingApproval {
    session_id: SessionId,
    turn_id: TurnId,
    tool_name: String,
    action_summary: String,
    justification: String,
    responder: oneshot::Sender<ApprovalResult>,
}

impl std::fmt::Debug for PendingApproval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingApproval")
            .field("session_id", &self.session_id)
            .field("turn_id", &self.turn_id)
            .field("tool_name", &self.tool_name)
            .field("action_summary", &self.action_summary)
            .field("justification", &self.justification)
            .finish_non_exhaustive()
    }
}

/// Manages pending approval requests across all sessions.
///
/// When a tool needs interactive approval, the orchestrator registers a pending
/// approval here and awaits the oneshot receiver. When the client sends
/// `approval/respond`, the manager looks up the pending entry and sends the
/// result back through the oneshot channel, unblocking the tool.
#[derive(Debug, Default)]
pub struct ApprovalManager {
    pending: HashMap<SmolStr, PendingApproval>,
}

impl ApprovalManager {
    pub fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Registers a new pending approval and returns the oneshot receiver
    /// that the orchestrator will await.
    ///
    /// If an approval with the same ID already exists (should not happen in
    /// practice), the previous one is cancelled and dropped.
    pub fn register(
        &mut self,
        approval_id: SmolStr,
        session_id: SessionId,
        turn_id: TurnId,
        tool_name: String,
        action_summary: String,
        justification: String,
    ) -> oneshot::Receiver<ApprovalResult> {
        let (tx, rx) = oneshot::channel();
        let previous = self.pending.insert(
            approval_id,
            PendingApproval {
                session_id,
                turn_id,
                tool_name,
                action_summary,
                justification,
                responder: tx,
            },
        );
        if let Some(previous) = previous {
            let _ = previous.responder.send(ApprovalResult { approved: false });
        }
        rx
    }

    /// Resolves a pending approval by looking up the approval ID and sending
    /// the result through the oneshot channel.
    ///
    /// Returns `Ok(ResolvedApproval)` if the approval was found and resolved,
    /// or `Err(())` if no pending approval with that ID exists. The only
    /// failure mode is "not found" — callers don't need a richer error type.
    #[allow(clippy::result_unit_err)]
    pub fn respond(
        &mut self,
        approval_id: &SmolStr,
        decision: ApprovalDecisionValue,
        _scope: ApprovalScopeValue,
    ) -> Result<ResolvedApproval, ()> {
        let pending = self.pending.remove(approval_id).ok_or(())?;
        let approved = matches!(decision, ApprovalDecisionValue::Approve);
        let _ = pending
            .responder
            .send(lpa_tools::ApprovalResult { approved });
        Ok(ResolvedApproval {
            session_id: pending.session_id,
            turn_id: pending.turn_id,
            tool_name: pending.tool_name,
            action_summary: pending.action_summary,
            justification: pending.justification,
        })
    }

    /// Cancels all pending approvals for a given turn (e.g., on turn interrupt).
    pub fn cancel_for_turn(&mut self, turn_id: &TurnId) {
        let matching_ids: Vec<SmolStr> = self
            .pending
            .iter()
            .filter(|(_, pending)| &pending.turn_id == turn_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in matching_ids {
            if let Some(pending) = self.pending.remove(&id) {
                let _ = pending.responder.send(ApprovalResult { approved: false });
            }
        }
    }

    /// Cancels all pending approvals for a given session (e.g., on session close).
    pub fn cancel_for_session(&mut self, session_id: &SessionId) {
        let matching_ids: Vec<SmolStr> = self
            .pending
            .iter()
            .filter(|(_, pending)| &pending.session_id == session_id)
            .map(|(id, _)| id.clone())
            .collect();
        for id in matching_ids {
            if let Some(pending) = self.pending.remove(&id) {
                let _ = pending.responder.send(ApprovalResult { approved: false });
            }
        }
    }
}

/// The resolved approval information returned when an approval is successfully responded to.
#[derive(Debug, Clone)]
pub struct ResolvedApproval {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub tool_name: String,
    pub action_summary: String,
    pub justification: String,
}

/// Thread-safe handle to the approval manager shared across the server runtime.
pub type SharedApprovalManager = Arc<Mutex<ApprovalManager>>;

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{SessionId, TurnId};

/// Describes a client response to a pending approval request.
///
/// `turn_id` is optional: the approval_id alone is enough to look up the
/// pending entry in `ApprovalManager`, and historical client builds (plus
/// the Launchpad Tauri bridge before mid-2026) omit it. When absent the
/// server resolves turn_id from the registered PendingApproval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespondParams {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub approval_id: SmolStr,
    pub decision: ApprovalDecisionValue,
    pub scope: ApprovalScopeValue,
}

/// Response returned by `approval/respond` once the server has marked the
/// pending approval as resolved and broadcast the resulting rollout events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRespondResult {
    pub resolved: bool,
}

/// Enumerates client decisions for approval requests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionValue {
    Approve,
    Deny,
    Cancel,
}

/// Enumerates the scopes supported by approval responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScopeValue {
    Once,
    Turn,
    Session,
    PathPrefix,
    Host,
    Tool,
}

/// Describes the payload for `events/subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeParams {
    pub session_id: Option<SessionId>,
    pub event_types: Option<Vec<String>>,
}

/// Describes the response returned by `events/subscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSubscribeResult {
    pub subscription_id: SmolStr,
}

use std::path::PathBuf;
use std::sync::Arc;

use lpa_safety::{ApprovalCache, legacy_permissions::PermissionPolicy};

/// Shared handle to one session's approval cache. The server runtime owns the
/// canonical cache on `RuntimeSession`; both the tool orchestrator (read side)
/// and the approval-respond RPC (write side) clone the same Arc so updates
/// made by one are immediately visible to the other.
pub type SharedApprovalCache = Arc<tokio::sync::Mutex<ApprovalCache>>;

/// The execution context provided to every tool call.
///
/// Instead of a monolithic context object, tools receive only the
/// dependencies they actually need. This makes tool implementations
/// easier to test and reason about.
pub struct ToolContext {
    /// Current working directory for the session.
    pub cwd: PathBuf,
    /// The permission policy in effect.
    pub permissions: Arc<dyn PermissionPolicy>,
    /// Session-level metadata tools can use for state.
    pub session_id: String,
}

/// Channel trait for sending approval requests and awaiting responses.
///
/// The server runtime provides an implementation that bridges to
/// `ApprovalManager`, while tests can provide a mock.
#[async_trait::async_trait]
pub trait ApprovalChannel: Send + Sync {
    /// Sends an approval request and returns a receiver that resolves when
    /// the user responds. `tool_name` is carried through so the server can
    /// apply session-scoped approvals back to the shared `ApprovalCache`.
    async fn request_approval(
        &self,
        approval_id: smol_str::SmolStr,
        tool_name: String,
        action_summary: String,
        justification: String,
    ) -> tokio::sync::oneshot::Receiver<crate::ApprovalResult>;
}

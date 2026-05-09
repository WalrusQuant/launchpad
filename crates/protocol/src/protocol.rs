use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRequest<T> {
    pub id: serde_json::Value,
    pub method: String,
    pub params: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientNotification<T> {
    pub method: String,
    pub params: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuccessResponse<T> {
    pub id: serde_json::Value,
    pub result: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub id: serde_json::Value,
    pub error: ProtocolError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationEnvelope<T> {
    pub method: String,
    pub params: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerRequestEnvelope<T> {
    pub id: serde_json::Value,
    pub method: String,
    pub params: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ProtocolErrorCode {
    #[error("NotInitialized")]
    NotInitialized,
    #[error("InvalidParams")]
    InvalidParams,
    #[error("SessionNotFound")]
    SessionNotFound,
    #[error("TurnNotFound")]
    TurnNotFound,
    #[error("TurnAlreadyRunning")]
    TurnAlreadyRunning,
    #[error("ApprovalNotFound")]
    ApprovalNotFound,
    #[error("PolicyDenied")]
    PolicyDenied,
    #[error("ContextLimitExceeded")]
    ContextLimitExceeded,
    #[error("NoActiveTurn")]
    NoActiveTurn,
    #[error("ExpectedTurnMismatch")]
    ExpectedTurnMismatch,
    #[error("ActiveTurnNotSteerable")]
    ActiveTurnNotSteerable,
    #[error("EmptyInput")]
    EmptyInput,
    #[error("InternalError")]
    InternalError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
    pub data: serde_json::Value,
}

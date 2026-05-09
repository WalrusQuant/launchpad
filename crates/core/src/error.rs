use thiserror::Error;

use lpa_provider::ProviderError;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("model provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("max turns ({0}) exceeded")]
    MaxTurnsExceeded(usize),

    #[error("context too long after compaction")]
    ContextTooLong,

    #[error("session aborted by user")]
    Aborted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages() {
        let err = AgentError::MaxTurnsExceeded(10);
        assert_eq!(err.to_string(), "max turns (10) exceeded");

        let err = AgentError::ContextTooLong;
        assert_eq!(err.to_string(), "context too long after compaction");

        let err = AgentError::Aborted;
        assert_eq!(err.to_string(), "session aborted by user");
    }

    #[test]
    fn provider_error_from_typed() {
        let provider_err = ProviderError::AuthenticationFailure {
            message: "bad key".to_string(),
        };
        let err: AgentError = provider_err.into();
        assert!(err.to_string().contains("bad key"));
    }
}

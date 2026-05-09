/// Typed errors that model providers can surface to the runtime.
///
/// Providers should map their HTTP-level errors into these variants so the
/// query loop can make retry and compaction decisions without string matching.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// The request exceeded the model's context window.
    #[error("context too long: {message}")]
    ContextTooLong { message: String },

    /// A bad request or invalid parameter was sent (HTTP 400).
    #[error("bad request: {message}")]
    BadRequest { message: String },

    /// Authentication credentials are missing, expired, or invalid (HTTP 401).
    #[error("authentication failure: {message}")]
    AuthenticationFailure { message: String },

    /// The requested resource or endpoint does not exist (HTTP 404).
    #[error("not found: {message}")]
    NotFound { message: String },

    /// The request was rate-limited (HTTP 429).
    #[error("rate limited: {message}")]
    RateLimited {
        message: String,
        retry_after: Option<u64>,
    },

    /// The request was refused due to insufficient API permissions.
    #[error("permission denied: {message}")]
    PermissionDenied { message: String },

    /// A file attached to the request is too large.
    #[error("file too large: {message}")]
    FileTooLarge { message: String },

    /// A file attached to the request has invalid or anomalous content.
    #[error("file content anomaly: {message}")]
    FileContentAnomaly { message: String },

    /// A transient server-side error that may resolve on retry (HTTP 5xx).
    #[error("server error: {message}")]
    ServerError {
        message: String,
        status_code: Option<u16>,
    },

    /// A stream-level error (connection lost, SSE parse failure, etc.).
    #[error("stream error: {message}")]
    StreamError { message: String },

    /// Any other error not covered by the variants above.
    #[error("{message}")]
    Other {
        message: String,
        source: Option<anyhow::Error>,
    },
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::ServerError { .. })
    }

    pub fn is_context_too_long(&self) -> bool {
        matches!(self, Self::ContextTooLong { .. })
    }

    pub fn from_http_status(status: u16, body: &str) -> Self {
        let message = if body.trim().is_empty() {
            format!("HTTP {status}")
        } else {
            format!("HTTP {status}: {body}")
        };

        match status {
            400 => {
                let lower = body.to_lowercase();
                if lower.contains("context_too_long") {
                    Self::ContextTooLong { message }
                } else if lower.contains("file content anomaly")
                    || lower.contains("jsonl file content")
                    || lower.contains("jsonl")
                {
                    Self::FileContentAnomaly { message }
                } else {
                    Self::BadRequest { message }
                }
            }
            401 => Self::AuthenticationFailure { message },
            404 => Self::NotFound { message },
            429 => Self::RateLimited {
                message,
                retry_after: None,
            },
            _ if (500..600).contains(&status) => Self::ServerError {
                message,
                status_code: Some(status),
            },
            _ => Self::Other {
                message,
                source: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_http_status_maps_context_too_long() {
        let err = ProviderError::from_http_status(
            400,
            r#"{"error":{"type":"error","message":"context_too_long"}}"#,
        );
        assert!(matches!(err, ProviderError::ContextTooLong { .. }));
        assert!(err.is_context_too_long());
        assert!(!err.is_retryable());
    }

    #[test]
    fn from_http_status_maps_401() {
        let err = ProviderError::from_http_status(401, "unauthorized");
        assert!(matches!(err, ProviderError::AuthenticationFailure { .. }));
    }

    #[test]
    fn from_http_status_maps_429() {
        let err = ProviderError::from_http_status(429, "rate limited");
        assert!(matches!(err, ProviderError::RateLimited { .. }));
        assert!(err.is_retryable());
    }

    #[test]
    fn from_http_status_maps_5xx() {
        let err = ProviderError::from_http_status(503, "service unavailable");
        assert!(matches!(
            err,
            ProviderError::ServerError {
                status_code: Some(503),
                ..
            }
        ));
        assert!(err.is_retryable());
    }

    #[test]
    fn from_http_status_maps_unknown_to_other() {
        let err = ProviderError::from_http_status(418, "I'm a teapot");
        assert!(matches!(err, ProviderError::Other { .. }));
    }
}

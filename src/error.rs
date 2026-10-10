/// Every failure the bindings report. Kotlin sees `MsgtransException`, Swift
/// sees `MsgtransError`; each variant carries the same fields msgtrans does.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MsgtransError {
    #[error("invalid configuration ({field}): {reason}")]
    Configuration { field: String, reason: String },
    #[error("connection error: {reason}")]
    Connection { reason: String, retryable: bool },
    /// `protocol_name`, not `protocol`: `protocol` is a Swift keyword and the
    /// generated Swift for it draws a compiler warning.
    #[error("protocol error ({protocol_name}): {reason}")]
    Protocol {
        protocol_name: String,
        reason: String,
    },
    #[error("resource exhausted: {resource} ({current}/{limit})")]
    Resource {
        resource: String,
        current: u64,
        limit: u64,
    },
    #[error("{operation} timed out after {duration_ms}ms")]
    Timeout { operation: String, duration_ms: u64 },
    /// The call is not valid for the object's current state, e.g. responding
    /// to a request that was already answered.
    #[error("{detail}")]
    InvalidState { detail: String },
    /// A msgtrans error kind newer than these bindings.
    #[error("{detail}")]
    Other { detail: String },
}

impl From<msgtrans::TransportError> for MsgtransError {
    fn from(error: msgtrans::TransportError) -> Self {
        use msgtrans::TransportError as E;
        match error {
            E::Configuration { field, reason, .. } => Self::Configuration { field, reason },
            E::Connection {
                reason, retryable, ..
            } => Self::Connection { reason, retryable },
            E::Protocol {
                protocol, reason, ..
            } => Self::Protocol {
                protocol_name: protocol,
                reason,
            },
            E::Resource {
                resource,
                current,
                limit,
                ..
            } => Self::Resource {
                resource,
                current: current as u64,
                limit: limit as u64,
            },
            E::Timeout {
                operation,
                duration,
                ..
            } => Self::Timeout {
                operation,
                duration_ms: duration.as_millis() as u64,
            },
            other => Self::Other {
                detail: other.to_string(),
            },
        }
    }
}

impl MsgtransError {
    pub(crate) fn config(field: &str, reason: impl Into<String>) -> Self {
        Self::Configuration {
            field: field.to_string(),
            reason: reason.into(),
        }
    }
}

impl From<msgtrans::spi::ConfigError> for MsgtransError {
    fn from(error: msgtrans::spi::ConfigError) -> Self {
        Self::config("endpoint", error.to_string())
    }
}

use crate::{event::TransportEvent, SessionId};
use std::time::Duration;

/// Connection close reason
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CloseReason {
    /// Normal close
    Normal,
    /// Timeout
    Timeout,
    /// Error
    Error(String),
    /// Forced close
    Forced,
}

/// Unified transport error type - simplified version
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum TransportError {
    /// Connection-related errors
    #[error("Connection error: {reason} (retryable: {retryable})")]
    Connection { reason: String, retryable: bool },

    /// Protocol-related errors
    #[error("Protocol error ({protocol}): {reason}")]
    Protocol { protocol: String, reason: String },

    /// Configuration-related errors
    #[error("Configuration error in field '{field}': {reason}")]
    Configuration { field: String, reason: String },

    /// Resource-related errors
    #[error("Resource '{resource}' exceeded: current {current}, limit {limit}")]
    Resource {
        resource: String,
        current: usize,
        limit: usize,
    },

    /// Timeout errors
    #[error("Operation '{operation}' timeout after {duration:?}")]
    Timeout {
        operation: String,
        duration: Duration,
    },
}

impl TransportError {
    /// Check if error is retryable
    pub fn is_retryable(&self) -> bool {
        match self {
            TransportError::Connection { retryable, .. } => *retryable,
            TransportError::Protocol { .. } => true, // Protocol errors are usually retryable
            TransportError::Configuration { .. } => false, // Configuration errors are not retryable
            TransportError::Resource { .. } => true, // Resource errors are retryable (wait for resource release)
            TransportError::Timeout { .. } => true,  // Timeouts are retryable
        }
    }

    /// Get suggested retry delay
    pub fn retry_delay(&self) -> Option<Duration> {
        if !self.is_retryable() {
            return None;
        }

        match self {
            TransportError::Connection { .. } => Some(Duration::from_millis(1000)),
            TransportError::Protocol { .. } => Some(Duration::from_millis(100)),
            TransportError::Resource { .. } => Some(Duration::from_millis(500)),
            TransportError::Timeout { .. } => Some(Duration::from_millis(200)),
            _ => None,
        }
    }

    /// Get error code
    pub fn error_code(&self) -> &'static str {
        match self {
            TransportError::Connection { .. } => "CONNECTION_ERROR",
            TransportError::Protocol { .. } => "PROTOCOL_ERROR",
            TransportError::Configuration { .. } => "CONFIG_ERROR",
            TransportError::Resource { .. } => "RESOURCE_ERROR",
            TransportError::Timeout { .. } => "TIMEOUT_ERROR",
        }
    }

    /// Add session context
    pub fn with_session(mut self, session_id: SessionId) -> Self {
        match &mut self {
            TransportError::Connection { reason, .. } => {
                if !reason.contains("session:") {
                    *reason = format!("{} (session: {})", reason, session_id);
                }
            }
            TransportError::Protocol { reason, .. } if !reason.contains("session:") => {
                *reason = format!("{} (session: {})", reason, session_id);
            }
            _ => {} // Other error types don't need session information
        }
        self
    }

    /// 添加操作上下文
    pub fn with_operation(mut self, op: &'static str) -> Self {
        match &mut self {
            TransportError::Connection { reason, .. } => {
                if !reason.contains("operation:") {
                    *reason = format!("{} (operation: {})", reason, op);
                }
            }
            TransportError::Protocol { reason, .. } => {
                if !reason.contains("operation:") {
                    *reason = format!("{} (operation: {})", reason, op);
                }
            }
            TransportError::Timeout { operation, .. } if operation.is_empty() => {
                *operation = op.to_string();
            }
            _ => {}
        }
        self
    }
}

/// 便利构造函数
impl TransportError {
    /// 创建连接错误
    pub fn connection_error(reason: impl Into<String>, retryable: bool) -> Self {
        Self::Connection {
            reason: reason.into(),
            retryable,
        }
    }

    /// 创建协议错误
    pub fn protocol_error(protocol: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Protocol {
            protocol: protocol.into(),
            reason: reason.into(),
        }
    }

    /// 创建配置错误
    pub fn config_error(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Configuration {
            field: field.into(),
            reason: reason.into(),
        }
    }

    /// 创建资源错误
    pub fn resource_error(resource: impl Into<String>, current: usize, limit: usize) -> Self {
        Self::Resource {
            resource: resource.into(),
            current,
            limit,
        }
    }

    /// 创建超时错误
    pub fn timeout_error(operation: impl Into<String>, duration: Duration) -> Self {
        Self::Timeout {
            operation: operation.into(),
            duration,
        }
    }
}

/// 兼容性转换 - 从标准IO错误
impl From<std::io::Error> for TransportError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::TimedOut {
            return TransportError::timeout_error("io", Duration::from_secs(0));
        }

        let retryable = matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionRefused
                | std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::Interrupted
        );

        TransportError::Connection {
            reason: format!("IO error: {}", error),
            retryable,
        }
    }
}

/// Convert from String - for lock-free error handling
impl From<String> for TransportError {
    fn from(error: String) -> Self {
        TransportError::connection_error(error, false)
    }
}

impl From<TransportError> for TransportEvent {
    fn from(error: TransportError) -> Self {
        TransportEvent::TransportError { error }
    }
}

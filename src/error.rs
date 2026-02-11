use crate::{event::TransportEvent, SessionId};
use std::time::Duration;

/// Connection close reason
#[derive(Debug, Clone)]
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
            TransportError::Protocol { reason, .. } => {
                if !reason.contains("session:") {
                    *reason = format!("{} (session: {})", reason, session_id);
                }
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
            TransportError::Timeout { operation, .. } => {
                if operation.is_empty() {
                    *operation = op.to_string();
                }
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
        let retryable = match error.kind() {
            std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted => true,
            _ => false,
        };

        TransportError::Connection {
            reason: format!("IO error: {}", error),
            retryable,
        }
    }
}

/// Convert from String - for lock-free error handling
impl From<String> for TransportError {
    fn from(error: String) -> Self {
        TransportError::Resource {
            resource: "lockfree_operation".to_string(),
            current: 0,
            limit: 0,
        }
    }
}

impl From<TransportError> for TransportEvent {
    fn from(error: TransportError) -> Self {
        TransportEvent::TransportError { error }
    }
}

/// 错误统计
#[derive(Debug, Default, Clone)]
pub struct ErrorStats {
    pub total_errors: u64,
    pub connection_errors: u64,
    pub protocol_errors: u64,
    pub config_errors: u64,
    pub resource_errors: u64,
    pub timeout_errors: u64,
    pub retries: u64,
    pub last_error: Option<TransportError>,
}

impl ErrorStats {
    pub fn record_error(&mut self, error: TransportError) {
        self.total_errors += 1;

        match &error {
            TransportError::Connection { .. } => self.connection_errors += 1,
            TransportError::Protocol { .. } => self.protocol_errors += 1,
            TransportError::Configuration { .. } => self.config_errors += 1,
            TransportError::Resource { .. } => self.resource_errors += 1,
            TransportError::Timeout { .. } => self.timeout_errors += 1,
        }

        self.last_error = Some(error);
    }

    pub fn record_retry(&mut self) {
        self.retries += 1;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// 错误分级
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorSeverity {
    Low,      // 可忽略的错误
    Medium,   // 需要关注的错误
    High,     // 需要处理的错误
    Critical, // 严重错误
}

impl TransportError {
    /// 获取错误严重性
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            TransportError::Configuration { .. } => ErrorSeverity::Critical,
            TransportError::Resource { current, limit, .. } => {
                let usage_ratio = *current as f64 / *limit as f64;
                if usage_ratio > 0.9 {
                    ErrorSeverity::Critical
                } else if usage_ratio > 0.8 {
                    ErrorSeverity::High
                } else {
                    ErrorSeverity::Medium
                }
            }
            TransportError::Connection { retryable, .. } => {
                if *retryable {
                    ErrorSeverity::Medium
                } else {
                    ErrorSeverity::High
                }
            }
            TransportError::Protocol { .. } => ErrorSeverity::Medium,
            TransportError::Timeout { .. } => ErrorSeverity::Medium,
        }
    }
}

/// 向后兼容的错误转换 - 用于旧代码迁移
impl TransportError {
    /// 兼容旧的Connection错误格式
    pub fn connection_legacy(reason: impl Into<String>) -> Self {
        Self::Connection {
            reason: reason.into(),
            retryable: true,
        }
    }

    /// 兼容旧的Protocol错误格式
    pub fn protocol_legacy(reason: impl Into<String>) -> Self {
        Self::Protocol {
            protocol: "unknown".to_string(),
            reason: reason.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Graceful shutdown timeout duration
    pub graceful_timeout: std::time::Duration,
    /// Per-connection resource limits (write deadline, pipe/outbound capacity).
    pub(crate) connection_limits: crate::transport::limits::ConnectionLimits,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            graceful_timeout: std::time::Duration::from_secs(5),
            connection_limits: crate::transport::limits::ConnectionLimits::default(),
        }
    }
}

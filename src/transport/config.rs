#[derive(Debug, Clone)]
pub struct TransportConfig {
    pub global: GlobalConfig,
    /// Graceful shutdown timeout duration
    pub graceful_timeout: std::time::Duration,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            global: GlobalConfig::default(),
            graceful_timeout: std::time::Duration::from_secs(5),
        }
    }
}

impl TransportConfig {
    pub fn validate(&self) -> Result<(), crate::error::TransportError> {
        // Implement configuration validation logic
        Ok(())
    }
}

#[derive(Default, Debug, Clone)]
pub struct GlobalConfig;

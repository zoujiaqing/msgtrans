use super::adapter::ProtocolConfig;

/// Empty configuration type used by FlumePoweredProtocolAdapter
#[derive(Debug, Clone)]
pub struct EmptyConfig;

impl ProtocolConfig for EmptyConfig {
    fn validate(&self) -> Result<(), super::adapter::ConfigError> {
        Ok(())
    }

    fn default_config() -> Self {
        Self
    }
}

use serde::{Deserialize, Serialize};
/// Expert-level configuration - Advanced Builder configuration options
///
/// Provides fine-grained performance tuning, monitoring and resilience configuration options for advanced users
use std::time::Duration;

use super::pool::ExpansionStrategy;
use crate::error::TransportError;

/// Smart connection pool expert configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmartPoolConfig {
    /// Initial connection pool size
    pub initial_size: usize,
    /// Maximum connection pool size
    pub max_size: usize,
    /// Expansion threshold (0.0-1.0, usage rate triggers expansion)
    pub expansion_threshold: f64,
    /// Shrink threshold (0.0-1.0, usage rate triggers shrinking)
    pub shrink_threshold: f64,
    /// Expansion cooldown time
    pub expansion_cooldown: Duration,
    /// Shrink cooldown time
    pub shrink_cooldown: Duration,
    /// Minimum retained connections
    pub min_connections: usize,
    /// Connection warmup switch
    pub enable_warmup: bool,
    /// Custom expansion factor sequence
    pub expansion_factors: Vec<f64>,
}

impl Default for SmartPoolConfig {
    fn default() -> Self {
        Self {
            initial_size: 100,
            max_size: 2000,
            expansion_threshold: 0.8, // 80% usage rate triggers expansion
            shrink_threshold: 0.3,    // 30% usage rate triggers shrinking
            expansion_cooldown: Duration::from_secs(30),
            shrink_cooldown: Duration::from_secs(60),
            min_connections: 10,
            enable_warmup: true,
            expansion_factors: vec![2.0, 1.5, 1.2, 1.1], // Progressive expansion factors
        }
    }
}

impl SmartPoolConfig {
    /// Create high-performance configuration preset
    pub fn high_performance() -> Self {
        Self {
            initial_size: 200,
            max_size: 5000,
            expansion_threshold: 0.75,
            shrink_threshold: 0.25,
            expansion_cooldown: Duration::from_secs(15),
            shrink_cooldown: Duration::from_secs(45),
            min_connections: 50,
            enable_warmup: true,
            expansion_factors: vec![2.5, 2.0, 1.5, 1.2, 1.1],
        }
    }

    /// Create conservative configuration preset
    pub fn conservative() -> Self {
        Self {
            initial_size: 50,
            max_size: 1000,
            expansion_threshold: 0.9,
            shrink_threshold: 0.2,
            expansion_cooldown: Duration::from_secs(60),
            shrink_cooldown: Duration::from_secs(120),
            min_connections: 5,
            enable_warmup: false,
            expansion_factors: vec![1.5, 1.3, 1.2, 1.1],
        }
    }

    /// Validate configuration validity
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.initial_size == 0 {
            return Err(TransportError::config_error(
                "smart_pool",
                "initial_size must be > 0",
            ));
        }

        if self.max_size < self.initial_size {
            return Err(TransportError::config_error(
                "smart_pool",
                "max_size must be >= initial_size",
            ));
        }

        if self.expansion_threshold <= 0.0 || self.expansion_threshold >= 1.0 {
            return Err(TransportError::config_error(
                "smart_pool",
                "expansion_threshold must be in (0.0, 1.0)",
            ));
        }

        if self.shrink_threshold <= 0.0 || self.shrink_threshold >= 1.0 {
            return Err(TransportError::config_error(
                "smart_pool",
                "shrink_threshold must be in (0.0, 1.0)",
            ));
        }

        if self.expansion_threshold <= self.shrink_threshold {
            return Err(TransportError::config_error(
                "smart_pool",
                "expansion_threshold must be > shrink_threshold",
            ));
        }

        if self.expansion_factors.is_empty() {
            return Err(TransportError::config_error(
                "smart_pool",
                "expansion_factors cannot be empty",
            ));
        }

        for factor in &self.expansion_factors {
            if *factor <= 1.0 {
                return Err(TransportError::config_error(
                    "smart_pool",
                    "all expansion_factors must be > 1.0",
                ));
            }
        }

        Ok(())
    }

    /// Convert to ExpansionStrategy
    pub fn to_expansion_strategy(&self) -> ExpansionStrategy {
        ExpansionStrategy {
            factors: self.expansion_factors.clone(),
            current_factor_index: 0,
            expansion_threshold: self.expansion_threshold,
            shrink_threshold: self.shrink_threshold,
        }
    }
}

/// Performance monitoring expert configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceConfig {
    /// Enable detailed monitoring
    pub enable_detailed_monitoring: bool,
    /// Monitoring data retention time
    pub monitoring_retention: Duration,
    /// Performance metrics sampling interval
    pub sampling_interval: Duration,
    /// Enable performance profiling
    pub enable_profiling: bool,
    /// Metrics history retention count
    pub metrics_history_size: usize,
    /// Automatic report interval
    pub auto_report_interval: Option<Duration>,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            enable_detailed_monitoring: true,
            monitoring_retention: Duration::from_secs(3600), // 1 hour
            sampling_interval: Duration::from_millis(100),
            enable_profiling: cfg!(debug_assertions),
            metrics_history_size: 1000,
            auto_report_interval: Some(Duration::from_secs(300)), // 5 minutes
        }
    }
}

impl PerformanceConfig {
    /// Create production environment configuration preset
    pub fn production() -> Self {
        Self {
            enable_detailed_monitoring: true,
            monitoring_retention: Duration::from_secs(7200), // 2 hours
            sampling_interval: Duration::from_millis(200),
            enable_profiling: false,
            metrics_history_size: 2000,
            auto_report_interval: Some(Duration::from_secs(600)), // 10 minutes
        }
    }

    /// Create development environment configuration preset
    pub fn development() -> Self {
        Self {
            enable_detailed_monitoring: true,
            monitoring_retention: Duration::from_secs(1800), // 30 minutes
            sampling_interval: Duration::from_millis(50),
            enable_profiling: true,
            metrics_history_size: 500,
            auto_report_interval: Some(Duration::from_secs(60)), // 1 minute
        }
    }

    /// Validate configuration validity
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.sampling_interval.as_millis() < 10 {
            return Err(TransportError::config_error(
                "performance",
                "sampling_interval must be >= 10ms",
            ));
        }

        if self.monitoring_retention.as_secs() < 60 {
            return Err(TransportError::config_error(
                "performance",
                "monitoring_retention must be >= 60 seconds",
            ));
        }

        if self.metrics_history_size == 0 {
            return Err(TransportError::config_error(
                "performance",
                "metrics_history_size must be > 0",
            ));
        }

        Ok(())
    }
}

/// Expert configuration collection
#[derive(Debug, Clone)]
pub struct ExpertConfig {
    /// Smart connection pool configuration
    pub smart_pool: Option<SmartPoolConfig>,
    /// Performance monitoring configuration
    pub performance: Option<PerformanceConfig>,
}

impl Default for ExpertConfig {
    fn default() -> Self {
        Self {
            smart_pool: None,
            performance: None,
        }
    }
}

impl ExpertConfig {
    /// Create new expert configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate all configurations
    pub fn validate(&self) -> Result<(), TransportError> {
        if let Some(ref config) = self.smart_pool {
            config.validate()?;
        }

        if let Some(ref config) = self.performance {
            config.validate()?;
        }

        Ok(())
    }

    /// Check if any expert configuration is enabled
    pub fn has_expert_config(&self) -> bool {
        self.smart_pool.is_some() || self.performance.is_some()
    }
}

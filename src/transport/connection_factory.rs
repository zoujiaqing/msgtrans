use crate::{transport::LockFreeConnection, Connection, SessionId, TransportError};
/// Connection factory: Unified management of different types of connection creation
///
/// Supports progressive migration from traditional connections to lock-free connections
use tokio::task::JoinHandle;

/// [ULTIMATE] Third stage ultimate simplification: Connection creation result
pub struct ConnectionResult {
    /// Connection instance (unified lock-free connection)
    pub connection: Box<dyn Connection>,
    /// Worker handle
    pub worker_handle: Option<JoinHandle<()>>,
}

/// Connection performance metrics
#[derive(Debug, Clone)]
pub struct ConnectionMetrics {
    /// Creation time
    pub creation_time: std::time::Duration,
    /// Buffer size
    pub buffer_size: usize,
}

/// [STAGE3] Third stage: Simplified connection configuration
#[derive(Clone, Debug)]
pub struct ConnectionConfig {
    /// Buffer size
    pub buffer_size: usize,
    /// Whether to enable performance monitoring
    pub enable_metrics: bool,
    /// Whether to enable intelligent optimization (automatically adjust buffer according to CPU)
    pub auto_optimize: bool,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1500,
            enable_metrics: true,
            auto_optimize: true, // Enable intelligent optimization by default
        }
    }
}

impl ConnectionConfig {
    /// High performance configuration
    pub fn high_performance() -> Self {
        Self {
            buffer_size: 2500,
            enable_metrics: true,
            auto_optimize: false, // Manually specify high performance parameters
        }
    }

    /// Intelligent optimization configuration
    pub fn auto_optimized() -> Self {
        Self {
            buffer_size: 1000, // Base value, will be overridden by intelligent optimization
            enable_metrics: true,
            auto_optimize: true,
        }
    }

    /// Silent configuration (monitoring disabled)
    pub fn silent() -> Self {
        Self {
            buffer_size: 1000,
            enable_metrics: false,
            auto_optimize: false,
        }
    }

    /// Custom buffer size
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self.auto_optimize = false; // Manual specification disables auto-optimization
        self
    }

    /// Enable/disable intelligent optimization
    pub fn with_auto_optimize(mut self, enabled: bool) -> Self {
        self.auto_optimize = enabled;
        self
    }
}

/// [ULTIMATE] Third stage ultimate simplification: Connection factory
pub struct ConnectionFactory;

impl ConnectionFactory {
    /// Create lock-free connection (ultimate simplified API)
    pub fn create_connection(
        adapter: Box<dyn Connection>,
        session_id: SessionId,
    ) -> Result<ConnectionResult, TransportError> {
        Self::create_connection_with_config(adapter, session_id, ConnectionConfig::default())
    }

    /// Create lock-free connection with configuration
    pub fn create_connection_with_config(
        adapter: Box<dyn Connection>,
        session_id: SessionId,
        config: ConnectionConfig,
    ) -> Result<ConnectionResult, TransportError> {
        let start_time = std::time::Instant::now();

        // [STAGE3] Third stage: Intelligent buffer size optimization
        let final_buffer_size = if config.auto_optimize {
            Self::optimize_buffer_size(config.buffer_size)
        } else {
            config.buffer_size
        };

        tracing::info!(
            "[STAGE3] Third stage: Creating lock-free connection (session: {}, buffer: {})",
            session_id,
            final_buffer_size
        );

        // Create lock-free connection
        let (lockfree_conn, worker_handle) =
            LockFreeConnection::new(adapter, session_id, final_buffer_size);

        let result = ConnectionResult {
            connection: Box::new(lockfree_conn),
            worker_handle: Some(worker_handle),
        };

        // Record performance metrics
        if config.enable_metrics {
            let creation_time = start_time.elapsed();
            let metrics = ConnectionMetrics {
                creation_time,
                buffer_size: final_buffer_size,
            };

            Self::record_metrics(session_id, metrics);
        }

        Ok(result)
    }

    /// [STAGE3] Third stage: Intelligent buffer optimization
    fn optimize_buffer_size(base_size: usize) -> usize {
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        let optimized_size = match cpu_count {
            1..=2 => base_size.max(1000), // Single/dual core: Conservative configuration
            3..=4 => base_size.max(1500), // Quad core: Balanced configuration
            5..=8 => base_size.max(2000), // Eight core: High performance configuration
            _ => base_size.max(2500),     // Multi-core: Highest performance configuration
        };

        tracing::debug!(
            "[TARGET] Stage 3 intelligent optimization: CPU {} cores, buffer {} → {}",
            cpu_count,
            base_size,
            optimized_size
        );

        optimized_size
    }

    /// [STAGE3] Third stage: Simplified environment variable processing
    pub fn from_env() -> ConnectionConfig {
        if let Ok(buffer_size) = std::env::var("MSGTRANS_BUFFER_SIZE") {
            if let Ok(size) = buffer_size.parse::<usize>() {
                tracing::info!(
                    "[CONFIG] Environment variable specified buffer size: {}",
                    size
                );
                return ConnectionConfig::default().with_buffer_size(size);
            }
        }

        if let Ok(conn_type) = std::env::var("MSGTRANS_CONNECTION_TYPE") {
            match conn_type.to_lowercase().as_str() {
                "traditional" => {
                    tracing::warn!(
                        "[ALERT] Stage 3 notice: Traditional connections removed, using intelligent optimized lock-free connection"
                    );
                    return ConnectionConfig::auto_optimized();
                }
                "auto" => {
                    tracing::info!(
                        "[CONFIG] Environment variable specified intelligent optimization"
                    );
                    return ConnectionConfig::auto_optimized();
                }
                "high_performance" => {
                    tracing::info!("[CONFIG] Environment variable specified high performance mode");
                    return ConnectionConfig::high_performance();
                }
                "lockfree" => {
                    tracing::info!("[CONFIG] Environment variable specified lockfree mode");
                    return ConnectionConfig::auto_optimized();
                }
                _ => {
                    tracing::warn!(
                        "[ALERT] Unknown connection configuration: {}, using default configuration",
                        conn_type
                    );
                }
            }
        }

        ConnectionConfig::default()
    }

    /// Record performance metrics
    fn record_metrics(session_id: SessionId, metrics: ConnectionMetrics) {
        tracing::info!(
            "[METRICS] Connection creation metrics - session: {}, time: {:?}, buffer: {}",
            session_id,
            metrics.creation_time,
            metrics.buffer_size
        );
    }

    /// [STAGE3] Third stage: Get recommended configuration
    pub fn recommend_config() -> ConnectionConfig {
        let cpu_count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        match cpu_count {
            1..=2 => {
                tracing::debug!(
                    "[RECOMMEND] Stage 3: {} core CPU, recommending intelligent optimization mode",
                    cpu_count
                );
                ConnectionConfig::auto_optimized()
            }
            _ => {
                tracing::debug!(
                    "[RECOMMEND] Stage 3: {} core CPU, recommending high performance mode",
                    cpu_count
                );
                ConnectionConfig::high_performance()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auto_detect_from_env() {
        // Set environment variable
        std::env::set_var("MSGTRANS_CONNECTION_TYPE", "lockfree");
        let detected = ConnectionFactory::from_env();
        assert_eq!(detected.buffer_size, 1000);

        // Stage 3: Traditional connections automatically converted to lock-free connections
        std::env::set_var("MSGTRANS_CONNECTION_TYPE", "traditional");
        let detected = ConnectionFactory::from_env();
        assert_eq!(detected.buffer_size, 1000);

        std::env::set_var("MSGTRANS_CONNECTION_TYPE", "auto");
        let detected = ConnectionFactory::from_env();
        assert_eq!(detected.buffer_size, 1000);

        // Clean up environment variables
        std::env::remove_var("MSGTRANS_CONNECTION_TYPE");
    }

    #[test]
    fn test_recommend_config() {
        let recommended = ConnectionFactory::recommend_config();

        // Should recommend LockFree or Auto (depending on CPU core count)
        assert!(matches!(recommended.buffer_size, 2500 | 1000));
    }

    #[test]
    fn test_config_presets() {
        let high_perf = ConnectionConfig::high_performance();
        assert_eq!(high_perf.buffer_size, 2500);
        assert!(high_perf.enable_metrics);

        let auto_optimized = ConnectionConfig::auto_optimized();
        assert_eq!(auto_optimized.buffer_size, 1000);
        assert!(auto_optimized.enable_metrics);

        let silent = ConnectionConfig::silent();
        assert_eq!(silent.buffer_size, 1000);
        assert!(!silent.enable_metrics);
    }
}

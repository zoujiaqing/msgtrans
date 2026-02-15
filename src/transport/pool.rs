use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
/// Intelligent expansion mechanism - core implementation
///
/// Progressive resource expansion based on mathematical models:
/// - Early rapid expansion: 2.0x (1G→2G→4G)
/// - Mid-period moderate expansion: 1.5x (4G→6G)
/// - Late conservative expansion: 1.2x (6G→7.2G)
/// - Final fine expansion: 1.1x (7.2G→7.9G→8.7G)

/// [PERF] High-performance connection pool full optimization
///
/// Based on successful experience from previous phases, applying hybrid architecture strategy to connection pool:
/// - LockFree + Crossbeam: Synchronous high-frequency path (connection get/return)
/// - Flume: Asynchronous processing path (connection management commands)
/// - Tokio: Ecosystem integration path (event broadcasting)
use std::sync::Arc;
use std::time::{Duration, Instant};

use flume::{unbounded as flume_unbounded, Receiver as FlumeReceiver, Sender as FlumeSender};
use tokio::sync::RwLock;

use crate::error::TransportError;
use crate::transport::lockfree::{LockFreeHashMap, LockFreeQueue};
use crate::transport::memory_pool::{OptimizedMemoryPool, OptimizedMemoryStatsSnapshot};
use crate::SessionId;

/// [PERF] Optimized intelligent connection pool
pub struct ConnectionPool {
    /// Connection ID counter
    connection_id_counter: AtomicU64,

    /// [PERF] LockFree connection storage
    active_connections: Arc<LockFreeHashMap<ConnectionId, PoolConnection>>,
    available_connections: Arc<LockFreeQueue<ConnectionId>>,

    /// [SYNC] Crossbeam synchronous control
    pool_control_tx: FlumeSender<PoolControlCommand>,
    pool_control_rx: FlumeReceiver<PoolControlCommand>,

    /// [EVENT] Tokio event broadcasting
    pub event_broadcaster: tokio::sync::broadcast::Sender<PoolEvent>,

    /// Configuration and state
    max_size: usize,
    initial_size: usize,

    /// [PERF] Optimized statistics
    stats: Arc<OptimizedPoolStats>,
    /// Expansion strategy (maintain compatibility)
    expansion_strategy: ExpansionStrategy,
    /// Memory pool
    memory_pool: Arc<OptimizedMemoryPool>,
    /// Performance monitor
    monitor: Arc<PerformanceMonitor>,
}

/// Connection ID type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

impl ConnectionId {
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Connection in pool
#[derive(Debug, Clone)]
pub struct PoolConnection {
    pub id: ConnectionId,
    pub session_id: Option<SessionId>,
    pub created_at: Instant,
    pub last_used: Instant,
    pub use_count: u64,
    pub state: ConnectionState,
}

/// Connection state
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Available,
    InUse,
    Maintenance,
    Error,
}

/// [SYNC] Crossbeam control commands
#[derive(Debug)]
pub enum PoolControlCommand {
    GetConnection {
        response_tx: FlumeSender<Result<ConnectionId, TransportError>>,
    },
    ReturnConnection {
        connection_id: ConnectionId,
        response_tx: FlumeSender<Result<(), TransportError>>,
    },
    CreateConnection {
        count: usize,
        response_tx: FlumeSender<Result<Vec<ConnectionId>, TransportError>>,
    },
    RemoveConnection {
        connection_id: ConnectionId,
        response_tx: FlumeSender<Result<(), TransportError>>,
    },
    GetStats {
        response_tx: FlumeSender<OptimizedPoolStatsSnapshot>,
    },
}

/// [EVENT] Tokio event types
#[derive(Debug, Clone)]
pub enum PoolEvent {
    ConnectionCreated { connection_id: ConnectionId },
    ConnectionAcquired { connection_id: ConnectionId },
    ConnectionReleased { connection_id: ConnectionId },
    ConnectionRemoved { connection_id: ConnectionId },
    PoolExpanded { from_size: usize, to_size: usize },
    PoolShrunk { from_size: usize, to_size: usize },
    PoolError { error: String },
}

/// [PERF] Optimized pool statistics
#[derive(Debug, Default)]
pub struct OptimizedPoolStats {
    /// Total connections
    pub total_connections: AtomicU64,
    /// Active connections
    pub active_connections: AtomicU64,
    /// Available connections
    pub available_connections: AtomicU64,
    /// Get operation count
    pub get_operations: AtomicU64,
    /// Return operation count
    pub return_operations: AtomicU64,
    /// Create operation count
    pub create_operations: AtomicU64,
    /// Remove operation count
    pub remove_operations: AtomicU64,
    /// Wait time statistics (nanoseconds)
    pub total_wait_time_ns: AtomicU64,
    /// Total operations (for calculating average wait time)
    pub total_operations: AtomicU64,
}

impl OptimizedPoolStats {
    /// Get statistics snapshot
    pub fn snapshot(&self) -> OptimizedPoolStatsSnapshot {
        OptimizedPoolStatsSnapshot {
            total_connections: self.total_connections.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            available_connections: self.available_connections.load(Ordering::Relaxed),
            get_operations: self.get_operations.load(Ordering::Relaxed),
            return_operations: self.return_operations.load(Ordering::Relaxed),
            create_operations: self.create_operations.load(Ordering::Relaxed),
            remove_operations: self.remove_operations.load(Ordering::Relaxed),
            total_wait_time_ns: self.total_wait_time_ns.load(Ordering::Relaxed),
            total_operations: self.total_operations.load(Ordering::Relaxed),
        }
    }
}

/// Statistics snapshot (Clone-able)
#[derive(Debug, Clone)]
pub struct OptimizedPoolStatsSnapshot {
    pub total_connections: u64,
    pub active_connections: u64,
    pub available_connections: u64,
    pub get_operations: u64,
    pub return_operations: u64,
    pub create_operations: u64,
    pub remove_operations: u64,
    pub total_wait_time_ns: u64,
    pub total_operations: u64,
}

/// Expansion strategy
#[derive(Debug, Clone)]
pub struct ExpansionStrategy {
    /// Expansion factor sequence: [2.0, 1.5, 1.2, 1.1]
    pub factors: Vec<f64>,
    /// Current factor index
    pub current_factor_index: usize,
    /// Expansion threshold (utilization triggers expansion)
    pub expansion_threshold: f64,
    /// Shrink threshold (utilization triggers shrink)
    pub shrink_threshold: f64,
}

/// Connection pool statistics
#[derive(Debug)]
pub struct PoolStats {
    /// Total expansion count
    pub expansion_count: AtomicU64,
    /// Total shrink count
    pub shrink_count: AtomicU64,
    /// Last expansion time
    pub last_expansion: RwLock<Option<Instant>>,
    /// Last shrink time
    pub last_shrink: RwLock<Option<Instant>>,
    /// Historical utilization
    pub utilization_history: RwLock<VecDeque<f64>>,
}

impl Default for ExpansionStrategy {
    fn default() -> Self {
        Self {
            factors: vec![2.0, 1.5, 1.2, 1.1],
            current_factor_index: 0,
            expansion_threshold: 0.8, // 80% utilization triggers expansion
            shrink_threshold: 0.3,    // 30% utilization triggers shrink
        }
    }
}

impl Clone for ConnectionPool {
    fn clone(&self) -> Self {
        Self {
            connection_id_counter: AtomicU64::new(
                self.connection_id_counter.load(Ordering::Relaxed),
            ),
            active_connections: self.active_connections.clone(),
            available_connections: self.available_connections.clone(),
            pool_control_tx: self.pool_control_tx.clone(),
            pool_control_rx: self.pool_control_rx.clone(),
            event_broadcaster: self.event_broadcaster.clone(),
            max_size: self.max_size,
            initial_size: self.initial_size,
            stats: self.stats.clone(),
            expansion_strategy: self.expansion_strategy.clone(),
            memory_pool: self.memory_pool.clone(),
            monitor: self.monitor.clone(),
        }
    }
}

impl ConnectionPool {
    /// [PERF] Create optimized intelligent connection pool
    pub fn new(initial_size: usize, max_size: usize) -> Self {
        let (pool_control_tx, pool_control_rx) = flume_unbounded();
        let (event_broadcaster, _) = tokio::sync::broadcast::channel(8192);

        Self {
            connection_id_counter: AtomicU64::new(0),
            active_connections: Arc::new(LockFreeHashMap::new()),
            available_connections: Arc::new(LockFreeQueue::new()),
            pool_control_tx,
            pool_control_rx,
            event_broadcaster,
            max_size,
            initial_size,
            stats: Arc::new(OptimizedPoolStats::default()),
            expansion_strategy: ExpansionStrategy::default(),
            memory_pool: Arc::new(OptimizedMemoryPool::new()),
            monitor: Arc::new(PerformanceMonitor::new()),
        }
    }

    /// [PERF] Initialize connection pool (replaces with_lockfree_optimization)
    pub async fn initialize_pool(self) -> Result<Self, TransportError> {
        // Create initial connections
        for _ in 0..self.initial_size {
            let connection_id =
                ConnectionId::new(self.connection_id_counter.fetch_add(1, Ordering::Relaxed));
            let connection = PoolConnection {
                id: connection_id,
                session_id: None,
                created_at: Instant::now(),
                last_used: Instant::now(),
                use_count: 0,
                state: ConnectionState::Available,
            };

            // LockFree store connection
            if let Err(e) = self.active_connections.insert(connection_id, connection) {
                return Err(TransportError::config_error(
                    "initialize_pool",
                    format!("Failed to create connection: {:?}", e),
                ));
            }

            // Add to available queue
            if let Err(e) = self.available_connections.push(connection_id) {
                return Err(TransportError::config_error(
                    "initialize_pool",
                    format!("Failed to add available connection: {:?}", e),
                ));
            }

            // Update statistics
            self.stats.total_connections.fetch_add(1, Ordering::Relaxed);
            self.stats
                .available_connections
                .fetch_add(1, Ordering::Relaxed);
            self.stats.create_operations.fetch_add(1, Ordering::Relaxed);

            // Send creation event
            let _ = self
                .event_broadcaster
                .send(PoolEvent::ConnectionCreated { connection_id });
        }

        tracing::info!(
            "[PERF] Connection pool initialization completed - initial connections: {}",
            self.initial_size
        );
        Ok(self)
    }

    /// [PERF] Compatibility method - with_lockfree_optimization (for benchmarking)
    pub fn with_lockfree_optimization(self) -> Self {
        // LockFree optimization is enabled by default, this method is for compatibility only
        tracing::info!("[PERF] LockFree optimization is enabled by default");
        self
    }

    /// [PERF] High-performance connection acquisition (LockFree)
    pub fn get_connection(&self) -> Result<ConnectionId, TransportError> {
        let start_time = Instant::now();

        // Try to get connection from available queue
        if let Some(connection_id) = self.available_connections.pop() {
            // Update connection state to in use
            if let Some(mut connection) = self.active_connections.get(&connection_id) {
                connection.state = ConnectionState::InUse;
                connection.last_used = Instant::now();
                connection.use_count += 1;

                // Update connection to storage (LockFree)
                let _ = self.active_connections.insert(connection_id, connection);

                // Update statistics
                self.stats.get_operations.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .available_connections
                    .fetch_sub(1, Ordering::Relaxed);
                self.stats
                    .active_connections
                    .fetch_add(1, Ordering::Relaxed);

                let wait_time = start_time.elapsed().as_nanos() as u64;
                self.stats
                    .total_wait_time_ns
                    .fetch_add(wait_time, Ordering::Relaxed);
                self.stats.total_operations.fetch_add(1, Ordering::Relaxed);

                // Send acquisition event
                let _ = self
                    .event_broadcaster
                    .send(PoolEvent::ConnectionAcquired { connection_id });

                tracing::debug!(
                    "[PERF] Get connection: {:?}, wait time: {:?}",
                    connection_id,
                    start_time.elapsed()
                );
                return Ok(connection_id);
            }
        }

        Err(TransportError::resource_error(
            "connection_pool",
            0,
            self.max_size,
        ))
    }

    /// [PERF] High-performance connection return (LockFree)
    pub fn return_connection(&self, connection_id: ConnectionId) -> Result<(), TransportError> {
        // Check if connection exists
        if let Some(mut connection) = self.active_connections.get(&connection_id) {
            // Update connection state to available
            connection.state = ConnectionState::Available;
            connection.last_used = Instant::now();

            // Update connection to storage (LockFree)
            let _ = self.active_connections.insert(connection_id, connection);

            // Add back to available queue
            if let Err(e) = self.available_connections.push(connection_id) {
                return Err(TransportError::config_error(
                    "return_connection",
                    format!("Failed to return connection: {:?}", e),
                ));
            }

            // Update statistics
            self.stats.return_operations.fetch_add(1, Ordering::Relaxed);
            self.stats
                .available_connections
                .fetch_add(1, Ordering::Relaxed);
            self.stats
                .active_connections
                .fetch_sub(1, Ordering::Relaxed);

            // Send return event
            let _ = self
                .event_broadcaster
                .send(PoolEvent::ConnectionReleased { connection_id });

            tracing::debug!("[PERF] Return connection: {:?}", connection_id);
            Ok(())
        } else {
            Err(TransportError::config_error(
                "return_connection",
                format!("Connection not found: {:?}", connection_id),
            ))
        }
    }

    /// [PERF] Get current utilization (LockFree)
    pub fn utilization(&self) -> f64 {
        let total = self.stats.total_connections.load(Ordering::Relaxed) as f64;
        let available = self.stats.available_connections.load(Ordering::Relaxed) as f64;

        if total == 0.0 {
            0.0
        } else {
            (total - available) / total
        }
    }

    /// [PERF] Intelligent expansion (based on LockFree statistics)
    pub async fn smart_expand(&mut self) -> Result<bool, TransportError> {
        // Get current statistics
        let current_total = self.stats.total_connections.load(Ordering::Relaxed) as usize;
        let available_count = self.stats.available_connections.load(Ordering::Relaxed) as usize;

        // Calculate utilization
        let utilization = if current_total > 0 {
            (current_total - available_count) as f64 / current_total as f64
        } else {
            0.0
        };

        // Check if expansion is needed
        if utilization < self.expansion_strategy.expansion_threshold {
            return Ok(false); // No expansion needed
        }

        // Get current expansion factor
        let factor = self.get_current_expansion_factor();
        let new_size = ((current_total as f64) * factor) as usize;

        // Check if exceeding maximum limit
        if new_size > self.max_size {
            return Err(TransportError::resource_error(
                "connection_pool_expansion",
                new_size,
                self.max_size,
            ));
        }

        // Create new connections
        let connections_to_create = new_size - current_total;
        for _ in 0..connections_to_create {
            let connection_id =
                ConnectionId::new(self.connection_id_counter.fetch_add(1, Ordering::Relaxed));
            let connection = PoolConnection {
                id: connection_id,
                session_id: None,
                created_at: Instant::now(),
                last_used: Instant::now(),
                use_count: 0,
                state: ConnectionState::Available,
            };

            // LockFree store connection
            if let Err(e) = self.active_connections.insert(connection_id, connection) {
                tracing::error!("[ERROR] Failed to create connection: {:?}", e);
                continue;
            }

            // Add to available queue
            if let Err(e) = self.available_connections.push(connection_id) {
                tracing::error!("[ERROR] Failed to add available connection: {:?}", e);
                continue;
            }

            // Update statistics
            self.stats.total_connections.fetch_add(1, Ordering::Relaxed);
            self.stats
                .available_connections
                .fetch_add(1, Ordering::Relaxed);
            self.stats.create_operations.fetch_add(1, Ordering::Relaxed);

            // Send creation event
            let _ = self
                .event_broadcaster
                .send(PoolEvent::ConnectionCreated { connection_id });
        }

        // Update expansion factor index
        self.advance_expansion_factor();

        // Record performance metrics
        self.monitor
            .record_expansion(current_total, new_size, factor)
            .await;

        // Send expansion event
        let _ = self.event_broadcaster.send(PoolEvent::PoolExpanded {
            from_size: current_total,
            to_size: new_size,
        });

        tracing::info!(
            "[PERF] Connection pool expanded: {} -> {} (factor: {:.1}x), utilization: {:.1}%",
            current_total,
            new_size,
            factor,
            utilization * 100.0
        );

        Ok(true)
    }

    /// [PERF] Intelligent expansion decision
    pub async fn try_expand(&mut self) -> Result<bool, TransportError> {
        self.smart_expand().await
    }

    /// [PERF] Force expansion (for testing and demonstration)
    pub async fn force_expand(&mut self) -> Result<bool, TransportError> {
        // Temporarily set expansion threshold to 0, force expansion
        let original_threshold = self.expansion_strategy.expansion_threshold;
        self.expansion_strategy.expansion_threshold = 0.0;

        let result = self.smart_expand().await;

        // Restore original threshold
        self.expansion_strategy.expansion_threshold = original_threshold;

        result
    }

    /// [PERF] Intelligent shrink decision (based on LockFree statistics)
    pub async fn try_shrink(&mut self) -> Result<bool, TransportError> {
        let utilization = self.utilization();

        // Check if shrink is needed
        if utilization > self.expansion_strategy.shrink_threshold {
            return Ok(false);
        }

        // Get current statistics
        let current_total = self.stats.total_connections.load(Ordering::Relaxed) as usize;
        let available_count = self.stats.available_connections.load(Ordering::Relaxed) as usize;

        // Maintain minimum size
        let min_size = self.initial_size;
        if current_total <= min_size {
            return Ok(false);
        }

        // Progressive shrink (reverse factor)
        let shrink_factor = 0.8; // Shrink to 80%
        let new_size = std::cmp::max(((current_total as f64) * shrink_factor) as usize, min_size);

        if new_size >= current_total {
            return Ok(false);
        }

        // Calculate connections to remove
        let connections_to_remove = current_total - new_size;
        let mut removed_count = 0;

        // Only remove available connections
        for _ in 0..std::cmp::min(connections_to_remove, available_count) {
            if let Some(connection_id) = self.available_connections.pop() {
                // Remove from storage
                if let Err(e) = self.active_connections.remove(&connection_id) {
                    tracing::warn!("[WARNING] Failed to remove connection: {:?}", e);
                    continue;
                }

                // Update statistics
                self.stats.total_connections.fetch_sub(1, Ordering::Relaxed);
                self.stats
                    .available_connections
                    .fetch_sub(1, Ordering::Relaxed);
                self.stats.remove_operations.fetch_add(1, Ordering::Relaxed);

                // Send removal event
                let _ = self
                    .event_broadcaster
                    .send(PoolEvent::ConnectionRemoved { connection_id });

                removed_count += 1;
            }
        }

        let final_size = current_total - removed_count;

        // Record performance metrics
        self.monitor
            .record_shrink(current_total, final_size, shrink_factor)
            .await;

        // Send shrink event
        let _ = self.event_broadcaster.send(PoolEvent::PoolShrunk {
            from_size: current_total,
            to_size: final_size,
        });

        tracing::info!(
            "[PERF] Connection pool shrunk: {} -> {} (removed {} connections, utilization: {:.1}%)",
            current_total,
            final_size,
            removed_count,
            utilization * 100.0
        );

        Ok(removed_count > 0)
    }

    /// Get current expansion factor
    fn get_current_expansion_factor(&self) -> f64 {
        let index = std::cmp::min(
            self.expansion_strategy.current_factor_index,
            self.expansion_strategy.factors.len() - 1,
        );
        self.expansion_strategy.factors[index]
    }

    /// Advance expansion factor index
    fn advance_expansion_factor(&mut self) {
        if self.expansion_strategy.current_factor_index < self.expansion_strategy.factors.len() - 1
        {
            self.expansion_strategy.current_factor_index += 1;
        }
    }

    /// [PERF] Get detailed status (based on LockFree architecture)
    pub async fn detailed_status(&self) -> PoolDetailedStatus {
        let stats = self.stats.snapshot();

        PoolDetailedStatus {
            current_size: stats.total_connections as usize,
            max_size: self.max_size,
            utilization: self.utilization(),
            expansion_count: 0, // Temporarily disabled
            shrink_count: 0,    // Temporarily disabled
            current_expansion_factor: self.get_current_expansion_factor(),
            avg_utilization: if stats.total_operations > 0 {
                stats.active_connections as f64 / stats.total_connections as f64
            } else {
                0.0
            },
            memory_pool_status: OptimizedMemoryStatsSnapshot {
                small_get_operations: 0,
                medium_get_operations: 0,
                large_get_operations: 0,
                small_return_operations: 0,
                medium_return_operations: 0,
                large_return_operations: 0,
                small_allocated: 0,
                medium_allocated: 0,
                large_allocated: 0,
                small_cached: 0,
                medium_cached: 0,
                large_cached: 0,
                total_operations: 0,
                cache_hit_rate: 0.0,
                cache_miss_rate: 0.0,
                total_memory_allocated_mb: 0.0,
                total_memory_cached_mb: 0.0,
                memory_efficiency: 0.0,
            },
        }
    }

    /// [PERF] Get performance statistics
    pub fn get_performance_stats(&self) -> OptimizedPoolStatsSnapshot {
        self.stats.snapshot()
    }

    /// Get memory pool reference
    pub fn memory_pool(&self) -> Arc<OptimizedMemoryPool> {
        self.memory_pool.clone()
    }
}

/// Detailed pool status
#[derive(Debug, Clone)]
pub struct PoolDetailedStatus {
    pub current_size: usize,
    pub max_size: usize,
    pub utilization: f64,
    pub expansion_count: u64,
    pub shrink_count: u64,
    pub current_expansion_factor: f64,
    pub avg_utilization: f64,
    pub memory_pool_status: OptimizedMemoryStatsSnapshot,
}

impl PoolStats {
    fn new() -> Self {
        Self {
            expansion_count: AtomicU64::new(0),
            shrink_count: AtomicU64::new(0),
            last_expansion: RwLock::new(None),
            last_shrink: RwLock::new(None),
            utilization_history: RwLock::new(VecDeque::with_capacity(100)),
        }
    }
}

// MemoryPool has been migrated to OptimizedMemoryPool in memory_pool.rs
// Through the alias system in mod.rs, users using MemoryPool will automatically use OptimizedMemoryPool
// Legacy version is preserved in the legacy module

// OptimizedMemoryStats, OptimizedMemoryStatsSnapshot and MemoryPoolEvent
// have been migrated to memory_pool.rs, users use them automatically through the alias system

// OptimizedMemoryStats implementation has been migrated to memory_pool.rs

/// Performance monitor
pub struct PerformanceMonitor {
    /// Expansion event history
    expansion_events: RwLock<VecDeque<ExpansionEvent>>,
    /// Shrink event history
    shrink_events: RwLock<VecDeque<ShrinkEvent>>,
    /// Performance metrics
    metrics: RwLock<PerformanceMetrics>,
}

/// Expansion event
#[derive(Debug, Clone)]
struct ExpansionEvent {
    timestamp: Instant,
    from_size: usize,
    to_size: usize,
    factor: f64,
}

/// Shrink event
#[derive(Debug, Clone)]
struct ShrinkEvent {
    timestamp: Instant,
    from_size: usize,
    to_size: usize,
    factor: f64,
}

/// Performance metrics
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub avg_expansion_factor: f64,
    pub expansion_frequency: f64, // Expansions per hour
    pub shrink_frequency: f64,    // Shrinks per hour
    pub memory_efficiency: f64,   // Memory usage efficiency
}

impl PerformanceMonitor {
    pub fn new() -> Self {
        Self {
            expansion_events: RwLock::new(VecDeque::with_capacity(1000)),
            shrink_events: RwLock::new(VecDeque::with_capacity(1000)),
            metrics: RwLock::new(PerformanceMetrics::default()),
        }
    }

    /// Record expansion event
    pub async fn record_expansion(&self, from_size: usize, to_size: usize, factor: f64) {
        let event = ExpansionEvent {
            timestamp: Instant::now(),
            from_size,
            to_size,
            factor,
        };

        {
            let mut events = self.expansion_events.write().await;
            events.push_back(event);

            // Keep last 1000 events
            if events.len() > 1000 {
                events.pop_front();
            }
        } // Release write lock

        // Note: Temporarily remove update_metrics call to avoid deadlock
        // TODO: Optimize metrics update mechanism in future versions
    }

    /// Record shrink event
    pub async fn record_shrink(&self, from_size: usize, to_size: usize, factor: f64) {
        let event = ShrinkEvent {
            timestamp: Instant::now(),
            from_size,
            to_size,
            factor,
        };

        {
            let mut events = self.shrink_events.write().await;
            events.push_back(event);

            // Keep last 1000 events
            if events.len() > 1000 {
                events.pop_front();
            }
        } // Release write lock

        // Note: Temporarily remove update_metrics call to avoid deadlock
        // TODO: Optimize metrics update mechanism in future versions
    }

    /// Update performance metrics
    async fn update_metrics(&self) {
        let expansion_events = self.expansion_events.read().await;
        let shrink_events = self.shrink_events.read().await;

        // Calculate average expansion factor
        let avg_expansion_factor = if expansion_events.is_empty() {
            1.0
        } else {
            expansion_events.iter().map(|e| e.factor).sum::<f64>() / expansion_events.len() as f64
        };

        // Calculate frequency (based on recent 1 hour)
        let one_hour_ago = Instant::now() - Duration::from_secs(3600);

        let recent_expansions = expansion_events
            .iter()
            .filter(|e| e.timestamp > one_hour_ago)
            .count() as f64;

        let recent_shrinks = shrink_events
            .iter()
            .filter(|e| e.timestamp > one_hour_ago)
            .count() as f64;

        let mut metrics = self.metrics.write().await;
        metrics.avg_expansion_factor = avg_expansion_factor;
        metrics.expansion_frequency = recent_expansions;
        metrics.shrink_frequency = recent_shrinks;
        metrics.memory_efficiency = 0.85; // TODO: Calculate actual memory efficiency
    }

    /// Get performance metrics
    pub async fn get_metrics(&self) -> PerformanceMetrics {
        self.metrics.read().await.clone()
    }
}

impl Default for PerformanceMetrics {
    fn default() -> Self {
        Self {
            avg_expansion_factor: 1.0,
            expansion_frequency: 0.0,
            shrink_frequency: 0.0,
            memory_efficiency: 1.0,
        }
    }
}

use bytes::BytesMut;
/// [MEMORY] Fully lock-free memory pool implementation
///
/// Optimization features:
/// - Complete removal of RwLock, using lock-free data structures
/// - Synchronous API to avoid async overhead
/// - Intelligent cache management and adaptive adjustment
/// - Zero-copy buffer reuse
/// - Real-time performance monitoring and event broadcasting
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc,
};

use crate::transport::lockfree::LockFreeQueue;

/// [OPTIMIZED] Fully lock-free memory pool
#[derive(Clone)]
pub struct OptimizedMemoryPool {
    /// [LOCKFREE] Buffer queues replacing RwLock
    small_buffers: Arc<LockFreeQueue<BytesMut>>,
    medium_buffers: Arc<LockFreeQueue<BytesMut>>,
    large_buffers: Arc<LockFreeQueue<BytesMut>>,

    /// [STATS] Optimized statistics
    stats: Arc<OptimizedMemoryStats>,

    /// [CONFIG] Lock-free configuration
    small_max_cached: Arc<AtomicUsize>, // Maximum cached small buffers
    medium_max_cached: Arc<AtomicUsize>, // Maximum cached medium buffers
    large_max_cached: Arc<AtomicUsize>,  // Maximum cached large buffers

    /// [EVENT] Event broadcaster
    event_broadcaster: tokio::sync::broadcast::Sender<MemoryPoolEvent>,
}

/// [OPTIMIZED] Memory pool statistics
#[derive(Debug, Default)]
pub struct OptimizedMemoryStats {
    /// Buffer operation statistics
    pub small_get_operations: AtomicU64,
    pub medium_get_operations: AtomicU64,
    pub large_get_operations: AtomicU64,
    pub small_return_operations: AtomicU64,
    pub medium_return_operations: AtomicU64,
    pub large_return_operations: AtomicU64,

    /// Buffer allocation statistics
    pub small_allocated: AtomicU64,
    pub medium_allocated: AtomicU64,
    pub large_allocated: AtomicU64,
    pub small_cached: AtomicU64,
    pub medium_cached: AtomicU64,
    pub large_cached: AtomicU64,

    /// Performance statistics
    pub total_get_operations: AtomicU64,
    pub total_return_operations: AtomicU64,
    pub cache_hit_count: AtomicU64,
    pub cache_miss_count: AtomicU64,

    /// Memory statistics (bytes)
    pub total_memory_allocated: AtomicU64,
    pub total_memory_cached: AtomicU64,
}

/// Memory pool statistics snapshot
#[derive(Debug, Clone)]
pub struct OptimizedMemoryStatsSnapshot {
    // Operation statistics
    pub small_get_operations: u64,
    pub medium_get_operations: u64,
    pub large_get_operations: u64,
    pub small_return_operations: u64,
    pub medium_return_operations: u64,
    pub large_return_operations: u64,

    // Allocation statistics
    pub small_allocated: u64,
    pub medium_allocated: u64,
    pub large_allocated: u64,
    pub small_cached: u64,
    pub medium_cached: u64,
    pub large_cached: u64,

    // Performance statistics
    pub total_operations: u64,
    pub cache_hit_rate: f64,
    pub cache_miss_rate: f64,

    // Memory statistics
    pub total_memory_allocated_mb: f64,
    pub total_memory_cached_mb: f64,
    pub memory_efficiency: f64,
}

/// [EVENT] Memory pool events
#[derive(Debug, Clone)]
pub enum MemoryPoolEvent {
    BufferAllocated { size: BufferSize, capacity: usize },
    BufferReturned { size: BufferSize, capacity: usize },
    CacheHit { size: BufferSize },
    CacheMiss { size: BufferSize },
    CacheEviction { size: BufferSize, count: usize },
    MemoryPressure { total_mb: f64, threshold_mb: f64 },
}

/// Buffer size enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSize {
    Small,  // 1KB
    Medium, // 8KB
    Large,  // 64KB
}

impl BufferSize {
    /// Get buffer capacity
    pub const fn capacity(self) -> usize {
        match self {
            BufferSize::Small => 1024,
            BufferSize::Medium => 8192,
            BufferSize::Large => 65536,
        }
    }

    /// Get buffer description
    pub const fn description(self) -> &'static str {
        match self {
            BufferSize::Small => "Small(1KB)",
            BufferSize::Medium => "Medium(8KB)",
            BufferSize::Large => "Large(64KB)",
        }
    }
}

impl OptimizedMemoryStats {
    /// Get statistics snapshot
    pub fn snapshot(&self) -> OptimizedMemoryStatsSnapshot {
        let small_get = self.small_get_operations.load(Ordering::Relaxed);
        let medium_get = self.medium_get_operations.load(Ordering::Relaxed);
        let large_get = self.large_get_operations.load(Ordering::Relaxed);
        let small_return = self.small_return_operations.load(Ordering::Relaxed);
        let medium_return = self.medium_return_operations.load(Ordering::Relaxed);
        let large_return = self.large_return_operations.load(Ordering::Relaxed);

        let total_operations =
            small_get + medium_get + large_get + small_return + medium_return + large_return;
        let cache_hits = self.cache_hit_count.load(Ordering::Relaxed);
        let cache_misses = self.cache_miss_count.load(Ordering::Relaxed);

        let cache_hit_rate = if cache_hits + cache_misses > 0 {
            cache_hits as f64 / (cache_hits + cache_misses) as f64
        } else {
            0.0
        };

        let total_memory_allocated = self.total_memory_allocated.load(Ordering::Relaxed);
        let total_memory_cached = self.total_memory_cached.load(Ordering::Relaxed);
        let memory_efficiency = if total_memory_allocated > 0 {
            total_memory_cached as f64 / total_memory_allocated as f64
        } else {
            0.0
        };

        OptimizedMemoryStatsSnapshot {
            small_get_operations: small_get,
            medium_get_operations: medium_get,
            large_get_operations: large_get,
            small_return_operations: small_return,
            medium_return_operations: medium_return,
            large_return_operations: large_return,

            small_allocated: self.small_allocated.load(Ordering::Relaxed),
            medium_allocated: self.medium_allocated.load(Ordering::Relaxed),
            large_allocated: self.large_allocated.load(Ordering::Relaxed),
            small_cached: self.small_cached.load(Ordering::Relaxed),
            medium_cached: self.medium_cached.load(Ordering::Relaxed),
            large_cached: self.large_cached.load(Ordering::Relaxed),

            total_operations,
            cache_hit_rate,
            cache_miss_rate: 1.0 - cache_hit_rate,

            total_memory_allocated_mb: total_memory_allocated as f64 / (1024.0 * 1024.0),
            total_memory_cached_mb: total_memory_cached as f64 / (1024.0 * 1024.0),
            memory_efficiency,
        }
    }
}

impl OptimizedMemoryPool {
    /// [PERF] Create fully lock-free memory pool
    pub fn new() -> Self {
        let (event_broadcaster, _) = tokio::sync::broadcast::channel(8192);

        Self {
            small_buffers: Arc::new(LockFreeQueue::new()),
            medium_buffers: Arc::new(LockFreeQueue::new()),
            large_buffers: Arc::new(LockFreeQueue::new()),
            stats: Arc::new(OptimizedMemoryStats::default()),
            small_max_cached: Arc::new(AtomicUsize::new(500)), // Maximum small buffer cache
            medium_max_cached: Arc::new(AtomicUsize::new(200)), // Maximum medium buffer cache
            large_max_cached: Arc::new(AtomicUsize::new(50)),  // Maximum large buffer cache
            event_broadcaster,
        }
    }

    /// [PERF] Pre-allocate buffer pool
    pub fn with_preallocation(
        self,
        small_count: usize,
        medium_count: usize,
        large_count: usize,
    ) -> Self {
        // Pre-allocate small buffers
        for _ in 0..small_count {
            let buffer = BytesMut::with_capacity(BufferSize::Small.capacity());
            let _ = self.small_buffers.push(buffer);
            self.stats.small_cached.fetch_add(1, Ordering::Relaxed);
            self.stats
                .total_memory_cached
                .fetch_add(BufferSize::Small.capacity() as u64, Ordering::Relaxed);
        }

        // Pre-allocate medium buffers
        for _ in 0..medium_count {
            let buffer = BytesMut::with_capacity(BufferSize::Medium.capacity());
            let _ = self.medium_buffers.push(buffer);
            self.stats.medium_cached.fetch_add(1, Ordering::Relaxed);
            self.stats
                .total_memory_cached
                .fetch_add(BufferSize::Medium.capacity() as u64, Ordering::Relaxed);
        }

        // Pre-allocate large buffers
        for _ in 0..large_count {
            let buffer = BytesMut::with_capacity(BufferSize::Large.capacity());
            let _ = self.large_buffers.push(buffer);
            self.stats.large_cached.fetch_add(1, Ordering::Relaxed);
            self.stats
                .total_memory_cached
                .fetch_add(BufferSize::Large.capacity() as u64, Ordering::Relaxed);
        }

        tracing::info!(
            "[PERF] Memory pool pre-allocation completed - Small:{}, Medium:{}, Large:{}",
            small_count,
            medium_count,
            large_count
        );

        self
    }

    /// [PERF] Synchronous buffer acquisition (LockFree + Zero-Copy)
    pub fn get_buffer(&self, size: BufferSize) -> BytesMut {
        let (queue, get_stat, cached_stat, alloc_stat) = match size {
            BufferSize::Small => (
                &self.small_buffers,
                &self.stats.small_get_operations,
                &self.stats.small_cached,
                &self.stats.small_allocated,
            ),
            BufferSize::Medium => (
                &self.medium_buffers,
                &self.stats.medium_get_operations,
                &self.stats.medium_cached,
                &self.stats.medium_allocated,
            ),
            BufferSize::Large => (
                &self.large_buffers,
                &self.stats.large_get_operations,
                &self.stats.large_cached,
                &self.stats.large_allocated,
            ),
        };

        // Update operation statistics
        get_stat.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_get_operations
            .fetch_add(1, Ordering::Relaxed);

        // Try to get from cache
        if let Some(mut buffer) = queue.pop() {
            // Cache hit
            cached_stat.fetch_sub(1, Ordering::Relaxed);
            self.stats.cache_hit_count.fetch_add(1, Ordering::Relaxed);
            self.stats
                .total_memory_cached
                .fetch_sub(size.capacity() as u64, Ordering::Relaxed);

            // Clear buffer to ensure zero-copy
            buffer.clear();

            // Send cache hit event
            let _ = self
                .event_broadcaster
                .send(MemoryPoolEvent::CacheHit { size });

            tracing::trace!(
                "[TARGET] Cache hit: {} capacity={}",
                size.description(),
                buffer.capacity()
            );
            return buffer;
        }

        // Cache miss, create new buffer
        let capacity = size.capacity();
        let buffer = BytesMut::with_capacity(capacity);

        // Update statistics
        alloc_stat.fetch_add(1, Ordering::Relaxed);
        self.stats.cache_miss_count.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_memory_allocated
            .fetch_add(capacity as u64, Ordering::Relaxed);

        // Send events
        let _ = self
            .event_broadcaster
            .send(MemoryPoolEvent::CacheMiss { size });
        let _ = self
            .event_broadcaster
            .send(MemoryPoolEvent::BufferAllocated { size, capacity });

        tracing::trace!(
            "[NEW] New allocation: {} capacity={}",
            size.description(),
            capacity
        );
        buffer
    }

    /// [PERF] Synchronous buffer return (LockFree + intelligent cache management)
    pub fn return_buffer(&self, buffer: BytesMut, size: BufferSize) {
        // Validate buffer size reasonableness
        if buffer.capacity() == 0 || buffer.capacity() > 10 * 1024 * 1024 {
            // Reject buffers over 10MB
            tracing::warn!(
                "[REJECT] Rejecting abnormal buffer: capacity={}",
                buffer.capacity()
            );
            return;
        }

        let (queue, return_stat, cached_stat, max_cached) = match size {
            BufferSize::Small => (
                &self.small_buffers,
                &self.stats.small_return_operations,
                &self.stats.small_cached,
                &self.small_max_cached,
            ),
            BufferSize::Medium => (
                &self.medium_buffers,
                &self.stats.medium_return_operations,
                &self.stats.medium_cached,
                &self.medium_max_cached,
            ),
            BufferSize::Large => (
                &self.large_buffers,
                &self.stats.large_return_operations,
                &self.stats.large_cached,
                &self.large_max_cached,
            ),
        };

        // Update operation statistics
        return_stat.fetch_add(1, Ordering::Relaxed);
        self.stats
            .total_return_operations
            .fetch_add(1, Ordering::Relaxed);

        // Check cache limits
        let current_cached = cached_stat.load(Ordering::Relaxed);
        let max_limit = max_cached.load(Ordering::Relaxed) as u64;

        if current_cached >= max_limit {
            // Cache is full, discard directly
            tracing::trace!(
                "[DROP] Cache full, dropping {} buffer (current={}/max={})",
                size.description(),
                current_cached,
                max_limit
            );
            return;
        }

        // Try to return to cache
        match queue.push(buffer) {
            Ok(()) => {
                // Successfully cached
                cached_stat.fetch_add(1, Ordering::Relaxed);
                self.stats
                    .total_memory_cached
                    .fetch_add(size.capacity() as u64, Ordering::Relaxed);

                // Send return event
                let _ = self
                    .event_broadcaster
                    .send(MemoryPoolEvent::BufferReturned {
                        size,
                        capacity: size.capacity(),
                    });

                tracing::trace!(
                    "[RECYCLE] Buffer returned: {} cache_count={}",
                    size.description(),
                    current_cached + 1
                );
            }
            Err(_) => {
                // Queue operation failed (extremely rare)
                tracing::warn!("[WARNING] Buffer return failed: {}", size.description());
            }
        }
    }

    /// [PERF] Adaptive cache adjustment
    pub fn adjust_cache_limits(&self, memory_pressure_threshold_mb: f64) {
        let stats = self.stats.snapshot();
        let current_memory_mb = stats.total_memory_cached_mb;

        if current_memory_mb > memory_pressure_threshold_mb {
            // Memory pressure too high, reduce cache limits
            let reduction_factor = 0.8;

            self.small_max_cached.store(
                ((self.small_max_cached.load(Ordering::Relaxed) as f64) * reduction_factor)
                    as usize,
                Ordering::Relaxed,
            );
            self.medium_max_cached.store(
                ((self.medium_max_cached.load(Ordering::Relaxed) as f64) * reduction_factor)
                    as usize,
                Ordering::Relaxed,
            );
            self.large_max_cached.store(
                ((self.large_max_cached.load(Ordering::Relaxed) as f64) * reduction_factor)
                    as usize,
                Ordering::Relaxed,
            );

            // Send memory pressure event
            let _ = self
                .event_broadcaster
                .send(MemoryPoolEvent::MemoryPressure {
                    total_mb: current_memory_mb,
                    threshold_mb: memory_pressure_threshold_mb,
                });

            tracing::info!("[REDUCE] Memory pressure adjustment: cache limits reduced to 80% (current={:.1}MB)", current_memory_mb);
        }
    }

    /// [PERF] Get memory pool performance statistics
    pub fn get_stats(&self) -> OptimizedMemoryStatsSnapshot {
        self.stats.snapshot()
    }

    /// [PERF] Clear cache (for low memory situations)
    pub fn clear_cache(&self) -> usize {
        let mut cleared_count = 0;
        let mut cleared_memory = 0u64;

        // Clear small buffer cache
        while let Some(_) = self.small_buffers.pop() {
            cleared_count += 1;
            cleared_memory += BufferSize::Small.capacity() as u64;
            self.stats.small_cached.fetch_sub(1, Ordering::Relaxed);
        }

        // Clear medium buffer cache
        while let Some(_) = self.medium_buffers.pop() {
            cleared_count += 1;
            cleared_memory += BufferSize::Medium.capacity() as u64;
            self.stats.medium_cached.fetch_sub(1, Ordering::Relaxed);
        }

        // Clear large buffer cache
        while let Some(_) = self.large_buffers.pop() {
            cleared_count += 1;
            cleared_memory += BufferSize::Large.capacity() as u64;
            self.stats.large_cached.fetch_sub(1, Ordering::Relaxed);
        }

        // Update total memory statistics
        self.stats.total_memory_cached.store(0, Ordering::Relaxed);

        tracing::info!(
            "[CLEAN] Cache cleared: {} buffers, {:.1}MB",
            cleared_count,
            cleared_memory as f64 / (1024.0 * 1024.0)
        );

        cleared_count
    }

    /// [PERF] Get event receiver (for monitoring)
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<MemoryPoolEvent> {
        self.event_broadcaster.subscribe()
    }

    /// [PERF] Get memory pool status (compatible with old API)
    pub async fn status(&self) -> OptimizedMemoryStatsSnapshot {
        self.get_stats()
    }
}

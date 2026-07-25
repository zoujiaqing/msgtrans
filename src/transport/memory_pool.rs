use bytes::BytesMut;
use std::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, OnceLock,
};

use crate::transport::lockfree::LockFreeQueue;

static SHARED_MEMORY_POOL: OnceLock<Arc<OptimizedMemoryPool>> = OnceLock::new();

/// Get the globally shared memory pool (lazy-initializes a default if none was registered).
pub fn shared_memory_pool() -> Arc<OptimizedMemoryPool> {
    SHARED_MEMORY_POOL
        .get_or_init(|| Arc::new(OptimizedMemoryPool::new()))
        .clone()
}

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
        Self {
            small_buffers: Arc::new(LockFreeQueue::new()),
            medium_buffers: Arc::new(LockFreeQueue::new()),
            large_buffers: Arc::new(LockFreeQueue::new()),
            stats: Arc::new(OptimizedMemoryStats::default()),
            small_max_cached: Arc::new(AtomicUsize::new(500)), // Maximum small buffer cache
            medium_max_cached: Arc::new(AtomicUsize::new(200)), // Maximum medium buffer cache
            large_max_cached: Arc::new(AtomicUsize::new(50)),  // Maximum large buffer cache
        }
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

    /// [PERF] Get memory pool performance statistics
    pub fn get_stats(&self) -> OptimizedMemoryStatsSnapshot {
        self.stats.snapshot()
    }
}

impl Default for OptimizedMemoryPool {
    fn default() -> Self {
        Self::new()
    }
}

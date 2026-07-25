use bytes::BytesMut;
use std::sync::{Arc, OnceLock};

use crate::transport::lockfree::LockFreeQueue;

static SHARED_MEMORY_POOL: OnceLock<Arc<OptimizedMemoryPool>> = OnceLock::new();

/// Get the globally shared memory pool (lazy-initializes a default if none was registered).
pub fn shared_memory_pool() -> Arc<OptimizedMemoryPool> {
    SHARED_MEMORY_POOL
        .get_or_init(|| Arc::new(OptimizedMemoryPool::new()))
        .clone()
}

/// Lock-free read-buffer pool for the byte-stream adapters (TCP/QUIC).
///
/// A per-tier lock-free queue caches reusable `BytesMut` buffers so the read
/// loop avoids re-allocating on every frame. 2.0 removed the per-operation
/// statistics (get/return counters, hit-rate and running-total atomics on the
/// hot path): nothing read them, so the buffer path no longer pays for metrics
/// nobody consumes. The cache is bounded purely by the queue length.
#[derive(Clone)]
pub struct OptimizedMemoryPool {
    small_buffers: Arc<LockFreeQueue<BytesMut>>,
    medium_buffers: Arc<LockFreeQueue<BytesMut>>,
    large_buffers: Arc<LockFreeQueue<BytesMut>>,
}

/// Buffer size tier. The three tiers exist for TCP's adaptive read sizing;
/// QUIC only ever asks for `Large`, so the smaller tiers are unused in a
/// QUIC-only build.
#[cfg_attr(not(feature = "tcp"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSize {
    Small,  // 1KB
    Medium, // 8KB
    Large,  // 64KB
}

impl BufferSize {
    /// Get buffer capacity.
    pub const fn capacity(self) -> usize {
        match self {
            BufferSize::Small => 1024,
            BufferSize::Medium => 8192,
            BufferSize::Large => 65536,
        }
    }

    /// Get buffer description.
    pub const fn description(self) -> &'static str {
        match self {
            BufferSize::Small => "Small(1KB)",
            BufferSize::Medium => "Medium(8KB)",
            BufferSize::Large => "Large(64KB)",
        }
    }

    /// Maximum number of buffers kept cached for this tier.
    const fn max_cached(self) -> usize {
        match self {
            BufferSize::Small => 500,
            BufferSize::Medium => 200,
            BufferSize::Large => 50,
        }
    }
}

impl OptimizedMemoryPool {
    /// Create a fully lock-free memory pool.
    pub fn new() -> Self {
        Self {
            small_buffers: Arc::new(LockFreeQueue::new()),
            medium_buffers: Arc::new(LockFreeQueue::new()),
            large_buffers: Arc::new(LockFreeQueue::new()),
        }
    }

    fn queue(&self, size: BufferSize) -> &LockFreeQueue<BytesMut> {
        match size {
            BufferSize::Small => &self.small_buffers,
            BufferSize::Medium => &self.medium_buffers,
            BufferSize::Large => &self.large_buffers,
        }
    }

    /// Acquire a buffer for `size`: reuse a cached one if available, otherwise
    /// allocate. Returned buffers are cleared, so callers always see an empty
    /// buffer with capacity `>= size.capacity()`.
    pub fn get_buffer(&self, size: BufferSize) -> BytesMut {
        if let Some(mut buffer) = self.queue(size).pop() {
            buffer.clear();
            tracing::trace!(
                "[TARGET] Cache hit: {} capacity={}",
                size.description(),
                buffer.capacity()
            );
            return buffer;
        }

        let capacity = size.capacity();
        tracing::trace!(
            "[NEW] New allocation: {} capacity={}",
            size.description(),
            capacity
        );
        BytesMut::with_capacity(capacity)
    }

    /// Return a buffer to the cache for reuse. Abnormal buffers (empty or over
    /// 10MB) are dropped; the cache is bounded by the tier's `max_cached`.
    pub fn return_buffer(&self, buffer: BytesMut, size: BufferSize) {
        if buffer.capacity() == 0 || buffer.capacity() > 10 * 1024 * 1024 {
            tracing::warn!(
                "[REJECT] Rejecting abnormal buffer: capacity={}",
                buffer.capacity()
            );
            return;
        }

        let queue = self.queue(size);
        if queue.len() >= size.max_cached() {
            tracing::trace!("[DROP] Cache full, dropping {} buffer", size.description());
            return;
        }

        if queue.push(buffer).is_err() {
            tracing::warn!("[WARNING] Buffer return failed: {}", size.description());
        }
    }
}

impl Default for OptimizedMemoryPool {
    fn default() -> Self {
        Self::new()
    }
}

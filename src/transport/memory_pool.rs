use bytes::BytesMut;
use std::sync::{Arc, OnceLock};

static SHARED_MEMORY_POOL: OnceLock<Arc<OptimizedMemoryPool>> = OnceLock::new();

/// Get the globally shared memory pool (lazy-initializes a default if none was registered).
pub fn shared_memory_pool() -> Arc<OptimizedMemoryPool> {
    SHARED_MEMORY_POOL
        .get_or_init(|| Arc::new(OptimizedMemoryPool::new()))
        .clone()
}

/// Read-buffer pool for the byte-stream adapters (TCP/QUIC).
///
/// Each tier is a **bounded** `flume` channel of reusable `BytesMut` buffers, so
/// the read loop avoids re-allocating on every frame. The cache limit is the
/// channel capacity itself: `return_buffer` uses `try_send`, which atomically
/// drops the buffer when the tier is full — the previous `len()`-then-`push()`
/// check was not atomic, so concurrent returns could overshoot the cap.
#[derive(Clone)]
pub struct OptimizedMemoryPool {
    small: BufferTier,
    medium: BufferTier,
    large: BufferTier,
}

/// One bounded cache of reusable buffers.
#[derive(Clone)]
struct BufferTier {
    tx: flume::Sender<BytesMut>,
    rx: flume::Receiver<BytesMut>,
}

impl BufferTier {
    fn with_capacity(max_cached: usize) -> Self {
        let (tx, rx) = flume::bounded(max_cached);
        Self { tx, rx }
    }
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
    /// Create a memory pool with each tier bounded at its `max_cached`.
    pub fn new() -> Self {
        Self {
            small: BufferTier::with_capacity(BufferSize::Small.max_cached()),
            medium: BufferTier::with_capacity(BufferSize::Medium.max_cached()),
            large: BufferTier::with_capacity(BufferSize::Large.max_cached()),
        }
    }

    fn tier(&self, size: BufferSize) -> &BufferTier {
        match size {
            BufferSize::Small => &self.small,
            BufferSize::Medium => &self.medium,
            BufferSize::Large => &self.large,
        }
    }

    /// Acquire a buffer for `size`: reuse a cached one if available, otherwise
    /// allocate. Returned buffers are cleared, so callers always see an empty
    /// buffer with capacity `>= size.capacity()`.
    pub fn get_buffer(&self, size: BufferSize) -> BytesMut {
        if let Ok(mut buffer) = self.tier(size).rx.try_recv() {
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
    /// 10MB) are dropped; `try_send` atomically drops the buffer when the tier
    /// is at capacity, so concurrent returns can never overshoot the cap.
    pub fn return_buffer(&self, buffer: BytesMut, size: BufferSize) {
        if buffer.capacity() == 0 || buffer.capacity() > 10 * 1024 * 1024 {
            tracing::warn!(
                "[REJECT] Rejecting abnormal buffer: capacity={}",
                buffer.capacity()
            );
            return;
        }
        if self.tier(size).tx.try_send(buffer).is_err() {
            tracing::trace!(
                "[DROP] Cache full or closed, dropping {} buffer",
                size.description()
            );
        }
    }
}

impl Default for OptimizedMemoryPool {
    fn default() -> Self {
        Self::new()
    }
}

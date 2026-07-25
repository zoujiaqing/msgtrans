use std::sync::Arc;

use crate::transport::memory_pool::{shared_memory_pool, OptimizedMemoryPool};
use crate::TransportError;

/// Shared context holding heavy resources that should be created once and reused.
///
/// The memory pool is also registered as the global shared pool so that adapters
/// (TCP, QUIC) created by factories automatically use the same instance —
/// eliminating the previous split between TransportContext's pool and the
/// global OnceLock pool.
#[derive(Clone)]
pub(crate) struct TransportContext {
    pub(crate) memory_pool: Arc<OptimizedMemoryPool>,
}

impl TransportContext {
    /// Create a new context holding the shared memory pool.
    pub(crate) async fn new() -> Result<Self, TransportError> {
        let pool = shared_memory_pool();
        Ok(Self { memory_pool: pool })
    }
}

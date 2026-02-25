use std::sync::Arc;

use crate::protocol::ProtocolRegistry;
use crate::transport::memory_pool::{init_shared_memory_pool, shared_memory_pool, OptimizedMemoryPool};
use crate::TransportError;

/// Shared context holding heavy resources that should be created once and reused.
///
/// The memory pool is also registered as the global shared pool so that adapters
/// (TCP, QUIC) created by factories automatically use the same instance —
/// eliminating the previous split between TransportContext's pool and the
/// global OnceLock pool.
#[derive(Clone)]
pub struct TransportContext {
    pub(crate) protocol_registry: Arc<ProtocolRegistry>,
    pub(crate) memory_pool: Arc<OptimizedMemoryPool>,
}

impl TransportContext {
    /// Create a new context with the standard protocol registry and the shared memory pool.
    pub async fn new() -> Result<Self, TransportError> {
        let registry = crate::adapters::create_standard_registry().await?;
        let pool = shared_memory_pool();
        Ok(Self {
            protocol_registry: Arc::new(registry),
            memory_pool: pool,
        })
    }

    /// Create with a custom memory pool (e.g. pre-allocated).
    ///
    /// The pool is registered globally so adapters created via factories
    /// also benefit from the custom configuration.
    pub async fn with_memory_pool(pool: OptimizedMemoryPool) -> Result<Self, TransportError> {
        let pool = Arc::new(pool);
        init_shared_memory_pool(pool.clone());
        let registry = crate::adapters::create_standard_registry().await?;
        Ok(Self {
            protocol_registry: Arc::new(registry),
            memory_pool: pool,
        })
    }

    pub fn memory_pool(&self) -> &Arc<OptimizedMemoryPool> {
        &self.memory_pool
    }

    pub fn protocol_registry(&self) -> &Arc<ProtocolRegistry> {
        &self.protocol_registry
    }
}

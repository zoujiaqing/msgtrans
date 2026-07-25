use crate::TransportError;

/// Shared context (dependency-injection seam) created once by the builder and
/// handed to every `Transport`.
///
/// The TCP/QUIC read-buffer pool is a global `OnceLock` the adapters reach
/// directly (`shared_memory_pool()`), so it no longer travels through this
/// context; the seam is retained for future shared resources.
#[derive(Clone)]
pub(crate) struct TransportContext {}

impl TransportContext {
    /// Create a new context.
    pub(crate) async fn new() -> Result<Self, TransportError> {
        Ok(Self {})
    }
}

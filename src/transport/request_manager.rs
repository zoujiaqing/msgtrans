use crate::packet::Packet;
use crate::transport::lockfree::LockFreeHashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Wrapper for oneshot::Sender to make it Clone
#[derive(Clone)]
struct SenderWrapper {
    inner: Arc<std::sync::Mutex<Option<oneshot::Sender<Packet>>>>,
}

impl SenderWrapper {
    fn new(sender: oneshot::Sender<Packet>) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new(Some(sender))),
        }
    }

    fn send(self, packet: Packet) -> Result<(), Packet> {
        if let Ok(mut guard) = self.inner.lock() {
            if let Some(sender) = guard.take() {
                return sender.send(packet);
            }
        }
        Err(packet)
    }
}

/// Lock-free request manager
pub struct RequestManager {
    pending: LockFreeHashMap<u32, SenderWrapper>,
    message_id_counter: AtomicU32,
}

impl RequestManager {
    pub fn new() -> Self {
        Self {
            pending: LockFreeHashMap::with_capacity(64),
            message_id_counter: AtomicU32::new(1),
        }
    }

    /// Register request, returns message_id and receiver
    pub fn register(&self) -> (u32, oneshot::Receiver<Packet>) {
        let (tx, rx) = oneshot::channel();
        let message_id = self.message_id_counter.fetch_add(1, Ordering::Relaxed);

        // If insertion fails, panic directly (should not happen in low-level library)
        self.pending
            .insert(message_id, SenderWrapper::new(tx))
            .expect("Failed to register request");

        tracing::debug!(
            "[REGISTER] RPC request registered: message_id={}",
            message_id
        );
        (message_id, rx)
    }

    /// Complete request
    pub fn complete(&self, message_id: u32, packet: Packet) -> bool {
        tracing::debug!(
            "[COMPLETE] Attempting to complete RPC request: message_id={}",
            message_id
        );
        match self.pending.remove(&message_id) {
            Ok(Some(sender)) => {
                tracing::debug!(
                    "[SUCCESS] Found matching RPC request, sending response: message_id={}",
                    message_id
                );
                let _ = sender.send(packet);
                true
            }
            _ => {
                tracing::warn!(
                    "[WARNING] No matching RPC request found: message_id={}",
                    message_id
                );
                false
            }
        }
    }

    /// Clear all pending requests (when connection closes)
    pub fn clear(&self) {
        if let Ok(snapshot) = self.pending.snapshot() {
            for (message_id, _) in snapshot {
                let _ = self.pending.remove(&message_id);
            }
        }
    }
}

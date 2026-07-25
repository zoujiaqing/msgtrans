//! Crate-internal concurrent containers.
//!
//! Thin wrappers over `dashmap::DashMap` and a `flume` MPMC channel, keeping the
//! small API the transport layer uses. 2.0 removed the per-operation statistics
//! (read/write counters and a running-average latency computed with
//! `Instant::now()` + a CAS loop on EVERY `get`): the counters had no consumer,
//! so the hot path was paying for metrics nobody read.

use crate::error::TransportError;
use dashmap::DashMap;
use std::hash::Hash;

/// Concurrent hash map (sharded, lock-free reads) — a `DashMap` with the
/// clone-on-read / `Result` API the transport layer expects.
pub struct LockFreeHashMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    map: DashMap<K, V>,
}

impl<K, V> LockFreeHashMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub fn new() -> Self {
        // Shard by available parallelism, rounded up to a power of two (DashMap
        // requires that), so concurrent writers rarely contend the same shard.
        let shard_count = std::thread::available_parallelism()
            .map(|n| (n.get() * 4).next_power_of_two())
            .unwrap_or(16);
        Self {
            map: DashMap::with_shard_amount(shard_count),
        }
    }

    /// Concurrent read (clones the value out).
    pub fn get(&self, key: &K) -> Option<V> {
        self.map.get(key).map(|v| v.clone())
    }

    /// Concurrent write.
    pub fn insert(&self, key: K, value: V) -> Result<Option<V>, TransportError> {
        Ok(self.map.insert(key, value))
    }

    /// Concurrent remove.
    pub fn remove(&self, key: &K) -> Result<Option<V>, TransportError> {
        Ok(self.map.remove(key).map(|(_, v)| v))
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Snapshot of the current keys.
    pub fn keys(&self) -> Result<Vec<K>, String> {
        Ok(self.map.iter().map(|entry| entry.key().clone()).collect())
    }
}

impl<K, V> Default for LockFreeHashMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Unbounded MPMC queue (a `flume` channel) with a lock-free `push`/`pop` API.
///
/// Only the TCP/QUIC read-buffer pool uses it, so it is gated to those
/// protocols — a WebSocket-only build carries neither the queue nor `flume`.
#[cfg(any(feature = "tcp", feature = "quic"))]
pub struct LockFreeQueue<T>
where
    T: Send + Sync + 'static,
{
    sender: flume::Sender<T>,
    receiver: flume::Receiver<T>,
}

#[cfg(any(feature = "tcp", feature = "quic"))]
impl<T> LockFreeQueue<T>
where
    T: Send + Sync + 'static,
{
    pub fn new() -> Self {
        let (sender, receiver) = flume::unbounded();
        Self { sender, receiver }
    }

    /// Enqueue.
    pub fn push(&self, item: T) -> Result<(), TransportError> {
        self.sender
            .send(item)
            .map_err(|_| TransportError::resource_error("queue_push", 1, 0))
    }

    /// Dequeue if non-empty.
    pub fn pop(&self) -> Option<T> {
        self.receiver.try_recv().ok()
    }

    /// Number of queued items (used to bound the buffer cache).
    pub fn len(&self) -> usize {
        self.receiver.len()
    }
}

#[cfg(any(feature = "tcp", feature = "quic"))]
impl<T> Default for LockFreeQueue<T>
where
    T: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashmap_basic_ops() {
        let map = LockFreeHashMap::new();
        assert!(map.insert("k1".to_string(), "v1".to_string()).is_ok());
        assert_eq!(map.get(&"k1".to_string()), Some("v1".to_string()));
        assert_eq!(map.len(), 1);
        assert_eq!(
            map.remove(&"k1".to_string()).unwrap(),
            Some("v1".to_string())
        );
        assert_eq!(map.len(), 0);
        assert_eq!(map.get(&"k1".to_string()), None);
    }

    #[cfg(any(feature = "tcp", feature = "quic"))]
    #[test]
    fn queue_fifo() {
        let q = LockFreeQueue::new();
        q.push(1).unwrap();
        q.push(2).unwrap();
        assert_eq!(q.pop(), Some(1));
        assert_eq!(q.pop(), Some(2));
        assert_eq!(q.pop(), None);
    }
}

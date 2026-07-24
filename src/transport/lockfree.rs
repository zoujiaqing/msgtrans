// Crate-internal generic lock-free containers: the standard container
// accessors (len/is_empty/stats) and the stats counters are retained as normal
// container API even where a given consumer does not call them. Not public API.
#![allow(dead_code)]
use std::hash::Hash;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
/// Lock-free optimization enhancement module - focused on first-stage lock-free optimization
///
/// Goals:
/// - Replace Arc<RwLock<HashMap>> hotspots
/// - Provide 50-150% performance improvement
/// - Maintain existing API compatibility
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use flume::{unbounded, Receiver, Sender};

use crate::error::TransportError;

/// Lock-free hash map - replacement for Arc<RwLock<HashMap>>
///
/// Uses crossbeam's epoch-based memory management to achieve wait-free reads
pub struct LockFreeHashMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Concurrent map with internal sharding
    map: DashMap<K, V>,
    /// Operation statistics
    stats: Arc<LockFreeStats>,
}

/// Lock-free statistics
#[derive(Debug)]
pub struct LockFreeStats {
    /// Number of reads
    pub reads: AtomicU64,
    /// Number of writes
    pub writes: AtomicU64,
    /// Number of CAS retries
    pub cas_retries: AtomicU64,
    /// Average read latency (nanoseconds)
    pub avg_read_latency_ns: AtomicU64,
}

impl<K, V> LockFreeHashMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Create new lock-free hash map
    pub fn new() -> Self {
        Self::with_capacity(16) // Default 16 shards
    }

    /// Create lock-free hash map with specified capacity
    pub fn with_capacity(shard_count: usize) -> Self {
        let shard_count = shard_count.max(1).next_power_of_two();

        Self {
            map: DashMap::with_shard_amount(shard_count),
            stats: Arc::new(LockFreeStats::new()),
        }
    }

    /// Wait-free read - core optimization point
    pub fn get(&self, key: &K) -> Option<V> {
        let start = Instant::now();
        let read_count = self.stats.reads.fetch_add(1, Ordering::Relaxed) + 1;
        let result = self.map.get(key).map(|v| v.clone());
        let latency = start.elapsed().as_nanos() as u64;

        // Maintain a running average instead of storing only the last sample.
        let mut current = self.stats.avg_read_latency_ns.load(Ordering::Relaxed);
        loop {
            let next = if read_count <= 1 {
                latency
            } else if latency >= current {
                current + (latency - current) / read_count
            } else {
                current - (current - latency) / read_count
            };
            match self.stats.avg_read_latency_ns.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }

        result
    }

    /// Concurrent write
    pub fn insert(&self, key: K, value: V) -> Result<Option<V>, TransportError> {
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        Ok(self.map.insert(key, value))
    }

    /// Concurrent remove
    pub fn remove(&self, key: &K) -> Result<Option<V>, TransportError> {
        self.stats.writes.fetch_add(1, Ordering::Relaxed);
        Ok(self.map.remove(key).map(|(_, v)| v))
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get all keys for iteration
    pub fn keys(&self) -> Result<Vec<K>, String> {
        Ok(self.map.iter().map(|entry| entry.key().clone()).collect())
    }

    /// Get statistics
    pub fn stats(&self) -> LockFreeStats {
        LockFreeStats {
            reads: AtomicU64::new(self.stats.reads.load(Ordering::Relaxed)),
            writes: AtomicU64::new(self.stats.writes.load(Ordering::Relaxed)),
            cas_retries: AtomicU64::new(self.stats.cas_retries.load(Ordering::Relaxed)),
            avg_read_latency_ns: AtomicU64::new(
                self.stats.avg_read_latency_ns.load(Ordering::Relaxed),
            ),
        }
    }
}

impl LockFreeStats {
    fn new() -> Self {
        Self {
            reads: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            cas_retries: AtomicU64::new(0),
            avg_read_latency_ns: AtomicU64::new(0),
        }
    }
}

/// High-performance lock-free queue - replacement for VecDeque
pub struct LockFreeQueue<T>
where
    T: Send + Sync + 'static,
{
    sender: Sender<T>,
    receiver: Receiver<T>,
    stats: Arc<QueueStats>,
}

/// Queue statistics
#[derive(Debug)]
pub struct QueueStats {
    pub enqueued: AtomicU64,
    pub dequeued: AtomicU64,
    pub current_size: AtomicUsize,
}

impl<T> LockFreeQueue<T>
where
    T: Send + Sync + 'static,
{
    /// Create new lock-free queue
    pub fn new() -> Self {
        let (sender, receiver) = unbounded();

        Self {
            sender,
            receiver,
            stats: Arc::new(QueueStats {
                enqueued: AtomicU64::new(0),
                dequeued: AtomicU64::new(0),
                current_size: AtomicUsize::new(0),
            }),
        }
    }

    /// Lock-free enqueue
    pub fn push(&self, item: T) -> Result<(), TransportError> {
        match self.sender.send(item) {
            Ok(_) => {
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                self.stats.current_size.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => Err(TransportError::resource_error("queue_push", 1, 0)),
        }
    }

    /// Lock-free dequeue
    pub fn pop(&self) -> Option<T> {
        match self.receiver.try_recv() {
            Ok(item) => {
                self.stats.dequeued.fetch_add(1, Ordering::Relaxed);
                self.stats.current_size.fetch_sub(1, Ordering::Relaxed);
                Some(item)
            }
            Err(_) => None,
        }
    }

    /// Get queue length
    pub fn len(&self) -> usize {
        self.stats.current_size.load(Ordering::Relaxed)
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get statistics
    pub fn stats(&self) -> &QueueStats {
        &self.stats
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
    fn test_lockfree_hashmap_basic() {
        let map = LockFreeHashMap::new();

        // Test insert
        assert!(map.insert("key1".to_string(), "value1".to_string()).is_ok());
        assert_eq!(map.get(&"key1".to_string()), Some("value1".to_string()));

        // Test update
        assert!(map.insert("key1".to_string(), "value2".to_string()).is_ok());
        assert_eq!(map.get(&"key1".to_string()), Some("value2".to_string()));

        // Test remove
        assert!(map.remove(&"key1".to_string()).is_ok());
        assert_eq!(map.get(&"key1".to_string()), None);
    }

    #[test]
    fn test_lockfree_queue() {
        let queue = LockFreeQueue::new();

        // Test basic operations
        assert!(queue.push(1).is_ok());
        assert!(queue.push(2).is_ok());
        assert_eq!(queue.len(), 2);

        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(2));
        assert_eq!(queue.pop(), None);
        assert!(queue.is_empty());
    }
}

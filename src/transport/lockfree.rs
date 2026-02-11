use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
/// Lock-free optimization enhancement module - focused on first-stage lock-free optimization
///
/// Goals:
/// - Replace Arc<RwLock<HashMap>> hotspots
/// - Provide 50-150% performance improvement
/// - Maintain existing API compatibility
use std::sync::Arc;
use std::time::Instant;

use crossbeam::epoch::{self, Atomic, Owned};
use crossbeam::utils::CachePadded;
use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::error::TransportError;

/// Lock-free hash map - replacement for Arc<RwLock<HashMap>>
///
/// Uses crossbeam's epoch-based memory management to achieve wait-free reads
pub struct LockFreeHashMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    /// Shard array to reduce contention
    shards: Vec<CachePadded<LockFreeShard<K, V>>>,
    /// Number of shards (must be a power of 2)
    shard_count: usize,
    /// Operation statistics
    stats: Arc<LockFreeStats>,
}

/// Lock-free shard
struct LockFreeShard<K, V> {
    /// Atomic pointer to HashMap
    map: Atomic<HashMap<K, V>>,
    /// Version number for CAS operations
    version: AtomicU64,
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
        let shard_count = shard_count.next_power_of_two();
        let mut shards = Vec::with_capacity(shard_count);

        for _ in 0..shard_count {
            shards.push(CachePadded::new(LockFreeShard {
                map: Atomic::new(HashMap::new()),
                version: AtomicU64::new(0),
            }));
        }

        Self {
            shards,
            shard_count,
            stats: Arc::new(LockFreeStats::new()),
        }
    }

    /// Get shard index
    fn shard_index(&self, key: &K) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) & (self.shard_count - 1)
    }

    /// Wait-free read - core optimization point
    pub fn get(&self, key: &K) -> Option<V> {
        let start = Instant::now();
        self.stats.reads.fetch_add(1, Ordering::Relaxed);

        let shard_idx = self.shard_index(key);
        let shard = &self.shards[shard_idx];

        let guard = epoch::pin();
        let map_ptr = shard.map.load(Ordering::Acquire, &guard);

        let result = if map_ptr.is_null() {
            None
        } else {
            unsafe { map_ptr.as_ref() }.unwrap().get(key).cloned()
        };

        // Record latency
        let latency = start.elapsed().as_nanos() as u64;
        self.stats
            .avg_read_latency_ns
            .store(latency, Ordering::Relaxed);

        result
    }

    /// Lock-free write using CAS operations
    pub fn insert(&self, key: K, value: V) -> Result<Option<V>, TransportError> {
        self.stats.writes.fetch_add(1, Ordering::Relaxed);

        let shard_idx = self.shard_index(&key);
        let shard = &self.shards[shard_idx];

        let mut retry_count = 0;
        const MAX_RETRIES: usize = 100;

        loop {
            let guard = epoch::pin();
            let current_ptr = shard.map.load(Ordering::Acquire, &guard);

            // Create new HashMap
            let mut new_map = if current_ptr.is_null() {
                HashMap::new()
            } else {
                unsafe { current_ptr.as_ref() }.unwrap().clone()
            };

            let old_value = new_map.insert(key.clone(), value.clone());

            // Attempt CAS update
            let new_ptr = Owned::new(new_map);
            match shard.map.compare_exchange_weak(
                current_ptr,
                new_ptr,
                Ordering::Release,
                Ordering::Relaxed,
                &guard,
            ) {
                Ok(_) => {
                    // Successfully update version number
                    shard.version.fetch_add(1, Ordering::Relaxed);

                    // Defer release of old data
                    if !current_ptr.is_null() {
                        unsafe {
                            guard.defer_unchecked(move || {
                                drop(current_ptr.into_owned());
                            });
                        }
                    }

                    return Ok(old_value);
                }
                Err(_) => {
                    retry_count += 1;
                    if retry_count >= MAX_RETRIES {
                        return Err(TransportError::resource_error(
                            "lockfree_insert",
                            retry_count,
                            MAX_RETRIES,
                        ));
                    }
                    self.stats.cas_retries.fetch_add(1, Ordering::Relaxed);

                    // Backoff strategy
                    if retry_count > 10 {
                        std::thread::yield_now();
                    }
                }
            }
        }
    }

    /// Lock-free remove
    pub fn remove(&self, key: &K) -> Result<Option<V>, TransportError> {
        self.stats.writes.fetch_add(1, Ordering::Relaxed);

        let shard_idx = self.shard_index(key);
        let shard = &self.shards[shard_idx];

        let mut retry_count = 0;
        const MAX_RETRIES: usize = 100;

        loop {
            let guard = epoch::pin();
            let current_ptr = shard.map.load(Ordering::Acquire, &guard);

            if current_ptr.is_null() {
                return Ok(None);
            }

            let mut new_map = unsafe { current_ptr.as_ref() }.unwrap().clone();
            let old_value = new_map.remove(key);

            let new_ptr = Owned::new(new_map);
            match shard.map.compare_exchange_weak(
                current_ptr,
                new_ptr,
                Ordering::Release,
                Ordering::Relaxed,
                &guard,
            ) {
                Ok(_) => {
                    shard.version.fetch_add(1, Ordering::Relaxed);

                    unsafe {
                        guard.defer_unchecked(move || {
                            drop(current_ptr.into_owned());
                        });
                    }

                    return Ok(old_value);
                }
                Err(_) => {
                    retry_count += 1;
                    if retry_count >= MAX_RETRIES {
                        return Err(TransportError::resource_error(
                            "lockfree_remove",
                            retry_count,
                            MAX_RETRIES,
                        ));
                    }
                    self.stats.cas_retries.fetch_add(1, Ordering::Relaxed);

                    if retry_count > 10 {
                        std::thread::yield_now();
                    }
                }
            }
        }
    }

    /// Get all key-value pairs snapshot for async operations
    pub fn snapshot(&self) -> Result<Vec<(K, V)>, String> {
        let mut result = Vec::new();

        for shard in &self.shards {
            let guard = epoch::pin();
            let map_ptr = shard.map.load(Ordering::Acquire, &guard);

            if !map_ptr.is_null() {
                let map = unsafe { map_ptr.as_ref() }.unwrap();
                for (k, v) in map.iter() {
                    result.push((k.clone(), v.clone()));
                }
            }
        }

        Ok(result)
    }

    /// Get number of entries
    pub fn len(&self) -> usize {
        let mut count = 0;

        for shard in &self.shards {
            let guard = epoch::pin();
            let map_ptr = shard.map.load(Ordering::Acquire, &guard);

            if !map_ptr.is_null() {
                count += unsafe { map_ptr.as_ref() }.unwrap().len();
            }
        }

        count
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get all keys for iteration
    pub fn keys(&self) -> Result<Vec<K>, String> {
        let mut all_keys = Vec::new();

        for shard in &self.shards {
            let guard = epoch::pin();
            let map_ptr = shard.map.load(Ordering::Acquire, &guard);

            if !map_ptr.is_null() {
                let map = unsafe { map_ptr.as_ref() }.unwrap();
                for key in map.keys() {
                    all_keys.push(key.clone());
                }
            }
        }

        Ok(all_keys)
    }

    /// Traverse operation - replacement for RwLock::read().await iter()
    pub fn for_each<F>(&self, mut f: F) -> Result<(), String>
    where
        F: FnMut(&K, &V),
    {
        for shard in &self.shards {
            let guard = epoch::pin();
            let map_ptr = shard.map.load(Ordering::Acquire, &guard);

            if !map_ptr.is_null() {
                let map = unsafe { map_ptr.as_ref() }.unwrap();
                for (k, v) in map.iter() {
                    f(k, v);
                }
            }
        }

        Ok(())
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

    /// Get CAS success rate
    pub fn cas_success_rate(&self) -> f64 {
        let writes = self.writes.load(Ordering::Relaxed) as f64;
        let retries = self.cas_retries.load(Ordering::Relaxed) as f64;

        if writes == 0.0 {
            1.0
        } else {
            writes / (writes + retries)
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

/// High-performance atomic counter - replacement for RwLock<usize>
pub struct LockFreeCounter {
    value: AtomicUsize,
    stats: Arc<CounterStats>,
}

/// Counter statistics
#[derive(Debug)]
pub struct CounterStats {
    pub increments: AtomicU64,
    pub decrements: AtomicU64,
    pub reads: AtomicU64,
}

impl LockFreeCounter {
    /// Create new lock-free counter
    pub fn new(initial: usize) -> Self {
        Self {
            value: AtomicUsize::new(initial),
            stats: Arc::new(CounterStats {
                increments: AtomicU64::new(0),
                decrements: AtomicU64::new(0),
                reads: AtomicU64::new(0),
            }),
        }
    }

    /// Atomic increment
    pub fn increment(&self) -> usize {
        self.stats.increments.fetch_add(1, Ordering::Relaxed);
        self.value.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Atomic decrement
    pub fn decrement(&self) -> usize {
        self.stats.decrements.fetch_add(1, Ordering::Relaxed);
        self.value.fetch_sub(1, Ordering::Relaxed).saturating_sub(1)
    }

    /// Atomic read
    pub fn get(&self) -> usize {
        self.stats.reads.fetch_add(1, Ordering::Relaxed);
        self.value.load(Ordering::Relaxed)
    }

    /// Atomic set
    pub fn set(&self, value: usize) {
        self.value.store(value, Ordering::Relaxed);
    }

    /// Atomic swap
    pub fn swap(&self, value: usize) -> usize {
        self.value.swap(value, Ordering::Relaxed)
    }

    /// Get statistics
    pub fn stats(&self) -> &CounterStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

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
    fn test_lockfree_hashmap_concurrent() {
        let map = Arc::new(LockFreeHashMap::new());
        let mut handles = vec![];

        // Concurrent writes
        for i in 0..10 {
            let map_clone = Arc::clone(&map);
            let handle = thread::spawn(move || {
                for j in 0..100 {
                    let key = format!("key_{}", i * 100 + j);
                    let value = format!("value_{}", i * 100 + j);
                    map_clone.insert(key, value).unwrap();
                }
            });
            handles.push(handle);
        }

        // Concurrent reads
        for i in 0..5 {
            let map_clone = Arc::clone(&map);
            let handle = thread::spawn(move || {
                for _ in 0..1000 {
                    let key = format!("key_{}", i);
                    let _value = map_clone.get(&key);
                }
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // Verify results
        assert_eq!(map.len(), 1000);
        let stats = map.stats();
        println!("CAS success rate: {:.2}%", stats.cas_success_rate() * 100.0);
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

    #[test]
    fn test_lockfree_counter() {
        let counter = LockFreeCounter::new(0);

        assert_eq!(counter.increment(), 1);
        assert_eq!(counter.increment(), 2);
        assert_eq!(counter.get(), 2);
        assert_eq!(counter.decrement(), 1);
        assert_eq!(counter.get(), 1);
    }
}

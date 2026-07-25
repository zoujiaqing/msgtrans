//! Crate-internal concurrent map — a thin wrapper over `dashmap::DashMap` with
//! the clone-on-read / `Result` API the transport layer uses. (It is sharded and
//! concurrent, not lock-free; the old `LockFreeHashMap` name overclaimed.) 2.0
//! removed the per-operation statistics the wrapper used to keep — read/write
//! counters and a running-average latency computed with `Instant::now()` + a CAS
//! loop on EVERY `get` — because nothing consumed them.

use crate::error::TransportError;
use dashmap::DashMap;
use std::hash::Hash;

/// Concurrent hash map — a sharded `DashMap` (per-shard `RwLock`, not
/// lock-free; the previous `LockFreeHashMap` name overclaimed) with the
/// clone-on-read / `Result` API the transport layer expects.
pub struct ConcurrentMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    map: DashMap<K, V>,
}

impl<K, V> ConcurrentMap<K, V>
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

impl<K, V> Default for ConcurrentMap<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
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
        let map = ConcurrentMap::new();
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
}

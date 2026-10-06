//! Exact, process-local computation caches. TTL is housekeeping, not freshness.
use moka::sync::Cache;
use std::{hash::Hash, time::Duration};

pub const LAYER_BYTES: u64 = 16 * 1024 * 1024;

pub struct ExactCache<K, V> {
    inner: Cache<K, V>,
    enabled: bool,
    hits: u64,
    misses: u64,
}
impl<K: Hash + Eq + Send + Sync + 'static, V: Clone + Send + Sync + 'static> ExactCache<K, V> {
    pub fn new(weight: impl Fn(&K, &V) -> usize + Send + Sync + 'static) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(LAYER_BYTES)
                .time_to_live(Duration::from_secs(900))
                .weigher(move |k, v| weight(k, v).saturating_add(256).min(u32::MAX as usize) as u32)
                .build(),
            enabled: true,
            hits: 0,
            misses: 0,
        }
    }
    pub fn get(&mut self, key: &K) -> Option<V> {
        let value = self.enabled.then(|| self.inner.get(key)).flatten();
        if value.is_some() {
            self.hits += 1
        } else {
            self.misses += 1
        }
        value
    }
    pub fn insert(&self, key: K, value: V) {
        if self.enabled {
            self.inner.insert(key, value);
        }
    }
    pub fn clear(&self) {
        self.inner.invalidate_all();
        self.inner.run_pending_tasks();
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        self.clear();
        self.enabled = enabled;
    }
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({"enabled":self.enabled,"hits":self.hits,"misses":self.misses,
            "entries_approx":self.inner.entry_count(),"weighted_bytes_approx":self.inner.weighted_size(),
            "capacity_bytes":LAYER_BYTES,"ttl_seconds":900})
    }
}

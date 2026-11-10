//! Cache Manager building blocks.
//!
//! HTTP caching itself is performed by Servo's network layer, which implements the
//! HTTP caching rules (`Cache-Control`, `Expires`, validation with `ETag` /
//! `Last-Modified`, `Vary`). Blanket does not duplicate it; instead it decides when the
//! engine cache should be dropped (memory pressure, user request) and reports on it.
//!
//! The browser's own caches (decoded favicons, thumbnails of suspended tabs) use the
//! byte-budgeted LRU below so their memory use is bounded and can be trimmed under
//! pressure.

use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

/// Anything stored in a [`ByteLruCache`] reports its approximate heap size.
pub trait Weighted {
    fn weight_bytes(&self) -> u64;
}

impl Weighted for Vec<u8> {
    fn weight_bytes(&self) -> u64 {
        self.len() as u64
    }
}

/// Hit/miss statistics of a cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub bytes: u64,
    pub budget_bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

impl CacheStats {
    pub fn hit_ratio(&self) -> f64 {
        let lookups = self.hits + self.misses;
        if lookups == 0 { 0.0 } else { self.hits as f64 / lookups as f64 }
    }
}

struct Entry<V> {
    value: V,
    bytes: u64,
    tick: u64,
}

/// Least-recently-used cache bounded by total weight rather than entry count.
pub struct ByteLruCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    recency: BTreeMap<u64, K>,
    tick: u64,
    bytes: u64,
    budget: u64,
    hits: u64,
    misses: u64,
    evictions: u64,
}

impl<K: Eq + Hash + Clone, V: Weighted> ByteLruCache<K, V> {
    pub fn new(budget_bytes: u64) -> Self {
        Self {
            entries: HashMap::new(),
            recency: BTreeMap::new(),
            tick: 0,
            bytes: 0,
            budget: budget_bytes,
            hits: 0,
            misses: 0,
            evictions: 0,
        }
    }

    fn next_tick(&mut self) -> u64 {
        self.tick += 1;
        self.tick
    }

    /// Look up a value, marking it as recently used.
    pub fn get<Q>(&mut self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let tick = self.tick + 1;
        match self.entries.get_mut(key) {
            Some(entry) => {
                self.tick = tick;
                let old = std::mem::replace(&mut entry.tick, tick);
                if let Some(owned_key) = self.recency.remove(&old) {
                    self.recency.insert(tick, owned_key);
                }
                self.hits += 1;
                Some(&entry.value)
            },
            None => {
                self.misses += 1;
                None
            },
        }
    }

    pub fn contains<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        self.entries.contains_key(key)
    }

    /// Insert a value. Values heavier than the whole budget are not cached at all.
    /// Returns `false` if the value was rejected.
    pub fn insert(&mut self, key: K, value: V) -> bool {
        let bytes = value.weight_bytes();
        self.remove(&key);
        if bytes > self.budget {
            return false;
        }
        let tick = self.next_tick();
        self.recency.insert(tick, key.clone());
        self.entries.insert(key, Entry { value, bytes, tick });
        self.bytes += bytes;
        self.evict_to(self.budget);
        true
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Eq + Hash + ?Sized,
    {
        let entry = self.entries.remove(key)?;
        self.recency.remove(&entry.tick);
        self.bytes -= entry.bytes;
        Some(entry.value)
    }

    /// Evict least-recently-used entries until at most `target_bytes` are used.
    pub fn evict_to(&mut self, target_bytes: u64) {
        while self.bytes > target_bytes {
            let Some((_, key)) = self.recency.pop_first() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&key) {
                self.bytes -= entry.bytes;
                self.evictions += 1;
            }
        }
    }

    /// Shrink to a fraction of the budget (used under memory pressure).
    pub fn trim(&mut self, keep_fraction: f64) {
        let target = (self.budget as f64 * keep_fraction.clamp(0.0, 1.0)) as u64;
        self.evict_to(target);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.recency.clear();
        self.bytes = 0;
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.entries.len(),
            bytes: self.bytes,
            budget_bytes: self.budget,
            hits: self.hits,
            misses: self.misses,
            evictions: self.evictions,
        }
    }
}

/// Decides when the engine's HTTP cache is dropped.
#[derive(Clone, Copy, Debug)]
pub struct HttpCachePolicy {
    pub clear_on_critical: bool,
}

impl HttpCachePolicy {
    /// The engine cache is in-memory only; dropping it frees memory at the cost of
    /// re-downloading resources, so only do it when memory is critically low.
    pub fn should_clear(&self, pressure: crate::memory::MemoryPressure) -> bool {
        self.clear_on_critical && pressure == crate::memory::MemoryPressure::Critical
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(size: usize) -> Vec<u8> {
        vec![0; size]
    }

    #[test]
    fn evicts_least_recently_used_by_weight() {
        let mut cache = ByteLruCache::new(100);
        cache.insert("a", blob(40));
        cache.insert("b", blob(40));
        assert!(cache.get("a").is_some());
        cache.insert("c", blob(40));
        assert!(cache.contains("a"));
        assert!(!cache.contains("b"), "b was least recently used");
        assert!(cache.contains("c"));
        assert_eq!(cache.stats().bytes, 80);
        assert_eq!(cache.stats().evictions, 1);
    }

    #[test]
    fn oversized_values_are_rejected() {
        let mut cache = ByteLruCache::new(10);
        assert!(!cache.insert("big", blob(11)));
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn replacing_a_key_updates_weight() {
        let mut cache = ByteLruCache::new(100);
        cache.insert("a", blob(60));
        cache.insert("a", blob(10));
        assert_eq!(cache.stats().bytes, 10);
        assert_eq!(cache.stats().entries, 1);
    }

    #[test]
    fn trim_shrinks_to_fraction_of_budget() {
        let mut cache = ByteLruCache::new(100);
        for key in 0..10 {
            cache.insert(key, blob(10));
        }
        cache.trim(0.25);
        assert!(cache.stats().bytes <= 25);
        assert!(cache.contains(&9), "most recent entries survive");
    }

    #[test]
    fn statistics_track_hits_and_misses() {
        let mut cache = ByteLruCache::new(100);
        cache.insert("a", blob(1));
        cache.get("a");
        cache.get("missing");
        let stats = cache.stats();
        assert_eq!((stats.hits, stats.misses), (1, 1));
        assert!((stats.hit_ratio() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn http_cache_only_cleared_when_critical() {
        use crate::memory::MemoryPressure;
        let policy = HttpCachePolicy { clear_on_critical: true };
        assert!(!policy.should_clear(MemoryPressure::Moderate));
        assert!(policy.should_clear(MemoryPressure::Critical));
        let disabled = HttpCachePolicy { clear_on_critical: false };
        assert!(!disabled.should_clear(MemoryPressure::Critical));
    }
}

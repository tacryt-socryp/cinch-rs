//! Tool result caching for the harness.
//!
//! Avoids re-executing identical tool calls when the result hasn't changed.
//! Read-only tools (those that return `cacheable() == true`) have their
//! results cached by `(tool_name, arguments_hash)`. Mutation tools
//! automatically invalidate relevant cache entries.

use std::collections::HashMap;

/// A cache entry for a tool result.
#[derive(Debug, Clone)]
struct CacheEntry {
    result: String,
    round_produced: u32,
}

/// Cache for tool results, keyed by (tool_name, arguments_hash).
///
/// The cache is checked before executing a tool call. If a cache hit is
/// found and the tool is cacheable, the cached result is returned without
/// re-execution. Mutation tools invalidate the cache.
#[derive(Debug)]
pub struct ToolResultCache {
    entries: HashMap<(String, u64), CacheEntry>,
    /// Maximum number of entries before eviction.
    max_entries: usize,
    /// Hits counter for diagnostics.
    hits: u64,
    /// Misses counter for diagnostics.
    misses: u64,
}

impl ToolResultCache {
    /// Create a new cache with the given capacity.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_entries,
            hits: 0,
            misses: 0,
        }
    }

    /// Look up a cached result. Returns `Some(result)` on cache hit.
    pub fn get(&mut self, tool_name: &str, arguments: &str) -> Option<&str> {
        let key = (tool_name.to_string(), hash_arguments(arguments));
        if let Some(entry) = self.entries.get(&key) {
            self.hits += 1;
            Some(&entry.result)
        } else {
            self.misses += 1;
            None
        }
    }

    /// Store a result in the cache.
    pub fn put(&mut self, tool_name: &str, arguments: &str, result: String, round: u32) {
        // Evict oldest entries if at capacity.
        if self.entries.len() >= self.max_entries {
            self.evict_oldest();
        }
        let key = (tool_name.to_string(), hash_arguments(arguments));
        self.entries.insert(
            key,
            CacheEntry {
                result,
                round_produced: round,
            },
        );
    }

    /// Invalidate all cache entries (e.g. after a mutation tool runs).
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
    }

    /// Invalidate cache entries older than `max_age` rounds.
    pub fn evict_older_than(&mut self, current_round: u32, max_age: u32) {
        self.entries
            .retain(|_, entry| current_round.saturating_sub(entry.round_produced) <= max_age);
    }

    /// Number of entries in the cache.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Cache hit count.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Cache miss count.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    /// Hit rate as a fraction (0.0 to 1.0).
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Evict the oldest entry (by round_produced).
    fn evict_oldest(&mut self) {
        if let Some(oldest_key) = self
            .entries
            .iter()
            .min_by_key(|(_, v)| v.round_produced)
            .map(|(k, _)| k.clone())
        {
            self.entries.remove(&oldest_key);
        }
    }
}

impl Default for ToolResultCache {
    fn default() -> Self {
        Self::new(100)
    }
}

/// Hash tool arguments for cache key. Uses a simple FNV-1a hash.
fn hash_arguments(arguments: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in arguments.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_put_and_get() {
        let mut cache = ToolResultCache::new(10);
        cache.put(
            "read_file",
            r#"{"path":"foo.rs"}"#,
            "file contents".into(),
            1,
        );

        let hit = cache.get("read_file", r#"{"path":"foo.rs"}"#);
        assert_eq!(hit, Some("file contents"));
        assert_eq!(cache.hits(), 1);
    }

    #[test]
    fn cache_miss() {
        let mut cache = ToolResultCache::new(10);
        let miss = cache.get("read_file", r#"{"path":"bar.rs"}"#);
        assert_eq!(miss, None);
        assert_eq!(cache.misses(), 1);
    }

    #[test]
    fn cache_invalidate_all() {
        let mut cache = ToolResultCache::new(10);
        cache.put("read_file", r#"{"path":"foo.rs"}"#, "contents".into(), 1);
        assert_eq!(cache.len(), 1);

        cache.invalidate_all();
        assert!(cache.is_empty());
    }

    #[test]
    fn cache_evict_older_than() {
        let mut cache = ToolResultCache::new(10);
        cache.put("read_file", r#"{"path":"a.rs"}"#, "a".into(), 1);
        cache.put("read_file", r#"{"path":"b.rs"}"#, "b".into(), 5);

        // Evict entries older than 2 rounds (current round = 6).
        cache.evict_older_than(6, 2);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn cache_capacity_eviction() {
        let mut cache = ToolResultCache::new(2);
        cache.put("a", "1", "r1".into(), 1);
        cache.put("b", "2", "r2".into(), 2);
        cache.put("c", "3", "r3".into(), 3); // Should evict oldest (round 1).
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn hit_rate_computation() {
        let mut cache = ToolResultCache::new(10);
        cache.put("t", "a", "r".into(), 1);
        cache.get("t", "a"); // hit
        cache.get("t", "b"); // miss
        assert!((cache.hit_rate() - 0.5).abs() < 0.01);
    }

    #[test]
    fn hash_deterministic() {
        let h1 = hash_arguments(r#"{"path":"foo.rs","line":42}"#);
        let h2 = hash_arguments(r#"{"path":"foo.rs","line":42}"#);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_different_inputs_differ() {
        let h1 = hash_arguments(r#"{"path":"foo.rs"}"#);
        let h2 = hash_arguments(r#"{"path":"bar.rs"}"#);
        assert_ne!(h1, h2);
    }
}

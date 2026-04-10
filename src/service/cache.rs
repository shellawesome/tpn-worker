use dashmap::DashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Simple TTL cache backed by DashMap.
/// Mirrors `modules/caching.js` (get_tpn_cache / set_tpn_cache).
#[derive(Debug, Clone)]
pub struct TtlCache {
    entries: Arc<DashMap<String, CacheEntry>>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    value: serde_json::Value,
    expires_at: Option<Instant>,
}

impl TtlCache {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
        }
    }

    /// Get a cached value. Returns None if missing or expired.
    pub fn get(&self, key: &str) -> Option<serde_json::Value> {
        let entry = self.entries.get(key)?;
        if let Some(expires_at) = entry.expires_at {
            if Instant::now() > expires_at {
                drop(entry);
                self.entries.remove(key);
                return None;
            }
        }
        Some(entry.value.clone())
    }

    /// Get a cached string value.
    pub fn get_string(&self, key: &str) -> Option<String> {
        self.get(key).and_then(|v| v.as_str().map(String::from))
    }

    /// Set a value with optional TTL.
    pub fn set(&self, key: &str, value: serde_json::Value, ttl: Option<Duration>) {
        let expires_at = ttl.map(|d| Instant::now() + d);
        self.entries
            .insert(key.to_string(), CacheEntry { value, expires_at });
    }

    /// Set a string value with optional TTL.
    pub fn set_string(&self, key: &str, value: &str, ttl: Option<Duration>) {
        self.set(key, serde_json::Value::String(value.to_string()), ttl);
    }

    /// Remove a key.
    pub fn remove(&self, key: &str) {
        self.entries.remove(key);
    }

    /// Check if key exists and is not expired.
    pub fn contains(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Evict all expired entries (optional background cleanup).
    pub fn evict_expired(&self) {
        let now = Instant::now();
        self.entries
            .retain(|_, entry| entry.expires_at.map_or(true, |exp| now <= exp));
    }
}

impl Default for TtlCache {
    fn default() -> Self {
        Self::new()
    }
}

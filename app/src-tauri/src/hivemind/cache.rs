use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Default upper bound on cached response body size (100 KB).
const DEFAULT_MAX_RESPONSE_SIZE: usize = 100 * 1024;

/// Emit an INFO metrics line every Nth `get()` call so users can see whether
/// the cache is doing useful work without spinning up a background loop.
const METRICS_LOG_EVERY_N: u64 = 100;

/// A cached LLM response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResponse {
    pub output: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub model_id: String,
    pub cached_at: DateTime<Utc>,
}

/// Lightweight access counters surfaced via [`ResponseCache::metrics`] and
/// auto-logged at INFO every `METRICS_LOG_EVERY_N` get calls.
#[derive(Debug, Default)]
struct CacheMetrics {
    hits: AtomicU64,
    misses: AtomicU64,
    inserts: AtomicU64,
    skipped_oversized: AtomicU64,
}

/// A snapshot of cache metrics suitable for logging or returning over IPC.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CacheMetricsSnapshot {
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub skipped_oversized: u64,
    pub entry_count: u64,
}

/// A lock-free, concurrent response cache backed by [`moka`].
///
/// Supports both TTL-based and capacity-based eviction. Responses larger than
/// `max_response_size` are silently skipped on insert to avoid evicting many
/// smaller entries.
///
/// Instrumented with hit/miss/insert/skip counters surfaced via
/// [`ResponseCache::metrics`]. Every Nth `get()` call emits an INFO log line
/// so cache effectiveness is visible without a background reporter.
#[derive(Debug, Clone)]
pub struct ResponseCache {
    cache: Cache<String, CachedResponse>,
    max_response_size: usize,
    metrics: Arc<CacheMetrics>,
}

impl ResponseCache {
    /// Create a new cache.
    ///
    /// * `max_entries` -- Maximum number of entries before eviction kicks in.
    /// * `ttl` -- Time-to-live for each entry after insertion.
    pub fn new(max_entries: u64, ttl: Duration) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_entries)
            .time_to_live(ttl)
            .build();

        Self {
            cache,
            max_response_size: DEFAULT_MAX_RESPONSE_SIZE,
            metrics: Arc::new(CacheMetrics::default()),
        }
    }

    /// Create a cache with a custom maximum response size.
    pub fn with_max_response_size(mut self, max_response_size: usize) -> Self {
        self.max_response_size = max_response_size;
        self
    }

    /// Return a snapshot of the current metrics counters.
    pub fn metrics(&self) -> CacheMetricsSnapshot {
        CacheMetricsSnapshot {
            hits: self.metrics.hits.load(Ordering::Relaxed),
            misses: self.metrics.misses.load(Ordering::Relaxed),
            inserts: self.metrics.inserts.load(Ordering::Relaxed),
            skipped_oversized: self.metrics.skipped_oversized.load(Ordering::Relaxed),
            entry_count: self.cache.entry_count(),
        }
    }

    /// Build a deterministic cache key from precomputed prompt hashes.
    ///
    /// The caller should precompute hashes of the system and user prompts
    /// with [`ResponseCache::hash_str`] so that large prompt strings are
    /// only hashed once per round rather than once per model.
    pub fn make_key(
        model_id: &str,
        system_prompt_hash: u64,
        user_prompt_hash: u64,
        provider_key: &str,
        temperature: Option<f64>,
        top_p: Option<f64>,
        max_tokens: Option<u32>,
    ) -> String {
        format!(
            "{}:{:016x}:{:016x}:{}:{}:{}:{}",
            model_id,
            system_prompt_hash,
            user_prompt_hash,
            provider_key,
            temperature.unwrap_or(f64::NAN).to_bits(),
            top_p.unwrap_or(f64::NAN).to_bits(),
            max_tokens.map_or(0, |v| v),
        )
    }

    /// Look up a cached response by key.
    pub async fn get(&self, key: &str) -> Option<CachedResponse> {
        let result = self.cache.get(key).await;
        if result.is_some() {
            self.metrics.hits.fetch_add(1, Ordering::Relaxed);
            debug!(key = %key, "cache hit");
        } else {
            self.metrics.misses.fetch_add(1, Ordering::Relaxed);
        }

        // Periodic metrics flush — lighter-touch than a background task, and
        // attached to the hot path that drives effectiveness numbers.
        let hits = self.metrics.hits.load(Ordering::Relaxed);
        let misses = self.metrics.misses.load(Ordering::Relaxed);
        let total = hits.saturating_add(misses);
        if total > 0 && total % METRICS_LOG_EVERY_N == 0 {
            let hit_rate = (hits as f64) / (total as f64);
            info!(
                hits,
                misses,
                inserts = self.metrics.inserts.load(Ordering::Relaxed),
                skipped_oversized = self.metrics.skipped_oversized.load(Ordering::Relaxed),
                entry_count = self.cache.entry_count(),
                hit_rate = format!("{:.2}", hit_rate),
                "response cache metrics",
            );
        }
        result
    }

    /// Insert a response into the cache.
    ///
    /// If the response body exceeds `max_response_size` the insert is silently
    /// skipped to avoid evicting many smaller, more useful entries.
    pub async fn insert(&self, key: String, response: CachedResponse) {
        if response.output.len() > self.max_response_size {
            self.metrics
                .skipped_oversized
                .fetch_add(1, Ordering::Relaxed);
            debug!(
                key = %key,
                size = response.output.len(),
                max = self.max_response_size,
                "skipping cache insert, response too large",
            );
            return;
        }
        self.metrics.inserts.fetch_add(1, Ordering::Relaxed);
        debug!(key = %key, "cache insert");
        self.cache.insert(key, response).await;
    }

    /// Remove all entries from the cache.
    pub async fn clear(&self) {
        debug!("cache clear");
        self.cache.invalidate_all();
    }

    /// Hash a string using [`RandomState`] (same SipHash-1-3 algorithm as
    /// [`std::collections::hash_map::DefaultHasher`] but with a per-process
    /// random key for better HashDoS resistance).
    ///
    /// The hasher instance is initialized once per process via [`OnceLock`]
    /// so that all calls within a process lifetime produce consistent results
    /// (the moka cache is in-memory and lost on restart, so cross-run
    /// determinism is not required).
    pub fn hash_str(s: &str) -> u64 {
        static RANDOM_STATE: OnceLock<RandomState> = OnceLock::new();
        let mut hasher = RANDOM_STATE.get_or_init(RandomState::new).build_hasher();
        s.hash(&mut hasher);
        hasher.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_and_get() {
        let cache = ResponseCache::new(100, Duration::from_secs(300));
        let sys_hash = ResponseCache::hash_str("system");
        let usr_hash = ResponseCache::hash_str("hello");
        let key = ResponseCache::make_key("gpt-4", sys_hash, usr_hash, "openai", None, None, None);

        let response = CachedResponse {
            output: "world".into(),
            input_tokens: 5,
            output_tokens: 1,
            model_id: "gpt-4".into(),
            cached_at: Utc::now(),
        };

        cache.insert(key.clone(), response.clone()).await;
        let hit = cache.get(&key).await;
        assert!(hit.is_some());
        assert_eq!(hit.unwrap().output, "world");
    }

    #[tokio::test]
    async fn skips_oversized_response() {
        let cache = ResponseCache::new(100, Duration::from_secs(300)).with_max_response_size(10);
        let key = "oversized".to_string();

        let response = CachedResponse {
            output: "a".repeat(100),
            input_tokens: 1,
            output_tokens: 100,
            model_id: "test".into(),
            cached_at: Utc::now(),
        };

        cache.insert(key.clone(), response).await;
        assert!(cache.get(&key).await.is_none());
    }

    #[tokio::test]
    async fn deterministic_key() {
        let sys_hash = ResponseCache::hash_str("sys");
        let usr_hash = ResponseCache::hash_str("usr");
        let k1 = ResponseCache::make_key("model", sys_hash, usr_hash, "provider", None, None, None);
        let k2 = ResponseCache::make_key("model", sys_hash, usr_hash, "provider", None, None, None);
        assert_eq!(k1, k2);

        let diff_hash = ResponseCache::hash_str("different");
        let k3 =
            ResponseCache::make_key("model", sys_hash, diff_hash, "provider", None, None, None);
        assert_ne!(k1, k3);
    }

    #[tokio::test]
    async fn two_consecutive_gets_both_hit() {
        // Acceptance test for the singleton-cache fix: once a key is inserted
        // a subsequent `get` returns a hit, and a second `get` on the same key
        // also returns a hit (i.e. `get` does not consume the entry and the
        // metrics counters reflect both lookups).
        let cache = ResponseCache::new(100, Duration::from_secs(300));
        let sys_hash = ResponseCache::hash_str("sys");
        let usr_hash = ResponseCache::hash_str("usr");
        let key =
            ResponseCache::make_key("model", sys_hash, usr_hash, "provider", None, None, None);

        let response = CachedResponse {
            output: "cached output".into(),
            input_tokens: 10,
            output_tokens: 20,
            model_id: "model".into(),
            cached_at: Utc::now(),
        };

        cache.insert(key.clone(), response.clone()).await;

        let first = cache.get(&key).await;
        assert!(first.is_some(), "first get should be a hit");
        assert_eq!(first.unwrap().output, "cached output");

        let second = cache.get(&key).await;
        assert!(second.is_some(), "second get should still be a hit");
        assert_eq!(second.unwrap().output, "cached output");

        let metrics = cache.metrics();
        assert_eq!(metrics.hits, 2, "both gets should count as hits");
        assert_eq!(metrics.misses, 0, "no misses expected");
        assert_eq!(metrics.inserts, 1, "exactly one insert performed");
    }

    #[tokio::test]
    async fn different_temperature_produces_different_key() {
        let sys_hash = ResponseCache::hash_str("sys");
        let usr_hash = ResponseCache::hash_str("usr");
        let k1 = ResponseCache::make_key(
            "model",
            sys_hash,
            usr_hash,
            "provider",
            Some(0.0),
            None,
            None,
        );
        let k2 = ResponseCache::make_key(
            "model",
            sys_hash,
            usr_hash,
            "provider",
            Some(1.0),
            None,
            None,
        );
        assert_ne!(k1, k2);
    }

    #[tokio::test]
    async fn different_top_p_produces_different_key() {
        let sys_hash = ResponseCache::hash_str("sys");
        let usr_hash = ResponseCache::hash_str("usr");
        let k1 = ResponseCache::make_key(
            "model",
            sys_hash,
            usr_hash,
            "provider",
            None,
            Some(0.5),
            None,
        );
        let k2 = ResponseCache::make_key(
            "model",
            sys_hash,
            usr_hash,
            "provider",
            None,
            Some(0.9),
            None,
        );
        assert_ne!(k1, k2);
    }

    #[tokio::test]
    async fn different_max_tokens_produces_different_key() {
        let sys_hash = ResponseCache::hash_str("sys");
        let usr_hash = ResponseCache::hash_str("usr");
        let k1 = ResponseCache::make_key(
            "model",
            sys_hash,
            usr_hash,
            "provider",
            None,
            None,
            Some(100),
        );
        let k2 = ResponseCache::make_key(
            "model",
            sys_hash,
            usr_hash,
            "provider",
            None,
            None,
            Some(500),
        );
        assert_ne!(k1, k2);
    }
}

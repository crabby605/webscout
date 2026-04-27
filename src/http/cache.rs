use std::collections::HashMap;
use std::time::{Duration, Instant};
use bytes::Bytes;
use tokio::sync::RwLock;
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;

use crate::http::HttpResponse;

#[derive(Debug, Clone)]
pub struct CachedEntry {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub body: Bytes,
    pub cached_at: Instant,
    pub ttl: Duration,
}

#[derive(Debug, Default)]
pub struct RequestCache {
    entries: RwLock<HashMap<u64, CachedEntry>>,
}

impl RequestCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generate_cache_key(method: &str, url: &str, accept: Option<&str>) -> u64 {
        let mut hasher = DefaultHasher::new();
        method.hash(&mut hasher);
        url.hash(&mut hasher);
        if let Some(a) = accept {
            a.hash(&mut hasher);
        }
        hasher.finish()
    }

    pub async fn get(&self, key: u64) -> Option<CachedEntry> {
        let entries = self.entries.read().await;
        if let Some(entry) = entries.get(&key) {
            if entry.cached_at.elapsed() <= entry.ttl {
                return Some(entry.clone());
            }
        }
        None
    }

    pub async fn insert(&self, key: u64, response: &HttpResponse, cache_control_header: Option<&str>) {
        let mut ttl = Duration::from_secs(300); // Default 5 minutes

        if let Some(cc) = cache_control_header {
            if cc.contains("no-store") || cc.contains("no-cache") {
                return;
            }
            if let Some(max_age_str) = cc.split(',').find(|s| s.trim().starts_with("max-age=")) {
                if let Some(max_age_val) = max_age_str.split('=').nth(1) {
                    if let Ok(seconds) = max_age_val.trim().parse::<u64>() {
                        ttl = Duration::from_secs(seconds);
                    }
                }
            }
        }

        let entry = CachedEntry {
            status: response.status,
            headers: response.headers.clone(),
            body: response.body.clone(),
            cached_at: Instant::now(),
            ttl,
        };

        let mut entries = self.entries.write().await;
        entries.insert(key, entry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderMap;

    #[tokio::test]
    async fn test_cache_ttl_expiry() {
        let cache = RequestCache::new();
        let key = RequestCache::generate_cache_key("GET", "https://example.com", None);

        let resp = HttpResponse {
            status: 200,
            final_url: "https://example.com".to_string(),
            headers: HeaderMap::new(),
            body: Bytes::from("ok"),
        };

        // Should cache for 1 second
        cache.insert(key, &resp, Some("max-age=1")).await;

        {
            let loaded = cache.get(key).await;
            assert!(loaded.is_some());
        }

        tokio::time::sleep(Duration::from_millis(1100)).await;

        {
            let expired = cache.get(key).await;
            assert!(expired.is_none());
        }
    }
}

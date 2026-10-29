//! Process-local key-value cache with absolute expiries and periodic cleanup.

use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::{
    sync::Mutex,
    time::{Instant, MissedTickBehavior, interval_at},
};

use super::now_seconds;

const CLEANUP_INTERVAL: Duration = Duration::from_secs(60);

/// A byte value with its absolute Unix expiry.
struct Entry {
    value: Vec<u8>,
    expires_at: i64,
}

/// Shared in-memory cache with Redis-like set, get, and get-and-delete operations.
#[derive(Clone)]
pub struct MemoryCache {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl MemoryCache {
    /// Create a cache and start a worker that removes expired keys every minute.
    pub fn new() -> Self {
        let entries = Arc::new(Mutex::new(HashMap::<String, Entry>::new()));
        let weak_entries = Arc::downgrade(&entries);
        tokio::spawn(async move {
            let mut interval = interval_at(Instant::now() + CLEANUP_INTERVAL, CLEANUP_INTERVAL);
            interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(entries) = weak_entries.upgrade() else {
                    break;
                };
                entries
                    .lock()
                    .await
                    .retain(|_, entry| entry.expires_at > now_seconds());
            }
        });
        Self { entries }
    }

    /// Store bytes under a key until the given Unix timestamp.
    pub async fn set(&self, key: String, value: Vec<u8>, expires_at: i64) {
        let mut entries = self.entries.lock().await;
        if expires_at > now_seconds() {
            entries.insert(key, Entry { value, expires_at });
        } else {
            entries.remove(&key);
        }
    }

    /// Read a live value without consuming it.
    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let mut entries = self.entries.lock().await;
        match entries.get(key) {
            Some(entry) if entry.expires_at > now_seconds() => Some(entry.value.clone()),
            Some(_) => {
                entries.remove(key);
                None
            }
            None => None,
        }
    }

    /// Atomically read and remove a live value.
    pub async fn get_del(&self, key: &str) -> Option<Vec<u8>> {
        self.entries
            .lock()
            .await
            .remove(key)
            .filter(|entry| entry.expires_at > now_seconds())
            .map(|entry| entry.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn cleanup_worker_removes_expired_entries() {
        let cache = MemoryCache::new();
        cache.entries.lock().await.insert(
            "expired".into(),
            Entry {
                value: b"value".to_vec(),
                expires_at: now_seconds() - 1,
            },
        );
        tokio::task::yield_now().await;
        tokio::time::advance(CLEANUP_INTERVAL).await;
        tokio::task::yield_now().await;
        assert!(cache.entries.lock().await.is_empty());
    }
}

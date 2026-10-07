//! Cache - L1 memory + L2 file driver (port of Cache.php).
//!
//! CACHE_DRIVER=file|memory (default file), CACHE_DIR (default `storage/cache`),
//! CACHE_TTL (default 300), CACHE_L1_MAX (default 1000 entries),
//! CACHE_L1_MAX_MEMORY_MB (default 64, 0 = disabled).
//! Values are JSON-only. File keys are SHA-256 hashed and bucketed in 256 sub-dirs
//! (`xx/<hash>.cache`), writes are atomic (tmp + rename).
//!
//! PHP -> Rust mapping:
//!
//! | PHP                     | Rust                                   |
//! |-------------------------|----------------------------------------|
//! | `get($key, $default)`   | `get` (Option) / `get_or`              |
//! | `set($key,$v,$ttl)`     | `set` (ttl `None` = default, `Some(0)` = forever) |
//! | `has`                   | `has`                                  |
//! | `delete / deleteMany`   | `delete` (alias `forget`) / `delete_many` |
//! | `clear`                 | `clear` (alias `flush`)                |
//! | `remember`              | `remember` (async) / `remember_sync`   |
//! | `increment / decrement` | `increment / decrement`                |
//! | `cleanup`               | `cleanup`                              |
//! | `reset / clearMemory / getMemorySize` | `reset / clear_memory / memory_size` |
//!
//! Redis (`CACHE_DRIVER=redis`) is not included: like PHP without a Redis
//! client installed, it logs once and falls back to the file driver.
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock, RwLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::env;

struct Entry {
    value: Value,
    /// 0 = never expires.
    expires: u64,
    /// Insertion sequence (oldest-first eviction).
    seq: u64,
    size: usize,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn memory() -> &'static RwLock<HashMap<String, Entry>> {
    static M: OnceLock<RwLock<HashMap<String, Entry>>> = OnceLock::new();
    M.get_or_init(|| RwLock::new(HashMap::new()))
}

fn seq() -> u64 {
    static S: AtomicU64 = AtomicU64::new(0);
    S.fetch_add(1, Ordering::Relaxed)
}

fn incr_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// File layer enabled? `redis` falls back to file (logged once).
fn use_file() -> bool {
    match env::get_or("CACHE_DRIVER", "file").as_str() {
        "memory" => false,
        "redis" => {
            static WARN: OnceLock<()> = OnceLock::new();
            WARN.get_or_init(|| {
                tracing::warn!("[padi] Redis cache driver is not available in the Rust core. Falling back to file cache.");
            });
            true
        }
        _ => true,
    }
}

fn dir() -> PathBuf {
    PathBuf::from(env::get_or("CACHE_DIR", "storage/cache"))
}

fn file_for(key: &str) -> PathBuf {
    let hash: String = Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    dir().join(&hash[..2]).join(format!("{hash}.cache"))
}

fn is_expired(expires: u64, t: u64) -> bool {
    expires > 0 && expires < t
}

/// Walk all `.cache` files recursively; removes empty bucket directories.
fn walk_cache_files(d: &Path, visitor: &mut dyn FnMut(&Path)) {
    let Ok(items) = fs::read_dir(d) else { return };
    for item in items.flatten() {
        let path = item.path();
        if path.is_dir() {
            walk_cache_files(&path, visitor);
            let _ = fs::remove_dir(&path); // only succeeds when empty
        } else if path.extension().is_some_and(|e| e == "cache") {
            visitor(&path);
        }
    }
}

pub struct Cache;

impl Cache {
    fn default_ttl() -> u64 {
        env::get_int("CACHE_TTL", 300).max(1) as u64
    }

    // ── L1 memory layer ──

    fn l1_get(key: &str) -> Option<Value> {
        let t = now();
        {
            let m = memory().read().ok()?;
            match m.get(key) {
                None => return None,
                Some(e) if !is_expired(e.expires, t) => return Some(e.value.clone()),
                _ => {}
            }
        }
        if let Ok(mut m) = memory().write() {
            m.remove(key); // expired
        }
        None
    }

    /// Write to L1 with bounded eviction: oldest 25% evicted at the entry limit,
    /// everything flushed when the approximate value size exceeds the memory guard.
    fn l1_put(key: &str, value: Value, expires: u64) {
        let max = env::get_int("CACHE_L1_MAX", 1000).max(1) as usize;
        let max_mb = env::get_int("CACHE_L1_MAX_MEMORY_MB", 64);
        let max_bytes = if max_mb > 0 { max_mb as usize * 1_048_576 } else { 0 };
        let size = value.to_string().len();
        if let Ok(mut m) = memory().write() {
            if max_bytes > 0 && m.values().map(|e| e.size).sum::<usize>() > max_bytes {
                m.clear();
            }
            if m.len() >= max && !m.contains_key(key) {
                let mut ages: Vec<(String, u64)> = m.iter().map(|(k, e)| (k.clone(), e.seq)).collect();
                ages.sort_by_key(|(_, s)| *s);
                for (k, _) in ages.into_iter().take((max / 4).max(1)) {
                    m.remove(&k);
                }
            }
            m.insert(key.to_string(), Entry { value, expires, seq: seq(), size });
        }
    }

    // ── L2 file layer ──

    fn file_write(key: &str, value: &Value, expires: u64) -> bool {
        let path = file_for(key);
        if let Some(parent) = path.parent() {
            if fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        let tmp = PathBuf::from(format!("{}.tmp.{}", path.display(), std::process::id()));
        let payload = json!({"key": key, "value": value, "expires": expires}).to_string();
        if fs::write(&tmp, payload).is_err() {
            return false;
        }
        fs::rename(&tmp, &path).is_ok()
    }

    /// Read + validate + expiry-check a cache file (no L1 promotion).
    fn file_read_entry(key: &str) -> Option<(Value, u64)> {
        let path = file_for(key);
        let raw = fs::read_to_string(&path).ok()?;
        let v: Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => {
                let _ = fs::remove_file(&path);
                return None;
            }
        };
        let (Some(expires), Some(value)) = (v["expires"].as_u64(), v.get("value")) else {
            let _ = fs::remove_file(&path);
            return None;
        };
        if is_expired(expires, now()) {
            let _ = fs::remove_file(&path);
            return None;
        }
        Some((value.clone(), expires))
    }

    // ── public API ──

    /// Lookup L1 -> L2. `None` = miss (a cached JSON `null` is `Some(Value::Null)`).
    pub fn get(key: &str) -> Option<Value> {
        if let Some(v) = Self::l1_get(key) {
            return Some(v);
        }
        if !use_file() {
            return None;
        }
        let (value, expires) = Self::file_read_entry(key)?;
        Self::l1_put(key, value.clone(), expires); // promote to L1
        Some(value)
    }

    /// `get($key, $default)`
    pub fn get_or(key: &str, default: Value) -> Value {
        Self::get(key).unwrap_or(default)
    }

    /// Write L1 + L2. `ttl`: `None` = default (CACHE_TTL), `Some(0)` = forever.
    pub fn set(key: &str, value: Value, ttl: Option<u64>) -> bool {
        let ttl = ttl.unwrap_or_else(Self::default_ttl);
        let expires = if ttl > 0 { now() + ttl } else { 0 };
        Self::store(key, value, expires)
    }

    fn store(key: &str, value: Value, expires: u64) -> bool {
        Self::l1_put(key, value.clone(), expires);
        if use_file() {
            Self::file_write(key, &value, expires)
        } else {
            true
        }
    }

    pub fn has(key: &str) -> bool {
        Self::get(key).is_some()
    }

    /// Delete a key (L1 + L2).
    pub fn delete(key: &str) -> bool {
        if let Ok(mut m) = memory().write() {
            m.remove(key);
        }
        let path = file_for(key);
        !path.exists() || fs::remove_file(path).is_ok()
    }

    /// Alias of [`Cache::delete`].
    pub fn forget(key: &str) -> bool {
        Self::delete(key)
    }

    /// Delete several keys; returns how many files were deleted.
    pub fn delete_many(keys: &[&str]) -> usize {
        if let Ok(mut m) = memory().write() {
            for k in keys {
                m.remove(*k);
            }
        }
        keys.iter()
            .filter(|k| {
                let p = file_for(k);
                p.exists() && fs::remove_file(p).is_ok()
            })
            .count()
    }

    /// Clear all entries (L1 + L2).
    pub fn clear() -> bool {
        Self::clear_memory();
        if use_file() {
            walk_cache_files(&dir(), &mut |p| {
                let _ = fs::remove_file(p);
            });
        }
        true
    }

    /// Alias of [`Cache::clear`].
    pub fn flush() {
        Self::clear();
    }

    /// Return cached value or compute, store and return it
    /// (cached `null` counts as a hit, preventing stampedes).
    pub async fn remember<F, Fut>(key: &str, ttl: Option<u64>, f: F) -> Value
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Value>,
    {
        if let Some(v) = Self::get(key) {
            return v;
        }
        let v = f().await;
        Self::set(key, v.clone(), ttl);
        v
    }

    /// Synchronous variant of [`Cache::remember`].
    pub fn remember_sync<F: FnOnce() -> Value>(key: &str, ttl: Option<u64>, f: F) -> Value {
        if let Some(v) = Self::get(key) {
            return v;
        }
        let v = f();
        Self::set(key, v.clone(), ttl);
        v
    }

    /// Atomically (process-wide lock) increment a numeric value, keeping its TTL.
    /// `None` when the stored value is not numeric.
    pub fn increment(key: &str, step: i64) -> Option<i64> {
        let _guard = incr_lock().lock().ok()?;
        let current = if use_file() {
            Self::file_read_entry(key)
        } else {
            memory().read().ok()?.get(key).filter(|e| !is_expired(e.expires, now())).map(|e| (e.value.clone(), e.expires))
        };
        let (cur, expires) = match current {
            Some((v, e)) => (v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))?, e),
            None => (0, 0),
        };
        let new = cur + step;
        Self::store(key, json!(new), expires);
        Some(new)
    }

    pub fn decrement(key: &str, step: i64) -> Option<i64> {
        Self::increment(key, -step)
    }

    /// Remove expired file entries; returns the number of deleted files.
    /// Recently modified files (< default TTL old) are skipped without reading.
    pub fn cleanup() -> usize {
        if !use_file() {
            return 0;
        }
        let t = now();
        let ttl = Self::default_ttl();
        let mut deleted = 0;
        walk_cache_files(&dir(), &mut |p| {
            let fresh = fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|m| t.saturating_sub(m.as_secs()) < ttl);
            if fresh {
                return;
            }
            let data: Option<Value> = fs::read_to_string(p).ok().and_then(|r| serde_json::from_str(&r).ok());
            let stale = match data.as_ref().and_then(|d| d["expires"].as_u64()) {
                None => true,
                Some(e) => is_expired(e, t),
            };
            if stale && fs::remove_file(p).is_ok() {
                deleted += 1;
            }
        });
        deleted
    }

    /// Per-request reset hook (API parity): cache state intentionally persists.
    pub fn reset() {}

    /// Clear only L1 (forces re-read from L2).
    pub fn clear_memory() {
        if let Ok(mut m) = memory().write() {
            m.clear();
        }
    }

    /// Current L1 entry count.
    pub fn memory_size() -> usize {
        memory().read().map(|m| m.len()).unwrap_or(0)
    }
}

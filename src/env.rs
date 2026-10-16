//! Env - Environment Variable Loader for Padi Rust Api Framework
//!
//! Loads `.env` files once and caches values in an in-memory cache.
//! - Multi-thread safe: loaded once during application startup, reused across requests.
//! - Fast lookups: in-memory cache avoids repeated system calls.
//! - Pure Rust parser: zero external runtime dependencies required.
//! - Compatible with both `Env::...` struct calls and `env::...` module calls.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, RwLock};

/// Global in-memory environment cache.
static CACHE: LazyLock<RwLock<HashMap<String, String>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Tracks whether `.env` has already been loaded.
static LOADED: AtomicBool = AtomicBool::new(false);

/// Env - Environment variable loader and accessor.
pub struct Env;

impl Env {
    /// Load `.env` file once.
    ///
    /// If already loaded and `force_reload` is false, this is a no-op.
    /// Existing process environment variables take precedence unless `force_reload` is true.
    pub fn load(path: impl AsRef<Path>) -> bool {
        Self::load_with(path, false)
    }

    /// Load `.env` file with optional forced reloading.
    ///
    /// Port of `Env::load(string $path, bool $forceReload = false)`.
    pub fn load_with(path: impl AsRef<Path>, force_reload: bool) -> bool {
        let path = path.as_ref();

        if (LOADED.load(Ordering::SeqCst) && !force_reload) || !path.exists() {
            return false;
        }

        let Ok(content) = fs::read_to_string(path) else {
            return false;
        };

        LOADED.store(true, Ordering::SeqCst);

        let mut cache = CACHE.write().unwrap_or_else(|poisoned| poisoned.into_inner());

        for line in content.lines() {
            if let Some((name, value)) = Self::parse_line(line) {
                // If force_reload or not already in cache/env
                let already_cached = cache.contains_key(&name);
                let already_in_process = std::env::var(&name).is_ok();

                if force_reload || (!already_cached && !already_in_process) {
                    #[allow(unused_unsafe)]
                    unsafe {
                        std::env::set_var(&name, &value);
                    }
                    cache.insert(name, value);
                } else if !already_cached && already_in_process {
                    // Populate cache with process env value for fast subsequent lookups
                    if let Ok(process_val) = std::env::var(&name) {
                        cache.insert(name, process_val);
                    }
                }
            }
        }

        true
    }

    /// Force reload `.env` file even if already loaded.
    pub fn reload(path: impl AsRef<Path>) -> bool {
        Self::load_with(path, true)
    }

    /// Parse a single line from a `.env` file following `Env.php` rules.
    pub fn parse_line(line: &str) -> Option<(String, String)> {
        let mut trimmed = line.trim_start();

        // Optional `export ` prefix support
        if let Some(rest) = trimmed.strip_prefix("export ") {
            trimmed = rest.trim_start();
        }

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return None;
        }

        // Parse name=value
        let eq_pos = trimmed.find('=')?;
        let name = trimmed[..eq_pos].trim();
        if name.is_empty() {
            return None;
        }

        let mut value = trimmed[eq_pos + 1..].trim();

        // Remove inline comments (only when not inside quotes)
        if !value.is_empty() && !value.starts_with('"') && !value.starts_with('\'') {
            if let Some(hash_pos) = value.find(" #") {
                value = value[..hash_pos].trim_end();
            }
        }

        // Remove surrounding quotes
        let clean_value = if value.len() >= 2 {
            let bytes = value.as_bytes();
            let first = bytes[0];
            let last = bytes[bytes.len() - 1];
            if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
                &value[1..value.len() - 1]
            } else {
                value
            }
        } else {
            value
        };

        Some((name.to_string(), clean_value.to_string()))
    }

    /// Get environment variable value.
    ///
    /// Priority: In-memory cache (`$_ENV`) > `std::env::var()` > `None`.
    /// When `std::env::var()` succeeds, the result is cached to avoid
    /// repeated system calls on subsequent lookups.
    pub fn get(key: &str) -> Option<String> {
        // 1. Fast read from cache
        if let Ok(cache) = CACHE.read() {
            if let Some(val) = cache.get(key) {
                return Some(val.clone());
            }
        }

        // 2. Fallback to process environment
        if let Ok(val) = std::env::var(key) {
            if let Ok(mut cache) = CACHE.write() {
                cache.insert(key.to_string(), val.clone());
            }
            return Some(val);
        }

        None
    }

    /// Get environment variable or return default.
    pub fn get_or(key: &str, default: &str) -> String {
        Self::get(key).unwrap_or_else(|| default.to_string())
    }

    /// Get environment variable parsed as `i64` or return default.
    pub fn get_int(key: &str, default: i64) -> i64 {
        Self::get(key)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default)
    }

    /// Get environment variable parsed as `f64` or return default.
    pub fn get_float(key: &str, default: f64) -> f64 {
        Self::get(key)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(default)
    }

    /// Get environment variable parsed as boolean or return default.
    /// Matches `true`, `1`, `yes`, `on` (case-insensitive).
    pub fn get_bool(key: &str, default: bool) -> bool {
        match Self::get(key) {
            Some(v) => {
                let s = v.trim().to_ascii_lowercase();
                s == "true" || s == "1" || s == "yes" || s == "on"
            }
            None => default,
        }
    }

    /// Check if variable is true (`true`, `1`, `yes`, `on`).
    pub fn is_true(key: &str) -> bool {
        Self::get_bool(key, false)
    }

    /// Get comma-separated slice environment variable.
    pub fn get_slice(key: &str, default: &[&str]) -> Vec<String> {
        match Self::get(key) {
            Some(v) => {
                let parts: Vec<String> = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if parts.is_empty() {
                    default.iter().map(|&s| s.to_string()).collect()
                } else {
                    parts
                }
            }
            None => default.iter().map(|&s| s.to_string()).collect(),
        }
    }

    /// Set an environment variable in both cache and process environment.
    pub fn set(key: &str, value: &str) {
        if let Ok(mut cache) = CACHE.write() {
            cache.insert(key.to_string(), value.to_string());
        }
        #[allow(unused_unsafe)]
        unsafe {
            std::env::set_var(key, value);
        }
    }

    /// Check if an environment variable exists.
    pub fn has(key: &str) -> bool {
        Self::get(key).is_some()
    }

    /// Get snapshot of all cached environment variables.
    pub fn all() -> HashMap<String, String> {
        CACHE.read().map(|c| c.clone()).unwrap_or_default()
    }

    /// True when running in production mode (`APP_ENV=production` or `APP_ENV=prod`).
    pub fn is_production() -> bool {
        let env = Self::get_or("APP_ENV", "production").to_ascii_lowercase();
        env == "production" || env == "prod"
    }

    /// True when running in development mode (`APP_ENV=development`, `dev`, `local`).
    pub fn is_development() -> bool {
        let env = Self::get_or("APP_ENV", "production").to_ascii_lowercase();
        env == "development" || env == "dev" || env == "local"
    }

    /// True when running in testing mode (`APP_ENV=testing` or `test`).
    pub fn is_testing() -> bool {
        let env = Self::get_or("APP_ENV", "production").to_ascii_lowercase();
        env == "testing" || env == "test"
    }

    /// True when `APP_ENV=development` and `APP_DEBUG=true`.
    pub fn is_debug() -> bool {
        Self::is_development() && Self::is_true("APP_DEBUG")
    }

    /// Reset internal cache and loaded flag (useful in testing).
    #[doc(hidden)]
    pub fn reset_cache_for_test() {
        if let Ok(mut cache) = CACHE.write() {
            cache.clear();
        }
        LOADED.store(false, Ordering::SeqCst);
    }
}

// Module-level functions forwarding to `Env::*` for backward compatibility
// and module-style usage: `use padi_core::env; env::get(...)`.

#[inline]
pub fn load(path: impl AsRef<Path>) -> bool {
    Env::load(path)
}

#[inline]
pub fn load_with(path: impl AsRef<Path>, force_reload: bool) -> bool {
    Env::load_with(path, force_reload)
}

#[inline]
pub fn reload(path: impl AsRef<Path>) -> bool {
    Env::reload(path)
}

#[inline]
pub fn parse_line(line: &str) -> Option<(String, String)> {
    Env::parse_line(line)
}

#[inline]
pub fn get(key: &str) -> Option<String> {
    Env::get(key)
}

#[inline]
pub fn get_or(key: &str, default: &str) -> String {
    Env::get_or(key, default)
}

#[inline]
pub fn get_int(key: &str, default: i64) -> i64 {
    Env::get_int(key, default)
}

#[inline]
pub fn get_float(key: &str, default: f64) -> f64 {
    Env::get_float(key, default)
}

#[inline]
pub fn get_bool(key: &str, default: bool) -> bool {
    Env::get_bool(key, default)
}

#[inline]
pub fn is_true(key: &str) -> bool {
    Env::is_true(key)
}

#[inline]
pub fn get_slice(key: &str, default: &[&str]) -> Vec<String> {
    Env::get_slice(key, default)
}

#[inline]
pub fn set(key: &str, value: &str) {
    Env::set(key, value);
}

#[inline]
pub fn has(key: &str) -> bool {
    Env::has(key)
}

#[inline]
pub fn all() -> HashMap<String, String> {
    Env::all()
}

#[inline]
pub fn is_production() -> bool {
    Env::is_production()
}

#[inline]
pub fn is_development() -> bool {
    Env::is_development()
}

#[inline]
pub fn is_testing() -> bool {
    Env::is_testing()
}

#[inline]
pub fn is_debug() -> bool {
    Env::is_debug()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_line_basic() {
        assert_eq!(
            Env::parse_line("APP_NAME=Padi Rust Api Framework"),
            Some(("APP_NAME".to_string(), "Padi Rust Api Framework".to_string()))
        );
        assert_eq!(
            Env::parse_line("export APP_PORT=8080"),
            Some(("APP_PORT".to_string(), "8080".to_string()))
        );
        assert_eq!(
            Env::parse_line("  DB_PASSWORD=   "),
            Some(("DB_PASSWORD".to_string(), "".to_string()))
        );
    }

    #[test]
    fn test_parse_line_comments_and_empty() {
        assert_eq!(Env::parse_line(""), None);
        assert_eq!(Env::parse_line("   "), None);
        assert_eq!(Env::parse_line("# this is a comment"), None);
        assert_eq!(Env::parse_line("   # indented comment"), None);
        assert_eq!(Env::parse_line("INVALID_LINE_NO_EQUALS"), None);
    }

    #[test]
    fn test_parse_line_inline_comments() {
        assert_eq!(
            Env::parse_line("APP_ENV=development # default environment"),
            Some(("APP_ENV".to_string(), "development".to_string()))
        );
        // Quoted value with `#` should NOT strip
        assert_eq!(
            Env::parse_line("SECRET_KEY=\"my#secret #not_a_comment\""),
            Some(("SECRET_KEY".to_string(), "my#secret #not_a_comment".to_string()))
        );
        assert_eq!(
            Env::parse_line("HASH_VAL='val #comment'"),
            Some(("HASH_VAL".to_string(), "val #comment".to_string()))
        );
    }

    #[test]
    fn test_parse_line_quotes() {
        assert_eq!(
            Env::parse_line("APP_KEY=\"secret123\""),
            Some(("APP_KEY".to_string(), "secret123".to_string()))
        );
        assert_eq!(
            Env::parse_line("APP_KEY='secret123'"),
            Some(("APP_KEY".to_string(), "secret123".to_string()))
        );
    }

    #[test]
    fn test_get_helpers() {
        Env::set("TEST_PORT", "9000");
        Env::set("TEST_FLOAT", "3.14");
        Env::set("TEST_BOOL", "true");
        Env::set("TEST_CORS", "http://localhost, https://example.com");

        assert_eq!(Env::get_or("TEST_PORT", "80"), "9000");
        assert_eq!(Env::get_int("TEST_PORT", 80), 9000);
        assert_eq!(Env::get_float("TEST_FLOAT", 1.0), 3.14);
        assert!(Env::is_true("TEST_BOOL"));
        assert!(Env::get_bool("TEST_BOOL", false));
        assert_eq!(
            Env::get_slice("TEST_CORS", &[]),
            vec!["http://localhost", "https://example.com"]
        );
        assert!(Env::has("TEST_PORT"));
        assert!(!Env::has("NON_EXISTENT_KEY_12345"));
    }

    #[test]
    fn test_debug_and_environments() {
        Env::set("APP_ENV", "development");
        Env::set("APP_DEBUG", "true");
        assert!(Env::is_development());
        assert!(!Env::is_production());
        assert!(Env::is_debug());

        Env::set("APP_ENV", "production");
        assert!(Env::is_production());
        assert!(!Env::is_debug());
    }

    #[test]
    fn test_file_load_and_reload() {
        let temp_dir = std::env::temp_dir();
        let env_file = temp_dir.join("padi_test.env");

        let content = "LOAD_TEST_VAR=original_val\nLOAD_TEST_OVERRIDE=initial\n";
        std::fs::write(&env_file, content).unwrap();

        Env::reset_cache_for_test();
        assert!(Env::load(&env_file));
        assert_eq!(Env::get("LOAD_TEST_VAR"), Some("original_val".to_string()));

        // Second normal load shouldn't reload
        let new_content = "LOAD_TEST_VAR=modified_val\nLOAD_TEST_OVERRIDE=updated\n";
        std::fs::write(&env_file, new_content).unwrap();
        Env::load(&env_file);
        assert_eq!(Env::get("LOAD_TEST_VAR"), Some("original_val".to_string()));

        // Force reload should update
        Env::reload(&env_file);
        assert_eq!(Env::get("LOAD_TEST_VAR"), Some("modified_val".to_string()));

        let _ = std::fs::remove_file(env_file);
    }
}

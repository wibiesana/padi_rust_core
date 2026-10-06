//! Env - environment variable loader (port of Env.php).
use std::sync::Once;

static LOADED: Once = Once::new();

/// Load a `.env` file once. Existing process variables always win.
pub fn load(path: &str) {
    LOADED.call_once(|| {
        let _ = dotenvy::from_path(path);
    });
}

pub fn get(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

pub fn get_or(key: &str, default: &str) -> String {
    get(key).unwrap_or_else(|| default.to_string())
}

pub fn get_int(key: &str, default: i64) -> i64 {
    get(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default)
}

pub fn is_true(key: &str) -> bool {
    get(key).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

/// True when APP_ENV=development and APP_DEBUG=true.
pub fn is_debug() -> bool {
    get_or("APP_ENV", "production") == "development" && is_true("APP_DEBUG")
}

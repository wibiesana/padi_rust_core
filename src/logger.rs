//! Logger - daily file logs (app-*.log, error-*.log) + stdout (Rust port of Logger.php).
use std::{fs, sync::OnceLock, time::Duration};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*};

static GUARDS: OnceLock<Vec<WorkerGuard>> = OnceLock::new();

pub struct Logger;

impl Logger {
    /// Initialise logging into `<root>/storage/logs`. Safe to call once at startup.
    pub fn init(root: &str) {
        let dir = format!("{root}/storage/logs");
        let _ = fs::create_dir_all(&dir);
        Self::rotate(&dir, 14);

        let (app, g1) = tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, "app.log"));
        let (err, g2) = tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, "error.log"));
        let _ = GUARDS.set(vec![g1, g2]);

        let _ = tracing_subscriber::registry()
            .with(fmt::layer().with_writer(app).with_ansi(false))
            .with(fmt::layer().with_writer(err).with_ansi(false).with_filter(LevelFilter::ERROR))
            .with(fmt::layer().with_filter(LevelFilter::INFO))
            .try_init();
    }

    /// Delete log files older than `days`.
    fn rotate(dir: &str, days: u64) {
        let cutoff = Duration::from_secs(days * 86_400);
        if let Ok(rd) = fs::read_dir(dir) {
            for e in rd.flatten() {
                let old = e.metadata().and_then(|m| m.modified()).ok()
                    .and_then(|m| m.elapsed().ok())
                    .map(|age| age > cutoff)
                    .unwrap_or(false);
                if old {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    /// Log informational message.
    pub fn info(message: impl std::fmt::Display) {
        tracing::info!("{}", message);
    }

    /// Log informational message with context.
    pub fn info_with(message: impl std::fmt::Display, context: &serde_json::Value) {
        tracing::info!(context = %context, "{}", message);
    }

    /// Log error message.
    pub fn error(message: impl std::fmt::Display) {
        tracing::error!("{}", message);
    }

    /// Log error message with context.
    pub fn error_with(message: impl std::fmt::Display, context: &serde_json::Value) {
        tracing::error!(context = %context, "{}", message);
    }

    /// Log warning message (alias for warn).
    pub fn warning(message: impl std::fmt::Display) {
        tracing::warn!("{}", message);
    }

    /// Log warning message.
    pub fn warn(message: impl std::fmt::Display) {
        tracing::warn!("{}", message);
    }

    /// Log warning message with context.
    pub fn warn_with(message: impl std::fmt::Display, context: &serde_json::Value) {
        tracing::warn!(context = %context, "{}", message);
    }

    /// Log debug message.
    pub fn debug(message: impl std::fmt::Display) {
        tracing::debug!("{}", message);
    }

    /// Log debug message with context.
    pub fn debug_with(message: impl std::fmt::Display, context: &serde_json::Value) {
        tracing::debug!(context = %context, "{}", message);
    }

    /// Log critical message.
    pub fn critical(message: impl std::fmt::Display) {
        tracing::error!(target: "critical", "CRITICAL: {}", message);
    }

    /// Log critical message with context.
    pub fn critical_with(message: impl std::fmt::Display, context: &serde_json::Value) {
        tracing::error!(target: "critical", context = %context, "CRITICAL: {}", message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_logger_methods_callable() {
        // Test basic logging functions
        Logger::info("test info message");
        Logger::warning("test warning message");
        Logger::warn("test warn message");
        Logger::error("test error message");
        Logger::debug("test debug message");
        Logger::critical("test critical message");

        // Test with context
        let ctx = json!({ "user_id": 42, "ip": "127.0.0.1" });
        Logger::info_with("test info with ctx", &ctx);
        Logger::warn_with("test warn with ctx", &ctx);
        Logger::error_with("test error with ctx", &ctx);
        Logger::debug_with("test debug with ctx", &ctx);
        Logger::critical_with("test critical with ctx", &ctx);
    }

    #[test]
    fn test_logger_rotation() {
        let temp_dir = std::env::temp_dir().join("padi_logger_test");
        let _ = fs::create_dir_all(&temp_dir);

        let dummy_log = temp_dir.join("app.log");
        let _ = fs::write(&dummy_log, "sample log");

        Logger::rotate(temp_dir.to_str().unwrap(), 14);
        assert!(dummy_log.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}


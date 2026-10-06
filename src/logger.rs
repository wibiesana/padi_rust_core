//! Logger - daily file logs (app-*.log, error-*.log) + stdout (port of Logger.php).
use std::{fs, sync::OnceLock, time::Duration};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{filter::LevelFilter, prelude::*, fmt};

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
}

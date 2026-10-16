//! Queue - Database-backed job queue for Padi Rust Api Framework.
//!
//! Features:
//! - Multi-database support (`sqlite`, `pgsql`, `mysql`).
//! - Runs asynchronously over existing database pool without requiring external message brokers.
//! - Async worker loop with graceful restart (`QUEUE_MAX_JOBS`), retry backoff, and max attempts.
//! - Registration of async job handlers (`Queue::register`, `Queue::register_sync`).

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock, RwLock},
    time::Duration,
};

use serde_json::{json, Value};

use crate::{
    db::{Database, Val},
    env,
    error::ApiError,
    logger::Logger,
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
pub type AsyncJobHandler = Arc<dyn Fn(Value) -> BoxFuture<'static, Result<(), ApiError>> + Send + Sync>;

static HANDLERS: LazyLock<RwLock<HashMap<String, AsyncJobHandler>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[inline]
fn current_time_secs() -> i64 {
    crate::now_secs() as i64
}

pub struct Queue;

impl Queue {
    /// Ensure the `jobs` database table exists.
    pub async fn init_table() -> Result<(), ApiError> {
        let sql = if Database::is_postgres() {
            "CREATE TABLE IF NOT EXISTS jobs (
                id SERIAL PRIMARY KEY,
                queue VARCHAR(255) NOT NULL,
                payload TEXT NOT NULL,
                attempts SMALLINT DEFAULT 0,
                reserved_at INTEGER NULL,
                available_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            )"
        } else if Database::is_sqlite() {
            "CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                queue VARCHAR(255) NOT NULL,
                payload TEXT NOT NULL,
                attempts INTEGER DEFAULT 0,
                reserved_at INTEGER NULL,
                available_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            )"
        } else {
            "CREATE TABLE IF NOT EXISTS jobs (
                id INT AUTO_INCREMENT PRIMARY KEY,
                queue VARCHAR(255) NOT NULL,
                payload LONGTEXT NOT NULL,
                attempts TINYINT DEFAULT 0,
                reserved_at INT NULL,
                available_at INT NOT NULL,
                created_at INT NOT NULL,
                INDEX idx_queue_available (queue, available_at, reserved_at)
            )"
        };

        Database::execute(sql, &[]).await?;
        Ok(())
    }

    /// Register an async job handler by name.
    ///
    /// # Example
    /// ```rust,no_run
    /// use padi_core::queue::Queue;
    ///
    /// Queue::register("SendWelcomeEmail", |data| Box::pin(async move {
    ///     println!("Sending welcome email to: {:?}", data);
    ///     Ok(())
    /// }));
    /// ```
    pub fn register<F>(job_name: &str, handler: F)
    where
        F: Fn(Value) -> BoxFuture<'static, Result<(), ApiError>> + Send + Sync + 'static,
    {
        if let Ok(mut handlers) = HANDLERS.write() {
            handlers.insert(job_name.to_string(), Arc::new(handler));
        }
    }

    /// Register a synchronous closure as a job handler.
    pub fn register_sync<F>(job_name: &str, handler: F)
    where
        F: Fn(Value) -> Result<(), ApiError> + Send + Sync + 'static,
    {
        let handler = Arc::new(handler);
        Self::register(job_name, move |data| {
            let h = Arc::clone(&handler);
            Box::pin(async move { h(data) })
        });
    }

    /// Push a job onto the queue.
    ///
    /// Port of `Queue::push(string $jobClass, array $data = [], string $queue = 'default', int $delay = 0)`.
    pub async fn push(
        job_name: &str,
        data: Value,
        queue: Option<&str>,
        delay_seconds: Option<i64>,
    ) -> Result<i64, ApiError> {
        Self::init_table().await?;

        let queue_name = queue.unwrap_or("default");
        let delay = delay_seconds.unwrap_or(0);
        let now = current_time_secs();
        let available_at = now + delay;

        let payload = json!({
            "job": job_name,
            "data": data,
        })
        .to_string();

        let sql = "INSERT INTO jobs (queue, payload, available_at, created_at) VALUES (?, ?, ?, ?)";
        let params = vec![
            Val::from(queue_name),
            Val::from(payload.as_str()),
            Val::Int(available_at),
            Val::Int(now),
        ];

        let (_, last_id) = Database::execute(sql, &params).await?;
        Ok(last_id.unwrap_or(0))
    }

    /// Push a job to be executed after a specified delay.
    pub async fn later(
        delay_seconds: i64,
        job_name: &str,
        data: Value,
        queue: Option<&str>,
    ) -> Result<i64, ApiError> {
        Self::push(job_name, data, queue, Some(delay_seconds)).await
    }

    /// Count of pending available jobs in a queue.
    pub async fn size(queue: Option<&str>) -> Result<i64, ApiError> {
        Self::init_table().await?;
        let queue_name = queue.unwrap_or("default");
        let now = current_time_secs();

        let sql = "SELECT COUNT(*) AS count FROM jobs WHERE queue = ? AND reserved_at IS NULL AND available_at <= ?";
        let row = Database::fetch_one(sql, &[Val::from(queue_name), Val::Int(now)]).await?;
        Ok(row.and_then(|r| r["count"].as_i64()).unwrap_or(0))
    }

    /// Clear all pending jobs from a queue.
    pub async fn clear(queue: Option<&str>) -> Result<u64, ApiError> {
        Self::init_table().await?;
        let queue_name = queue.unwrap_or("default");
        let (affected, _) = Database::execute("DELETE FROM jobs WHERE queue = ?", &[Val::from(queue_name)]).await?;
        Ok(affected)
    }

    /// Process the next available job on the queue. Returns `Ok(true)` if a job was processed,
    /// or `Ok(false)` if no jobs were available.
    pub async fn run_next(queue: Option<&str>) -> Result<bool, ApiError> {
        Self::init_table().await?;
        let queue_name = queue.unwrap_or("default");
        let now = current_time_secs();
        let max_attempts = env::get_int("QUEUE_MAX_ATTEMPTS", 3);
        let timeout_window = now - 300; // Allow reclaiming jobs orphaned by dead workers > 5 min

        // 1. Fetch next available job
        let sql = "SELECT id, queue, payload, attempts FROM jobs WHERE queue = ? AND available_at <= ? AND (reserved_at IS NULL OR reserved_at < ?) ORDER BY id ASC LIMIT 1";
        let row = Database::fetch_one(sql, &[Val::from(queue_name), Val::Int(now), Val::Int(timeout_window)]).await?;

        let Some(job) = row else {
            return Ok(false);
        };

        let job_id = job["id"].as_i64().unwrap_or(0);
        let attempts = job["attempts"].as_i64().unwrap_or(0);
        let payload_str = job["payload"].as_str().unwrap_or("{}");

        // 2. Reserve job atomically
        let (reserved, _) = Database::execute(
            "UPDATE jobs SET reserved_at = ?, attempts = attempts + 1 WHERE id = ? AND (reserved_at IS NULL OR reserved_at < ?)",
            &[Val::Int(now), Val::Int(job_id), Val::Int(timeout_window)],
        )
        .await?;

        if reserved == 0 {
            // Another worker claimed it first
            return Ok(false);
        }

        // 3. Dispatch and execute
        Logger::info(format!("Processing queue job #{job_id}..."));
        let success = Self::dispatch_payload(payload_str).await;

        if success {
            Database::execute("DELETE FROM jobs WHERE id = ?", &[Val::Int(job_id)]).await?;
            Logger::info(format!("Job #{job_id} DONE"));
        } else if attempts + 1 >= max_attempts {
            Database::execute("DELETE FROM jobs WHERE id = ?", &[Val::Int(job_id)]).await?;
            Logger::error(format!("Job #{job_id} DELETED (Max attempts {max_attempts} reached)"));
        } else {
            let retry_delay = (attempts + 1) * 15; // 15s, 30s, etc. backoff
            let retry_available = now + retry_delay;
            Database::execute(
                "UPDATE jobs SET reserved_at = NULL, available_at = ? WHERE id = ?",
                &[Val::Int(retry_available), Val::Int(job_id)],
            )
            .await?;
            Logger::warning(format!("Job #{job_id} FAILED (Will retry in {retry_delay}s)"));
        }

        Ok(true)
    }

    /// Run the queue worker loop.
    ///
    /// Supports:
    /// - `queue`: queue name (default `"default"`)
    /// - `once`: process one job then return
    /// - `stop_when_empty`: terminate when no pending jobs remain
    pub async fn work(
        queue: Option<&str>,
        once: bool,
        stop_when_empty: bool,
    ) -> Result<(), ApiError> {
        let queue_name = queue.unwrap_or("default");
        let sleep_duration = Duration::from_millis(
            (env::get_float("QUEUE_SLEEP", 3.0).max(0.01) * 1000.0) as u64,
        );
        let max_jobs = env::get_int("QUEUE_MAX_JOBS", 1000) as u64;

        Logger::info(format!("Queue worker started on queue [{queue_name}]"));

        let mut processed_count = 0u64;

        loop {
            match Self::run_next(Some(queue_name)).await {
                Ok(true) => {
                    processed_count += 1;
                    if once {
                        return Ok(());
                    }
                    if max_jobs > 0 && processed_count >= max_jobs {
                        Logger::info(format!("Worker processed {max_jobs} jobs — stopping for hygiene."));
                        return Ok(());
                    }
                }
                Ok(false) => {
                    if once || stop_when_empty {
                        Logger::info(format!("No jobs available on queue [{queue_name}]. Exiting."));
                        return Ok(());
                    }
                    tokio::time::sleep(sleep_duration).await;
                }
                Err(err) => {
                    Logger::error(format!("Queue worker error: {err}"));
                    if once || stop_when_empty {
                        return Err(err);
                    }
                    tokio::time::sleep(sleep_duration).await;
                }
            }
        }
    }

    async fn dispatch_payload(payload_str: &str) -> bool {
        let parsed: Value = match serde_json::from_str(payload_str) {
            Ok(v) => v,
            Err(e) => {
                Logger::error(format!("Invalid queue job JSON payload: {e}"));
                return false;
            }
        };

        let job_name = parsed["job"].as_str().unwrap_or("");
        let data = parsed.get("data").cloned().unwrap_or(Value::Null);

        let handler = {
            let handlers = match HANDLERS.read() {
                Ok(h) => h,
                Err(_) => return false,
            };
            handlers.get(job_name).cloned()
        };

        let Some(handler) = handler else {
            Logger::error(format!("No registered handler found for queue job: '{job_name}'"));
            return false;
        };

        match (handler)(data).await {
            Ok(_) => true,
            Err(e) => {
                Logger::error(format!("Queue job '{job_name}' failed execution: {e}"));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn test_register_and_dispatch() {
        static CALL_COUNT: AtomicU32 = AtomicU32::new(0);

        Queue::register_sync("TestEchoJob", |data| {
            if data["count"] == 1 {
                CALL_COUNT.fetch_add(1, Ordering::SeqCst);
                Ok(())
            } else {
                Err(ApiError::bad_request("bad count"))
            }
        });

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let valid = json!({ "job": "TestEchoJob", "data": { "count": 1 } }).to_string();
            assert!(Queue::dispatch_payload(&valid).await);
            assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 1);

            let invalid = json!({ "job": "TestEchoJob", "data": { "count": 99 } }).to_string();
            assert!(!Queue::dispatch_payload(&invalid).await);

            let unknown = json!({ "job": "UnknownJob404", "data": {} }).to_string();
            assert!(!Queue::dispatch_payload(&unknown).await);
        });
    }
}

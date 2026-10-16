//! Database - sqlx `Any` pool (MySQL / PostgreSQL / SQLite). Port of Database.php.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};

use serde_json::{json, Map, Value};
use sqlx::{any::AnyPoolOptions, AnyPool, Column, Row};

use crate::{env, error::ApiError};

static POOL: OnceLock<AnyPool> = OnceLock::new();
static POSTGRES: OnceLock<bool> = OnceLock::new();
static SQLITE: OnceLock<bool> = OnceLock::new();
static QUERY_COUNT: AtomicU64 = AtomicU64::new(0);

/// A bindable SQL value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

impl From<&str> for Val { fn from(v: &str) -> Self { Val::Str(v.into()) } }
impl From<String> for Val { fn from(v: String) -> Self { Val::Str(v) } }
impl From<i64> for Val { fn from(v: i64) -> Self { Val::Int(v) } }
impl From<i32> for Val { fn from(v: i32) -> Self { Val::Int(v as i64) } }
impl From<i16> for Val { fn from(v: i16) -> Self { Val::Int(v as i64) } }
impl From<i8> for Val { fn from(v: i8) -> Self { Val::Int(v as i64) } }
impl From<u64> for Val { fn from(v: u64) -> Self { Val::Int(v as i64) } }
impl From<u32> for Val { fn from(v: u32) -> Self { Val::Int(v as i64) } }
impl From<u16> for Val { fn from(v: u16) -> Self { Val::Int(v as i64) } }
impl From<u8> for Val { fn from(v: u8) -> Self { Val::Int(v as i64) } }
impl From<usize> for Val { fn from(v: usize) -> Self { Val::Int(v as i64) } }
impl From<f64> for Val { fn from(v: f64) -> Self { Val::Float(v) } }
impl From<f32> for Val { fn from(v: f32) -> Self { Val::Float(v as f64) } }
impl From<bool> for Val { fn from(v: bool) -> Self { Val::Bool(v) } }
impl<T: Into<Val>> From<Option<T>> for Val {
    fn from(v: Option<T>) -> Self { v.map(Into::into).unwrap_or(Val::Null) }
}

impl std::fmt::Display for Val {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Val::Null => write!(f, "NULL"),
            Val::Bool(b) => write!(f, "{b}"),
            Val::Int(i) => write!(f, "{i}"),
            Val::Float(fl) => write!(f, "{fl}"),
            Val::Str(s) => write!(f, "{s}"),
        }
    }
}

impl From<&Value> for Val {
    fn from(v: &Value) -> Self {
        match v {
            Value::Null => Val::Null,
            Value::Bool(b) => Val::Bool(*b),
            Value::Number(n) => n.as_i64().map(Val::Int).unwrap_or_else(|| Val::Float(n.as_f64().unwrap_or(0.0))),
            Value::String(s) => Val::Str(s.clone()),
            other => Val::Str(other.to_string()),
        }
    }
}
impl From<Value> for Val { fn from(v: Value) -> Self { (&v).into() } }

impl From<&Val> for Value {
    fn from(v: &Val) -> Self {
        match v {
            Val::Null => Value::Null,
            Val::Bool(b) => Value::Bool(*b),
            Val::Int(i) => json!(i),
            Val::Float(f) => json!(f),
            Val::Str(s) => Value::String(s.clone()),
        }
    }
}
impl From<Val> for Value {
    fn from(v: Val) -> Self {
        (&v).into()
    }
}

/// A recorded SQL query profile entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueryLog {
    pub sql: String,
    pub bindings: Vec<Value>,
    pub duration: String,
    pub duration_ms: f64,
}

tokio::task_local! {
    pub static REQUEST_QUERIES: std::sync::Arc<std::sync::Mutex<Vec<QueryLog>>>;
}

/// Validate whether an identifier is safe (alphanumeric, underscores, dots).
#[inline]
pub fn is_valid_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

pub struct Database;

impl Database {
    /// Connect using DATABASE_URL (or build one from DB_* variables).
    pub async fn connect() -> Result<(), ApiError> {
        sqlx::any::install_default_drivers();
        let url = Self::url();
        let _ = POSTGRES.set(url.starts_with("postgres"));
        let _ = SQLITE.set(url.starts_with("sqlite"));
        let pool = AnyPoolOptions::new()
            .max_connections(env::get_int("DB_MAX_CONNECTIONS", 10) as u32)
            .connect(&url)
            .await?;
        let _ = POOL.set(pool);
        Ok(())
    }

    fn url() -> String {
        if let Some(u) = env::get("DATABASE_URL") {
            return u;
        }
        let driver = env::get_or("DB_DRIVER", "sqlite");
        if driver == "sqlite" {
            return format!("sqlite://{}?mode=rwc", env::get_or("DB_DATABASE", "database/app.db"));
        }
        format!(
            "{}://{}:{}@{}:{}/{}",
            if driver == "pgsql" { "postgres" } else { "mysql" },
            env::get_or("DB_USERNAME", "root"),
            env::get_or("DB_PASSWORD", ""),
            env::get_or("DB_HOST", "127.0.0.1"),
            env::get_or("DB_PORT", if driver == "pgsql" { "5432" } else { "3306" }),
            env::get_or("DB_DATABASE", "padi"),
        )
    }

    pub fn pool() -> Result<&'static AnyPool, ApiError> {
        POOL.get().ok_or_else(|| ApiError::internal("Database not connected"))
    }

    pub fn is_postgres() -> bool {
        *POSTGRES.get().unwrap_or(&false)
    }

    pub fn is_sqlite() -> bool {
        *SQLITE.get().unwrap_or(&false)
    }

    fn record_query(sql: &str, params: &[Val], start: std::time::Instant) {
        QUERY_COUNT.fetch_add(1, Ordering::Relaxed);
        let duration = start.elapsed();
        let ms = (duration.as_secs_f64() * 1000.0 * 100.0).round() / 100.0;
        let _ = REQUEST_QUERIES.try_with(|cell| {
            if let Ok(mut logs) = cell.lock() {
                logs.push(QueryLog {
                    sql: sql.to_string(),
                    bindings: params.iter().map(Value::from).collect(),
                    duration: format!("{:.2}ms", ms),
                    duration_ms: ms,
                });
            }
        });
    }

    pub fn query_count() -> u64 {
        REQUEST_QUERIES.try_with(|cell| {
            cell.lock().map(|logs| logs.len() as u64).unwrap_or(0)
        }).unwrap_or_else(|_| QUERY_COUNT.load(Ordering::Relaxed))
    }

    pub fn total_query_count() -> u64 {
        QUERY_COUNT.load(Ordering::Relaxed)
    }

    pub fn queries() -> Vec<QueryLog> {
        REQUEST_QUERIES.try_with(|cell| {
            cell.lock().map(|logs| logs.clone()).unwrap_or_default()
        }).unwrap_or_default()
    }

    /// Rewrite `?` placeholders to `$n` for PostgreSQL.
    fn prepare(sql: &str) -> String {
        if !Self::is_postgres() {
            return sql.to_string();
        }
        let mut out = String::with_capacity(sql.len() + 8);
        let mut n = 0;
        for c in sql.chars() {
            if c == '?' {
                n += 1;
                out.push('$');
                out.push_str(&n.to_string());
            } else {
                out.push(c);
            }
        }
        out
    }

    fn bind_all<'q>(
        sql: &'q str,
        params: &'q [Val],
    ) -> sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments<'q>> {
        let mut q = sqlx::query(sql);
        for p in params {
            q = match p {
                Val::Null => q.bind(Option::<String>::None),
                Val::Bool(b) => q.bind(*b),
                Val::Int(i) => q.bind(*i),
                Val::Float(f) => q.bind(*f),
                Val::Str(s) => q.bind(s.as_str()),
            };
        }
        q
    }

    pub async fn fetch_all(sql: &str, params: &[Val]) -> Result<Vec<Value>, ApiError> {
        let sql = Self::prepare(sql);
        let start = std::time::Instant::now();
        let res = Self::bind_all(&sql, params).fetch_all(Self::pool()?).await;
        Self::record_query(&sql, params, start);
        let rows = res?;
        Ok(rows.iter().map(row_to_json).collect())
    }

    pub async fn fetch_one(sql: &str, params: &[Val]) -> Result<Option<Value>, ApiError> {
        Ok(Self::fetch_all(sql, params).await?.into_iter().next())
    }

    /// Returns (rows_affected, last_insert_id when the driver supports it).
    pub async fn execute(sql: &str, params: &[Val]) -> Result<(u64, Option<i64>), ApiError> {
        let sql = Self::prepare(sql);
        let start = std::time::Instant::now();
        let res = Self::bind_all(&sql, params).execute(Self::pool()?).await;
        Self::record_query(&sql, params, start);
        let r = res?;
        Ok((r.rows_affected(), r.last_insert_id()))
    }
}

fn row_to_json(row: &sqlx::any::AnyRow) -> Value {
    let mut map = Map::new();
    for (i, col) in row.columns().iter().enumerate() {
        let v = if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
            v.map(|x| json!(x))
        } else if let Ok(v) = row.try_get::<Option<bool>, _>(i) {
            v.map(|x| json!(x))
        } else if let Ok(v) = row.try_get::<Option<f64>, _>(i) {
            v.map(|x| json!(x))
        } else if let Ok(v) = row.try_get::<Option<String>, _>(i) {
            v.map(|x| json!(x))
        } else {
            None
        };
        map.insert(col.name().to_string(), v.unwrap_or(Value::Null));
    }
    Value::Object(map)
}

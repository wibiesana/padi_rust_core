//! ApiError - unified error type (replaces exceptions + ValidationException).
use std::collections::BTreeMap;

use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::{env, response};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{message}")]
    Http { status: u16, message: String },
    #[error("Validation failed")]
    Validation(BTreeMap<String, Vec<String>>),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl ApiError {
    pub fn http(status: u16, message: impl Into<String>) -> Self {
        Self::Http { status, message: message.into() }
    }
    pub fn bad_request(m: impl Into<String>) -> Self { Self::http(400, m) }
    pub fn unauthorized(m: impl Into<String>) -> Self { Self::http(401, m) }
    pub fn forbidden(m: impl Into<String>) -> Self { Self::http(403, m) }
    pub fn not_found(m: impl Into<String>) -> Self { Self::http(404, m) }
    pub fn internal(m: impl Into<String>) -> Self { Self::http(500, m) }

    pub fn status(&self) -> u16 {
        match self {
            Self::Http { status, .. } => *status,
            Self::Validation(_) => 422,
            Self::Db(_) | Self::Io(_) => 500,
            Self::Json(_) => 400,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        // Never leak DB internals outside debug mode.
        let message = match &self {
            Self::Db(e) => {
                tracing::error!("database error: {e}");
                if env::is_debug() { e.to_string() } else { "An error occurred".to_string() }
            }
            other => other.to_string(),
        };
        let mut map = serde_json::Map::new();
        map.insert("message".to_string(), json!(message));
        map.insert("message_code".to_string(), json!(response::status_code_name(status)));
        map.insert("success".to_string(), json!(false));
        if let Self::Validation(errors) = &self {
            map.insert("errors".to_string(), serde_json::to_value(errors).unwrap_or_default());
        }

        if env::is_debug() || env::get_or("APP_ENV", "production") == "development" {
            let elapsed_str = crate::application::REQUEST_START
                .try_with(|s| format!("{:.2}ms", s.elapsed().as_secs_f64() * 1000.0))
                .unwrap_or_else(|_| "0.00ms".to_string());

            let err_type = match &self {
                Self::Http { .. } => "HttpException",
                Self::Validation(_) => "ValidationException",
                Self::Db(_) => "DatabaseException",
                Self::Io(_) => "IoException",
                Self::Json(_) => "JsonException",
            };

            map.insert(
                "debug".to_string(),
                json!({
                    "env": env::get_or("APP_ENV", "development"),
                    "error_type": err_type,
                    "execution_time": elapsed_str,
                    "queries_count": crate::database::Database::query_count(),
                    "queries": crate::database::Database::queries(),
                    "timestamp": chrono::Local::now().to_rfc3339(),
                }),
            );
        }

        response::json_response(status, &serde_json::Value::Object(map))
    }
}

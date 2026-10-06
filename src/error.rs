//! ApiError - unified error type (replaces exceptions + ValidationException).
use std::collections::BTreeMap;

use axum::response::{IntoResponse, Response};
use serde_json::{json, Value};

use crate::{env, response};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{message}")]
    Http { status: u16, message: String },
    #[error("Validation failed")]
    Validation(BTreeMap<String, Vec<String>>),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
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
            Self::Db(_) => 500,
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
        let mut body = json!({
            "success": false,
            "message": message,
            "message_code": response::status_code_name(status),
        });
        if let Self::Validation(errors) = &self {
            body["errors"] = Value::from(serde_json::to_value(errors).unwrap_or_default());
        }
        response::json_response(status, &body)
    }
}

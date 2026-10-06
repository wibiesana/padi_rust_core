//! Response formatting (port of Router::formatResponse / Response.php).
use axum::{
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

use crate::{env, error::ApiError};

pub type ApiResult = Result<ApiResponse, ApiError>;

pub fn status_code_name(code: u16) -> &'static str {
    match code {
        201 => "CREATED",
        204 => "NO_CONTENT",
        400 => "BAD_REQUEST",
        401 => "UNAUTHORIZED",
        403 => "FORBIDDEN",
        404 => "NOT_FOUND",
        405 => "METHOD_NOT_ALLOWED",
        409 => "CONFLICT",
        422 => "VALIDATION_FAILED",
        429 => "TOO_MANY_REQUESTS",
        500 => "INTERNAL_SERVER_ERROR",
        502 => "BAD_GATEWAY",
        503 => "SERVICE_UNAVAILABLE",
        _ => "SUCCESS",
    }
}

/// A successful handler result. Formatted by RESPONSE_FORMAT: full | simple | raw.
pub struct ApiResponse {
    pub status: u16,
    pub data: Value,
    pub message: String,
}

pub fn ok(data: impl serde::Serialize) -> ApiResult {
    Ok(ApiResponse::new(200, data))
}

pub fn created(data: impl serde::Serialize) -> ApiResult {
    Ok(ApiResponse::new(201, data))
}

pub fn no_content() -> ApiResult {
    Ok(ApiResponse::new(204, Value::Null))
}

impl ApiResponse {
    pub fn new(status: u16, data: impl serde::Serialize) -> Self {
        Self {
            status,
            data: serde_json::to_value(data).unwrap_or(Value::Null),
            message: "Success".into(),
        }
    }

    pub fn message(mut self, m: impl Into<String>) -> Self {
        self.message = m.into();
        self
    }
}

/// Sequential array or paginated `{meta, data}` result.
fn is_collection(v: &Value) -> bool {
    match v {
        Value::Array(_) => true,
        Value::Object(o) => o.contains_key("meta") && o.contains_key("data"),
        _ => false,
    }
}

fn format_body(r: &ApiResponse) -> Value {
    let format = env::get_or("RESPONSE_FORMAT", "full").to_lowercase();
    match format.as_str() {
        "raw" => r.data.clone(),
        "simple" => {
            if r.data.get("status").is_some() {
                r.data.clone()
            } else {
                json!({"status": "success", "code": status_code_name(r.status), "item": r.data})
            }
        }
        _ => {
            if r.data.get("success").is_some() {
                return r.data.clone();
            }
            let mut body = json!({
                "success": true,
                "message": r.message,
                "message_code": status_code_name(r.status),
            });
            if is_collection(&r.data) || !r.data.is_null() {
                body["item"] = r.data.clone();
            }
            body
        }
    }
}

pub fn json_response(status: u16, body: &Value) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status == 204 {
        return code.into_response();
    }
    let bytes = if env::get_or("APP_ENV", "production") == "development" {
        serde_json::to_vec_pretty(body)
    } else {
        serde_json::to_vec(body)
    }
    .unwrap_or_else(|_| b"{\"success\":false,\"message\":\"Response encoding error\",\"message_code\":\"ENCODING_ERROR\"}".to_vec());
    let mut res = (code, bytes).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    res
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> Response {
        json_response(self.status, &format_body(&self))
    }
}

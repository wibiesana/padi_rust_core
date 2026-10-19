//! Response handling and formatting for Padi Rust Api Framework.
//!
//! Provides both idiomatic Axum response constructors (`ok`, `created`, `no_content`,
//! `ApiResponse`, `ApiResult`) and the full-featured builder `Response` / `ResponseBuilder`:
//! - `json(data, status)`
//! - `text(data, status)`
//! - `html(data, status)`
//! - `download(file_path, filename)`
//! - `redirect(url, status)`
//! - `header(key, value)` & `status(code)`
//! - `status_text(code)`

use std::collections::HashMap;

use axum::{
    body::Body,
    http::{header, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response as AxumResponse},
};
use serde::Serialize;
use serde_json::{json, Value};

use crate::{env, error::ApiError};

pub type ApiResult = Result<ApiResponse, ApiError>;

/// Get standard HTTP status canonical reason text.
pub fn status_text(code: u16) -> &'static str {
    StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("Unknown Status")
}

/// Get standardized UPPERCASE error/status code name (e.g. "VALIDATION_FAILED").
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

/// A successful handler result. Formatted according to RESPONSE_FORMAT env: full | simple | raw.
#[derive(Debug, Clone)]
pub struct ApiResponse {
    pub status: u16,
    pub data: Value,
    pub message: String,
    pub headers: HashMap<String, String>,
    pub created_at: std::time::Instant,
}

pub fn ok(data: impl Serialize) -> ApiResult {
    Ok(ApiResponse::new(200, data))
}

pub fn created(data: impl Serialize) -> ApiResult {
    Ok(ApiResponse::new(201, data))
}

pub fn no_content() -> ApiResult {
    Ok(ApiResponse::new(204, Value::Null))
}

impl ApiResponse {
    pub fn new(status: u16, data: impl Serialize) -> Self {
        Self {
            status,
            data: serde_json::to_value(data).unwrap_or(Value::Null),
            message: "Success".into(),
            headers: HashMap::new(),
            created_at: std::time::Instant::now(),
        }
    }

    pub fn message(mut self, m: impl Into<String>) -> Self {
        self.message = m.into();
        self
    }

    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
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
    let mut map = serde_json::Map::new();

    match format.as_str() {
        "raw" => return r.data.clone(),
        "simple" => {
            if let Value::Object(o) = &r.data {
                if o.contains_key("status") {
                    map = o.clone();
                    if let Some(item_obj) = map.get("item").and_then(|v| v.as_object()) {
                        if item_obj.contains_key("data") && item_obj.contains_key("meta") {
                            let items = item_obj["data"].clone();
                            let meta = item_obj["meta"].clone();
                            map.insert("item".to_string(), items);
                            if !map.contains_key("meta") {
                                map.insert("meta".to_string(), meta);
                            }
                        }
                    }
                } else if o.contains_key("meta") && o.contains_key("data") {
                    map.insert("status".to_string(), json!("success"));
                    map.insert("code".to_string(), json!(status_code_name(r.status)));
                    map.insert("item".to_string(), o["data"].clone());
                    map.insert("meta".to_string(), o["meta"].clone());
                } else {
                    map.insert("status".to_string(), json!("success"));
                    map.insert("code".to_string(), json!(status_code_name(r.status)));
                    map.insert("item".to_string(), r.data.clone());
                }
            } else {
                map.insert("status".to_string(), json!("success"));
                map.insert("code".to_string(), json!(status_code_name(r.status)));
                map.insert("item".to_string(), r.data.clone());
            }
        }
        _ => {
            if let Value::Object(o) = &r.data {
                if o.contains_key("success") {
                    map = o.clone();
                    if let Some(item_obj) = map.get("item").and_then(|v| v.as_object()) {
                        if item_obj.contains_key("data") && item_obj.contains_key("meta") {
                            let items = item_obj["data"].clone();
                            let meta = item_obj["meta"].clone();
                            map.insert("item".to_string(), items);
                            if !map.contains_key("meta") {
                                map.insert("meta".to_string(), meta);
                            }
                        }
                    }
                } else if o.contains_key("meta") && o.contains_key("data") {
                    map.insert("message".to_string(), json!(r.message));
                    map.insert("message_code".to_string(), json!(status_code_name(r.status)));
                    map.insert("success".to_string(), json!(true));
                    map.insert("item".to_string(), o["data"].clone());
                    map.insert("meta".to_string(), o["meta"].clone());
                } else {
                    map.insert("message".to_string(), json!(r.message));
                    map.insert("message_code".to_string(), json!(status_code_name(r.status)));
                    map.insert("success".to_string(), json!(true));
                    if is_collection(&r.data) || !r.data.is_null() {
                        map.insert("item".to_string(), r.data.clone());
                    }
                }
            } else {
                map.insert("message".to_string(), json!(r.message));
                map.insert("message_code".to_string(), json!(status_code_name(r.status)));
                map.insert("success".to_string(), json!(true));
                if is_collection(&r.data) || !r.data.is_null() {
                    map.insert("item".to_string(), r.data.clone());
                }
            }
        }
    }

    if env::is_debug() || env::get_or("APP_ENV", "production") == "development" {
        let elapsed_str = crate::application::REQUEST_START
            .try_with(|s| format!("{:.2}ms", s.elapsed().as_secs_f64() * 1000.0))
            .unwrap_or_else(|_| format!("{:.2}ms", r.created_at.elapsed().as_secs_f64() * 1000.0));

        map.insert(
            "debug".to_string(),
            json!({
                "env": env::get_or("APP_ENV", "development"),
                "execution_time": elapsed_str,
                "queries_count": crate::database::Database::query_count(),
                "queries": crate::database::Database::queries(),
                "timestamp": chrono::Local::now().to_rfc3339(),
            }),
        );
    }

    Value::Object(map)
}

/// Generate Axum Response with JSON body and proper Content-Type headers.
pub fn json_response(status: u16, body: &Value) -> AxumResponse {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    if status == 204 {
        return code.into_response();
    }
    let bytes = if env::get_or("APP_ENV", "production") == "development" {
        serde_json::to_vec_pretty(body)
    } else {
        serde_json::to_vec(body)
    }
    .unwrap_or_else(|_| {
        b"{\"success\":false,\"message\":\"Response encoding error\",\"message_code\":\"ENCODING_ERROR\"}".to_vec()
    });

    let mut res = (code, bytes).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    res
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> AxumResponse {
        let mut res = json_response(self.status, &format_body(&self));
        for (k, v) in self.headers {
            if let (Ok(hname), Ok(hval)) = (HeaderName::try_from(k.as_str()), HeaderValue::try_from(v.as_str())) {
                res.headers_mut().insert(hname, hval);
            }
        }
        res
    }
}

// -----------------------------------------------------------------------------
// Fluent Response Builder
// -----------------------------------------------------------------------------

/// Response builder for constructing custom HTTP responses.
#[derive(Debug, Clone, Default)]
pub struct ResponseBuilder {
    status_code: u16,
    headers: HashMap<String, String>,
}

/// Type alias for ergonomic access (`Response::json(...)`, `Response::text(...)`, etc.).
pub type Response = ResponseBuilder;

impl ResponseBuilder {
    pub fn new() -> Self {
        Self {
            status_code: 200,
            headers: HashMap::new(),
        }
    }

    /// Set response header (`response.header(key, value)`).
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Set HTTP status code (`response.status(code)`).
    pub fn status(mut self, code: u16) -> Self {
        self.status_code = code;
        self
    }

    /// Build JSON response (`response.json(data, statusCode)`).
    pub fn json(self, data: impl Serialize, status_code: Option<u16>) -> AxumResponse {
        let code = status_code.unwrap_or(self.status_code);
        let val = serde_json::to_value(data).unwrap_or(Value::Null);
        let mut res = json_response(code, &val);
        self.apply_headers(&mut res);
        res
    }

    /// Static helper for immediate JSON response (`Response::json_data(data, code)`).
    pub fn json_data(data: impl Serialize, code: u16) -> AxumResponse {
        Self::new().status(code).json(data, Some(code))
    }

    /// Build plain text response (`response.text(data, code)`).
    pub fn text(self, data: impl Into<String>, code: Option<u16>) -> AxumResponse {
        let status = StatusCode::from_u16(code.unwrap_or(self.status_code))
            .unwrap_or(StatusCode::OK);
        let text_content = data.into();
        let mut res = (status, text_content).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        self.apply_headers(&mut res);
        res
    }

    /// Static helper for text response.
    pub fn text_data(data: impl Into<String>, code: u16) -> AxumResponse {
        Self::new().text(data, Some(code))
    }

    /// Build HTML response (`response.html(data, code)`).
    pub fn html(self, data: impl Into<String>, code: Option<u16>) -> AxumResponse {
        let status = StatusCode::from_u16(code.unwrap_or(self.status_code))
            .unwrap_or(StatusCode::OK);
        let html_content = data.into();
        let mut res = (status, axum::response::Html(html_content)).into_response();
        self.apply_headers(&mut res);
        res
    }

    /// Send file download (`response.download(file_path, filename)`).
    pub fn download(self, file_path: &str, filename: Option<&str>) -> AxumResponse {
        let path = std::path::Path::new(file_path);
        if !path.exists() {
            return (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "File not found",
            )
                .into_response();
        }

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    "Unable to read file",
                )
                    .into_response();
            }
        };

        let resolved_name = filename
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("download")
                    .to_string()
            });

        // Sanitize filename against header injection
        let safe_filename = resolved_name.replace(['\r', '\n', '"'], "");
        let disposition = format!("attachment; filename=\"{}\"", safe_filename);

        let mut res = (StatusCode::OK, Body::from(bytes)).into_response();
        res.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        if let Ok(disp_val) = HeaderValue::try_from(disposition) {
            res.headers_mut().insert(header::CONTENT_DISPOSITION, disp_val);
        }
        res.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, must-revalidate"),
        );

        self.apply_headers(&mut res);
        res
    }

    /// Redirect to URL (`response.redirect(url, code)`).
    pub fn redirect(self, url: &str, code: Option<u16>) -> AxumResponse {
        let status = match code.unwrap_or(302) {
            301 => StatusCode::MOVED_PERMANENTLY,
            307 => StatusCode::TEMPORARY_REDIRECT,
            308 => StatusCode::PERMANENT_REDIRECT,
            _ => StatusCode::FOUND,
        };

        let mut res = match HeaderValue::try_from(url) {
            Ok(loc) => {
                let mut r = status.into_response();
                r.headers_mut().insert(header::LOCATION, loc);
                r
            }
            Err(_) => (StatusCode::BAD_REQUEST, "Invalid redirect URL").into_response(),
        };

        self.apply_headers(&mut res);
        res
    }

    /// Helper to copy custom headers onto the final Axum response.
    fn apply_headers(&self, res: &mut AxumResponse) {
        for (k, v) in &self.headers {
            if let (Ok(hname), Ok(hval)) = (HeaderName::try_from(k.as_str()), HeaderValue::try_from(v.as_str())) {
                res.headers_mut().insert(hname, hval);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_text() {
        assert_eq!(status_text(200), "OK");
        assert_eq!(status_text(201), "Created");
        assert_eq!(status_text(204), "No Content");
        assert_eq!(status_text(404), "Not Found");
        assert_eq!(status_text(422), "Unprocessable Entity");
        assert_eq!(status_text(500), "Internal Server Error");
    }

    #[test]
    fn test_api_response_format() {
        let resp = ok(json!({"id": 1})).unwrap();
        assert_eq!(resp.status, 200);
        let val = format_body(&resp);
        assert_eq!(val["success"], true);
        assert_eq!(val["item"]["id"], 1);
    }

    #[test]
    fn test_api_response_paginated() {
        let paged_data = json!({
            "data": [{"id": 1, "name": "Work 1"}, {"id": 2, "name": "Work 2"}],
            "meta": {
                "total": 2,
                "per_page": 10,
                "current_page": 1,
                "last_page": 1
            }
        });
        let resp = ok(paged_data).unwrap();
        let val = format_body(&resp);
        assert_eq!(val["success"], true);
        assert!(val["item"].is_array());
        assert_eq!(val["item"].as_array().unwrap().len(), 2);
        assert_eq!(val["item"][0]["name"], "Work 1");
        assert!(val.get("meta").is_some());
        assert_eq!(val["meta"]["total"], 2);
    }

    #[test]
    fn test_api_response_debug_in_dev() {
        std::env::set_var("APP_ENV", "development");
        std::env::set_var("APP_DEBUG", "true");
        let resp = ok(json!({"id": 2})).unwrap();
        let val = format_body(&resp);
        assert_eq!(val["success"], true);
        assert!(val.get("debug").is_some());
        assert_eq!(val["debug"]["env"], "development");
        assert!(val["debug"].get("execution_time").is_some());
        assert!(val["debug"].get("queries_count").is_some());
        assert!(val["debug"].get("queries").is_some());
        assert!(val["debug"].get("timestamp").is_some());
    }

    #[test]
    fn test_response_builder_text() {
        let res = Response::new()
            .header("X-Custom", "test")
            .text("Hello Padi", Some(200));

        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(res.headers().get("x-custom").unwrap(), "test");
    }

    #[test]
    fn test_response_redirect() {
        let res = Response::new().redirect("/login", Some(302));
        assert_eq!(res.status(), StatusCode::FOUND);
        assert_eq!(res.headers().get("location").unwrap(), "/login");
    }
}

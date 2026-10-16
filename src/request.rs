//! Request handler and extractors for Padi Rust Api Framework.
//!
//! Provides both lightweight extractors (`Body`, `Params`) and a full-featured
//! `Request` struct:
//! - Method, URI, Path
//! - Query parameters (`query`, `has`)
//! - Merged inputs (`all`, `input`, `has`, `only`, `except`)
//! - Headers inspection (`header`, `headers`, `bearer_token`, `is_json`)
//! - Client IP resolution with trusted proxy / CIDR validation (`ip`)
//! - Response status code override hook (`set_response_status_code`, `response_status_code`)
//! - Multipart and file upload support representation

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::OnceLock,
};

use axum::{
    extract::{FromRequest, FromRequestParts, Request as AxumRequest},
    http::request::Parts,
};
use serde_json::{Map, Value};

use crate::{env, error::ApiError};

/// Uploaded file information (matching Request.php / File.php file representation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UploadedFile {
    pub name: String,
    pub content_type: Option<String>,
    pub size: usize,
    pub bytes: Vec<u8>,
}

/// Dynamic JSON body extractor (empty object when no body). Invalid JSON => 400.
#[derive(Debug, Clone, Default)]
pub struct Body(pub Value);

impl std::ops::Deref for Body {
    type Target = Value;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<S: Send + Sync> FromRequest<S> for Body {
    type Rejection = ApiError;

    async fn from_request(req: AxumRequest, state: &S) -> Result<Self, Self::Rejection> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|_| ApiError::bad_request("Unable to read request body"))?;
        if bytes.iter().all(|b| b.is_ascii_whitespace()) {
            return Ok(Body(Value::Object(Map::new())));
        }
        serde_json::from_slice(&bytes)
            .map(Body)
            .map_err(|_| ApiError::bad_request("Invalid JSON body"))
    }
}

/// Query-string parameters extractor.
#[derive(Debug, Clone, Default)]
pub struct Params(pub HashMap<String, String>);

impl std::ops::Deref for Params {
    type Target = HashMap<String, String>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Params {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }

    /// `?page=` & `?per_page=` (per_page default 15, max 100).
    pub fn pagination(&self) -> (u64, u64) {
        let page = self.get("page").and_then(|v| v.parse().ok()).unwrap_or(1u64).max(1);
        let per = self.get("per_page").and_then(|v| v.parse().ok()).unwrap_or(15u64).clamp(1, 100);
        (page, per)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let q = parts.uri.query().unwrap_or("");
        let map = parse_query_string(q);
        Ok(Params(map))
    }
}

/// Request encapsulates incoming HTTP request data.
#[derive(Debug, Clone)]
pub struct Request {
    method: String,
    uri: String,
    params: HashMap<String, String>,
    query: HashMap<String, String>,
    body: Value,
    raw_body: Vec<u8>,
    headers: HashMap<String, String>,
    files: HashMap<String, UploadedFile>,
    remote_addr: Option<String>,
    response_status_code: Option<u16>,
    pub user: Option<Value>,
}

impl Request {
    /// Create a new Request manually or from components.
    pub fn new(
        method: impl Into<String>,
        uri: impl Into<String>,
        headers: HashMap<String, String>,
        query: HashMap<String, String>,
        raw_body: Vec<u8>,
    ) -> Self {
        let raw_str = std::str::from_utf8(&raw_body).unwrap_or("");
        let body: Value = if !raw_str.trim().is_empty() {
            serde_json::from_str(raw_str).unwrap_or(Value::Null)
        } else {
            Value::Object(Map::new())
        };

        let uri_str = uri.into();
        let path = normalize_uri_path(&uri_str);

        Self {
            method: method.into(),
            uri: path,
            params: HashMap::new(),
            query,
            body,
            raw_body,
            headers,
            files: HashMap::new(),
            remote_addr: None,
            response_status_code: None,
            user: None,
        }
    }

    /// Set route parameters (e.g. from Axum `Path`).
    pub fn set_params(&mut self, params: HashMap<String, String>) {
        self.params = params;
    }

    /// Get route parameter by key.
    pub fn param(&self, key: &str) -> Option<&str> {
        self.params.get(key).map(|s| s.as_str())
    }

    /// Get HTTP request method (e.g. "GET", "POST").
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Get request URI / path.
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Get query parameter value or default.
    pub fn query<'a>(&'a self, key: &str, default: Option<&'a str>) -> Option<&'a str> {
        self.query.get(key).map(|s| s.as_str()).or(default)
    }

    /// Get all query parameters.
    pub fn query_params(&self) -> &HashMap<String, String> {
        &self.query
    }

    /// Get header value by key (case-insensitive lookup).
    pub fn header(&self, key: &str) -> Option<&str> {
        let key_lower = key.to_ascii_lowercase();
        for (k, v) in &self.headers {
            if k.to_ascii_lowercase() == key_lower {
                return Some(v.as_str());
            }
        }
        None
    }

    /// Get all headers.
    pub fn headers(&self) -> &HashMap<String, String> {
        &self.headers
    }

    /// Get raw body bytes.
    pub fn raw_bytes(&self) -> &[u8] {
        &self.raw_body
    }

    /// Get raw request body as UTF-8 string.
    pub fn raw_body(&self) -> &str {
        std::str::from_utf8(&self.raw_body).unwrap_or("")
    }

    /// Get JSON body parsed as serde_json::Value.
    pub fn body_value(&self) -> &Value {
        &self.body
    }

    /// Get all merged input data (query + body + route params) as a JSON Map.
    pub fn all(&self) -> Map<String, Value> {
        let mut map = Map::new();

        // 1. Query params
        for (k, v) in &self.query {
            map.insert(k.clone(), Value::String(v.clone()));
        }

        // 2. Body (if object)
        if let Value::Object(ref obj) = self.body {
            for (k, v) in obj {
                map.insert(k.clone(), v.clone());
            }
        }

        // 3. Route params
        for (k, v) in &self.params {
            map.insert(k.clone(), Value::String(v.clone()));
        }

        map
    }

    /// Alias for all() matching Request.php raw().
    pub fn raw(&self) -> Map<String, Value> {
        self.all()
    }

    /// Get specific input value by key (body -> query -> params -> default).
    pub fn input(&self, key: &str) -> Option<Value> {
        if let Value::Object(ref obj) = self.body {
            if let Some(v) = obj.get(key) {
                return Some(v.clone());
            }
        }
        if let Some(v) = self.query.get(key) {
            return Some(Value::String(v.clone()));
        }
        if let Some(v) = self.params.get(key) {
            return Some(Value::String(v.clone()));
        }
        None
    }

    /// Get input as string slice or default.
    pub fn input_str<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        if let Value::Object(ref obj) = self.body {
            if let Some(Value::String(s)) = obj.get(key) {
                return s.as_str();
            }
        }
        if let Some(v) = self.query.get(key) {
            return v.as_str();
        }
        if let Some(v) = self.params.get(key) {
            return v.as_str();
        }
        default
    }

    /// Check if input has a key.
    pub fn has(&self, key: &str) -> bool {
        if let Value::Object(ref obj) = self.body {
            if obj.contains_key(key) {
                return true;
            }
        }
        self.query.contains_key(key) || self.params.contains_key(key)
    }

    /// Get only specified keys from merged input.
    pub fn only(&self, keys: &[&str]) -> Map<String, Value> {
        let all = self.all();
        let mut result = Map::new();
        for key in keys {
            if let Some(v) = all.get(*key) {
                result.insert((*key).to_string(), v.clone());
            }
        }
        result
    }

    /// Get all inputs except specified keys.
    pub fn except(&self, keys: &[&str]) -> Map<String, Value> {
        let mut all = self.all();
        for key in keys {
            all.remove(*key);
        }
        all
    }

    /// Get bearer token from Authorization header.
    pub fn bearer_token(&self) -> Option<String> {
        let auth = self.header("Authorization")?;
        if let Some(token) = auth.strip_prefix("Bearer ") {
            return Some(token.trim().to_string());
        }
        if let Some(token) = auth.strip_prefix("bearer ") {
            return Some(token.trim().to_string());
        }
        None
    }

    /// Check if request has JSON Content-Type.
    pub fn is_json(&self) -> bool {
        self.header("Content-Type")
            .map(|c| c.contains("application/json"))
            .unwrap_or(false)
    }

    /// Set uploaded files.
    pub fn set_files(&mut self, files: HashMap<String, UploadedFile>) {
        self.files = files;
    }

    /// Get uploaded file by key.
    pub fn file(&self, key: &str) -> Option<&UploadedFile> {
        self.files.get(key)
    }

    /// Get all uploaded files.
    pub fn files(&self) -> &HashMap<String, UploadedFile> {
        &self.files
    }

    /// Set response status code (used by Controller for auto-formatting).
    pub fn set_response_status_code(&mut self, code: u16) {
        self.response_status_code = Some(code);
    }

    /// Get response status code.
    pub fn response_status_code(&self) -> Option<u16> {
        self.response_status_code
    }

    /// Set client remote IP (e.g. from axum ConnectInfo).
    pub fn set_remote_addr(&mut self, addr: impl Into<String>) {
        self.remote_addr = Some(addr.into());
    }

    /// Get client IP address.
    ///
    /// Only trusts proxy headers (CF-Connecting-IP, X-Real-IP, X-Forwarded-For)
    /// when the direct connection is a known trusted proxy.
    ///
    /// Configure trusted proxies via TRUSTED_PROXIES env var (comma-separated IPs/CIDRs):
    ///   TRUSTED_PROXIES=127.0.0.1,10.0.0.0/8,::1
    pub fn ip(&self) -> String {
        let remote_addr = self
            .remote_addr
            .as_deref()
            .unwrap_or("127.0.0.1");

        // Clean remote_addr in case it has port e.g. "127.0.0.1:8080"
        let direct_ip = extract_ip_str(remote_addr);

        if is_trusted_proxy(&direct_ip) {
            let proxy_headers = ["cf-connecting-ip", "x-real-ip", "x-forwarded-for"];
            for header_name in proxy_headers {
                if let Some(val) = self.header(header_name) {
                    if val.trim().is_empty() {
                        continue;
                    }
                    // For X-Forwarded-For, take the leftmost IP
                    let candidate = val.split(',').next().unwrap_or("").trim();
                    if candidate.parse::<IpAddr>().is_ok() {
                        return candidate.to_string();
                    }
                }
            }
        } else if env::get_or("APP_ENV", "production") == "development" {
            let has_proxy_header = self.header("x-forwarded-for").is_some()
                || self.header("x-real-ip").is_some()
                || self.header("cf-connecting-ip").is_some();

            if has_proxy_header {
                tracing::warn!(
                    "[padi] WARNING: Proxy headers detected but remote connection ({}) is not in TRUSTED_PROXIES. ip() returns direct IP. Add to TRUSTED_PROXIES in .env",
                    direct_ip
                );
            }
        }

        if direct_ip.parse::<IpAddr>().is_ok() {
            direct_ip
        } else {
            "0.0.0.0".to_string()
        }
    }
}

/// Axum extractor implementation for `Request`.
impl<S: Send + Sync> FromRequest<S> for Request {
    type Rejection = ApiError;

    async fn from_request(req: AxumRequest, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let bytes = axum::body::Bytes::from_request(AxumRequest::from_parts(parts.clone(), body), state)
            .await
            .map_err(|_| ApiError::bad_request("Unable to read request body"))?;

        let mut headers_map = HashMap::new();
        for (k, v) in parts.headers.iter() {
            if let Ok(val_str) = v.to_str() {
                headers_map.insert(k.as_str().to_string(), val_str.to_string());
            }
        }

        let query_map = parse_query_string(parts.uri.query().unwrap_or(""));

        let mut request = Request::new(
            parts.method.as_str(),
            parts.uri.to_string(),
            headers_map,
            query_map,
            bytes.to_vec(),
        );

        // Populate remote IP if forwarded by proxy or connect info extension
        if let Some(direct_header) = parts.headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
            request.set_remote_addr(direct_header);
        }

        Ok(request)
    }
}

// -----------------------------------------------------------------------------
// Helper Functions: CIDR & IP validation, Query String parsing
// -----------------------------------------------------------------------------

static TRUSTED_PROXIES: OnceLock<Vec<String>> = OnceLock::new();

fn get_trusted_proxies() -> &'static [String] {
    TRUSTED_PROXIES.get_or_init(|| {
        let env_val = env::get_or("TRUSTED_PROXIES", "127.0.0.1,::1");
        env_val
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
}

fn is_trusted_proxy(ip_str: &str) -> bool {
    let target_ip: IpAddr = match ip_str.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };

    for proxy in get_trusted_proxies() {
        if proxy == ip_str {
            return true;
        }

        if proxy.contains('/')
            && ip_in_cidr(&target_ip, proxy) {
                return true;
            }
    }

    false
}

fn ip_in_cidr(target: &IpAddr, cidr: &str) -> bool {
    let (subnet_str, bits_str) = match cidr.split_once('/') {
        Some(parts) => parts,
        None => return false,
    };

    let bits: u32 = match bits_str.parse() {
        Ok(b) => b,
        Err(_) => return false,
    };

    let subnet: IpAddr = match subnet_str.parse() {
        Ok(s) => s,
        Err(_) => return false,
    };

    match (target, subnet) {
        (IpAddr::V4(t), IpAddr::V4(s)) => {
            if bits > 32 {
                return false;
            }
            if bits == 0 {
                return true;
            }
            let mask = !((1u32 << (32 - bits)) - 1);
            (u32::from(*t) & mask) == (u32::from(s) & mask)
        }
        (IpAddr::V6(t), IpAddr::V6(s)) => {
            if bits > 128 {
                return false;
            }
            if bits == 0 {
                return true;
            }
            let t_u128 = u128::from(*t);
            let s_u128 = u128::from(s);
            let mask = !((1u128 << (128 - bits)) - 1);
            (t_u128 & mask) == (s_u128 & mask)
        }
        _ => false,
    }
}

fn extract_ip_str(raw: &str) -> String {
    let s = raw.trim();
    if let Ok(ip) = s.parse::<IpAddr>() {
        return ip.to_string();
    }
    // Handle "host:port" IPv4
    if let Some((ip, _)) = s.split_once(':') {
        if let Ok(ip_v4) = ip.parse::<Ipv4Addr>() {
            return ip_v4.to_string();
        }
    }
    // Handle "[::1]:port" IPv6
    if s.starts_with('[') {
        if let Some(end_bracket) = s.find(']') {
            let inner = &s[1..end_bracket];
            if let Ok(ip_v6) = inner.parse::<Ipv6Addr>() {
                return ip_v6.to_string();
            }
        }
    }
    s.to_string()
}

fn normalize_uri_path(uri: &str) -> String {
    let path = uri.split('?').next().unwrap_or("/");
    let path = if path.starts_with("//") {
        format!("/{}", path.trim_start_matches('/'))
    } else {
        path.to_string()
    };
    // replace consecutive slashes
    let mut cleaned = String::new();
    let mut prev_slash = false;
    for c in path.chars() {
        if c == '/' {
            if !prev_slash {
                cleaned.push(c);
            }
            prev_slash = true;
        } else {
            cleaned.push(c);
            prev_slash = false;
        }
    }
    if cleaned.is_empty() {
        "/".to_string()
    } else {
        cleaned
    }
}

fn parse_query_string(q: &str) -> HashMap<String, String> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            Some((pct_decode(k)?, pct_decode(v)?))
        })
        .collect()
}

fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3]).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 2;
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query() {
        let q = parse_query_string("name=Padi%20Framework&tag=rust&page=2");
        assert_eq!(q.get("name").unwrap(), "Padi Framework");
        assert_eq!(q.get("tag").unwrap(), "rust");
        assert_eq!(q.get("page").unwrap(), "2");
    }

    #[test]
    fn test_request_inputs() {
        let mut req = Request::new(
            "POST",
            "/api/users?sort=asc",
            HashMap::new(),
            parse_query_string("sort=asc"),
            br#"{"name":"Alice","role":"admin"}"#.to_vec(),
        );

        let mut params = HashMap::new();
        params.insert("id".to_string(), "42".to_string());
        req.set_params(params);

        assert_eq!(req.method(), "POST");
        assert_eq!(req.uri(), "/api/users");
        assert_eq!(req.input_str("name", ""), "Alice");
        assert_eq!(req.input_str("sort", ""), "asc");
        assert_eq!(req.param("id"), Some("42"));
        assert!(req.has("name"));
        assert!(req.has("sort"));
        assert!(req.has("id"));
        assert!(!req.has("nonexistent"));

        let only = req.only(&["name", "id"]);
        assert_eq!(only.len(), 2);
        assert_eq!(only.get("name").unwrap(), &Value::String("Alice".into()));

        let except = req.except(&["role", "raw_body"]);
        assert!(!except.contains_key("role"));
    }

    #[test]
    fn test_bearer_token() {
        let mut headers = HashMap::new();
        headers.insert("Authorization".to_string(), "Bearer secret-token-xyz".to_string());
        let req = Request::new("GET", "/", headers, HashMap::new(), vec![]);
        assert_eq!(req.bearer_token().as_deref(), Some("secret-token-xyz"));
    }

    #[test]
    fn test_ip_cidr_check() {
        let v4: IpAddr = "10.0.5.20".parse().unwrap();
        assert!(ip_in_cidr(&v4, "10.0.0.0/8"));
        assert!(!ip_in_cidr(&v4, "192.168.0.0/16"));

        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert!(ip_in_cidr(&v6, "2001:db8::/32"));
        assert!(!ip_in_cidr(&v6, "2001:db9::/32"));
    }
}

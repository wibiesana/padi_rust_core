//! Request extractors (port of Request.php essentials).
use std::collections::HashMap;

use axum::{
    extract::{FromRequest, FromRequestParts, Request},
    http::request::Parts,
};
use serde_json::{Map, Value};

use crate::error::ApiError;

/// JSON body as a dynamic `Value` (empty object when no body). Invalid JSON => 400.
pub struct Body(pub Value);

impl<S: Send + Sync> FromRequest<S> for Body {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
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

/// Query-string parameters.
pub struct Params(pub HashMap<String, String>);

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
        let map = q
            .split('&')
            .filter(|s| !s.is_empty())
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                Some((pct_decode(k)?, pct_decode(v)?))
            })
            .collect();
        Ok(Params(map))
    }
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

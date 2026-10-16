//! Auth - JWT authentication and Axum request extractors for Padi Rust Api Framework.
//!
//! Provides:
//! - Cryptographically secure JWT creation (`Auth::generate_token`)
//! - Signature verification and payload decoding (`Auth::verify_token`)
//! - Header Bearer token extraction (`Auth::extract_token`)
//! - Direct Axum extractors (`AuthUser`) for type-safe route protection
//! - Task-local user scoping for database audit logs (`with_user`)
use std::sync::OnceLock;

use axum::{extract::FromRequestParts, http::request::Parts};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde_json::{json, Value};

use crate::{env, error::ApiError};

const WEAK_SECRETS: &[&str] = &[
    "your-secret-key",
    "change-this",
    "secret",
    "your-secret-key-change-this",
    "jwt-secret",
    "supersecret",
    "12345678901234567890123456789012",
];

struct Keys {
    enc: EncodingKey,
    dec: DecodingKey,
    alg: Algorithm,
}

static KEYS: OnceLock<Result<Keys, String>> = OnceLock::new();

fn keys() -> Result<&'static Keys, ApiError> {
    KEYS.get_or_init(|| {
        let secret = env::get("JWT_SECRET").ok_or("JWT_SECRET is not set")?;
        if secret.len() < 32 {
            return Err(format!("JWT secret must be at least 32 characters long. Current length: {}", secret.len()));
        }
        let lower = secret.to_lowercase();
        if WEAK_SECRETS.iter().any(|w| lower.contains(w)) {
            return Err("JWT secret appears to be using a default or weak value. Please use a cryptographically secure random string.".into());
        }
        let alg = match env::get_or("JWT_ALGORITHM", "HS256").as_str() {
            "HS384" => Algorithm::HS384,
            "HS512" => Algorithm::HS512,
            _ => Algorithm::HS256,
        };
        Ok(Keys {
            enc: EncodingKey::from_secret(secret.as_bytes()),
            dec: DecodingKey::from_secret(secret.as_bytes()),
            alg,
        })
    })
    .as_ref()
    .map_err(|e| ApiError::internal(e.clone()))
}

pub struct Auth;

impl Auth {
    /// Validate JWT configuration at startup.
    pub fn check_config() -> Result<(), ApiError> {
        keys().map(|_| ())
    }

    /// Generate a JWT. `payload` must be a JSON object. `expiry` in seconds.
    pub fn generate_token(mut payload: Value, expiry: i64) -> Result<String, ApiError> {
        let k = keys()?;
        let now = chrono::Utc::now().timestamp();
        let obj = payload.as_object_mut().ok_or_else(|| ApiError::internal("JWT payload must be an object"))?;
        obj.insert("iat".into(), json!(now));
        obj.insert("exp".into(), json!(now + expiry));
        obj.insert("nbf".into(), json!(now));
        encode(&Header::new(k.alg), &payload, &k.enc).map_err(|e| ApiError::internal(e.to_string()))
    }

    /// Decode and verify a JWT. Returns None when invalid/expired.
    pub fn verify_token(token: &str) -> Option<Value> {
        let k = keys().ok()?;
        if token.is_empty() || token.matches('.').count() != 2 {
            return None;
        }
        let mut v = Validation::new(k.alg);
        v.leeway = 60;
        v.validate_nbf = true;
        match decode::<Value>(token, &k.dec, &v) {
            Ok(d) => Some(d.claims),
            Err(e) => {
                if env::get_or("APP_ENV", "production") == "development" && env::get("APP_DEBUG").as_deref() == Some("true") {
                    tracing::debug!("[Auth] JWT verification failed: {e}");
                }
                None
            }
        }
    }

    /// `extractToken()`: bearer token from the request headers.
    pub fn extract_token(parts: &Parts) -> Option<String> {
        bearer_token(parts)
    }

    /// `user()`: decoded claims of the request's bearer token.
    pub fn user(parts: &Parts) -> Option<Value> {
        Self::verify_token(&bearer_token(parts)?)
    }

    /// `userId()`: `user_id` claim of the request's bearer token.
    pub fn user_id(parts: &Parts) -> Option<i64> {
        Self::user(parts).map(AuthUser).and_then(|u| u.user_id())
    }

    /// `reset()`: no-op, Rust keeps no per-request global auth cache.
    pub fn reset() {}
}

/// Extract the `Bearer` token from request headers.
pub fn bearer_token(parts: &Parts) -> Option<String> {
    let h = parts.headers.get("authorization")?.to_str().ok()?;
    h.strip_prefix("Bearer ").map(|t| t.trim().to_string())
}

/// Authenticated user (decoded JWT claims). Using it as a handler argument
/// enforces authentication - the equivalent of AuthMiddleware.
#[derive(Debug, Clone)]
pub struct AuthUser(pub Value);

impl AuthUser {
    /// Run `fut` so ActiveRecord fills `created_by` / `updated_by` with this user
    /// (equivalent of `Auth::userId()` inside `beforeSave`).
    pub async fn run_as<F: std::future::Future>(&self, fut: F) -> F::Output {
        crate::active_record::with_user(self.user_id(), fut).await
    }

    pub fn user_id(&self) -> Option<i64> {
        self.0.get("user_id").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
    }

    pub fn role(&self) -> Option<&str> {
        self.0.get("role").and_then(|v| v.as_str())
    }

    /// Equivalent of `RoleMiddleware:admin,manager`.
    pub fn require_role(&self, roles: &[&str]) -> Result<(), ApiError> {
        match self.role() {
            Some(r) if roles.contains(&r) => Ok(()),
            _ => Err(ApiError::forbidden("Insufficient permissions")),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let token = bearer_token(parts).ok_or_else(|| ApiError::unauthorized("Authentication required"))?;
        Auth::verify_token(&token)
            .map(AuthUser)
            .ok_or_else(|| ApiError::unauthorized("Invalid or expired token"))
    }
}

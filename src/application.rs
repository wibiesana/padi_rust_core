//! Application bootstrap for Padi Rust Api Framework: env, logging, DB pool, CORS,
//! compression, security headers, JSON 404/panic handling and graceful shutdown.
//!
//! Provides:
//! - Full server lifecycle management (`Application::run()`)
//! - Configurable CORS policy (`CorsConfig::from_env()`)
//! - Strict security headers middleware (HSTS, X-Content-Type-Options, Frame protection)
//! - JSON error responses on 404 and uncaught panics
//! - Graceful SIGINT/SIGTERM shutdown signal listeners
use std::{any::Any, time::Duration};

use axum::{
    extract::Request,
    http::{header, HeaderName, HeaderValue, Method, Uri},
    middleware::{self, Next},
    response::Response,
    Router,
};
use serde_json::{json, Value};
use tower_http::{
    catch_panic::CatchPanicLayer,
    compression::CompressionLayer,
    cors::{AllowOrigin, CorsLayer},
};

use crate::{auth::Auth, db::Database, env, error::ApiError, logger::Logger, response};

pub struct Application {
    router: Router,
    root: String,
}

/// Pre-computed CORS origins (`CORS_ALLOWED_ORIGINS`, comma separated, `*` = all).
#[derive(Debug, Clone, Default)]
pub struct CorsConfig {
    pub allow_all: bool,
    pub origins: Vec<String>,
}

impl CorsConfig {
    pub fn from_env() -> Self {
        let raw = env::get_or("CORS_ALLOWED_ORIGINS", "");
        let parsed: Vec<String> = raw
            .split(',')
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .collect();
        if parsed.iter().any(|o| o == "*") {
            Self { allow_all: true, origins: vec![] }
        } else {
            Self { allow_all: false, origins: parsed }
        }
    }
}

async fn not_found(_: Uri) -> Response {
    response::json_response(
        404,
        &json!({"success": false, "message": "Route not found", "message_code": "ROUTE_NOT_FOUND"}),
    )
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".into()
    }
}

/// `handleException()`: generic 500 in production, details only when `APP_DEBUG`.
fn on_panic(debug: bool, p: Box<dyn Any + Send + 'static>) -> Response {
    let msg = panic_message(p.as_ref());
    tracing::error!("handler panicked: {msg}");
    let mut body = json!({
        "success": false,
        "message": "Internal Server Error",
        "message_code": "INTERNAL_SERVER_ERROR",
    });
    if debug {
        body["message"] = Value::String(msg.clone());
        body["debug"] = json!({ "message": msg, "type": "panic" });
    }
    response::json_response(500, &body)
}

/// `sendSecurityHeaders()`: applied to every response.
async fn security_headers(development: bool, req: Request, next: Next) -> Response {
    let https = req
        .headers()
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let mut set = |name: &'static str, value: &'static str| {
        h.insert(HeaderName::from_static(name), HeaderValue::from_static(value));
    };
    set("x-frame-options", "DENY");
    set("x-content-type-options", "nosniff");
    set("x-xss-protection", "0"); // modern browsers: CSP is preferred
    set("referrer-policy", "strict-origin-when-cross-origin");
    set("permissions-policy", "camera=(), microphone=(), geolocation=()");
    // HSTS only on production HTTPS
    if !development && https {
        set("strict-transport-security", "max-age=31536000; includeSubDomains");
    }
    res
}

tokio::task_local! {
    pub static REQUEST_START: std::time::Instant;
}

async fn request_timing_layer(req: Request, next: Next) -> Response {
    let start = std::time::Instant::now();
    let queries = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    crate::database::REQUEST_QUERIES.scope(queries, async move {
        REQUEST_START.scope(start, async move {
            next.run(req).await
        }).await
    }).await
}

impl Application {
    /// `root` is the project directory (holding `.env`, `storage/`, `database/`).
    pub fn new(root: &str, router: Router) -> Self {
        Self { router, root: root.trim_end_matches('/').to_string() }
    }

    /// Check if application is running in development mode (`APP_ENV=development`).
    pub fn is_development() -> bool {
        env::get("APP_ENV").as_deref() == Some("development")
    }

    /// `handleCors()` + preflight: reflect the allowed origin with credentials,
    /// cache preflight for 24h.
    fn cors() -> CorsLayer {
        let cfg = CorsConfig::from_env();
        let dev = Self::is_development();
        CorsLayer::new()
            .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE, Method::OPTIONS, Method::PATCH])
            .allow_headers([
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                HeaderName::from_static("x-requested-with"),
                HeaderName::from_static("x-response-format"),
                header::ACCEPT,
                header::ORIGIN,
            ])
            .allow_credentials(true)
            .max_age(Duration::from_secs(86400))
            .allow_origin(AllowOrigin::predicate(move |origin, _| {
                dev || cfg.allow_all
                    || origin.to_str().is_ok_and(|o| cfg.origins.iter().any(|a| a == o))
            }))
    }

    /// Wrap the user router with framework layers.
    fn build(self) -> Router {
        let dev = Self::is_development();
        let debug = env::is_debug();
        let mut app = self
            .router
            .fallback(not_found)
            .layer(middleware::from_fn(request_timing_layer))
            .layer(middleware::from_fn(move |req: Request, next: Next| async move {
                security_headers(dev, req, next).await
            }))
            .layer(Self::cors())
            .layer(CatchPanicLayer::custom(move |p: Box<dyn Any + Send + 'static>| on_panic(debug, p)));
        if env::get_or("ENABLE_COMPRESSION", "true") == "true" {
            app = app.layer(CompressionLayer::new().gzip(true));
        }
        app
    }

    /// Load env, init logger, validate JWT config, connect DB and serve.
    pub async fn run(self) -> Result<(), ApiError> {
        env::load(format!("{}/.env", self.root));
        Logger::init(&self.root);
        if env::get("JWT_SECRET").is_some() {
            Auth::check_config()?;
        }
        Database::connect().await?;

        let host = env::get_or("APP_HOST", "0.0.0.0");
        let port = env::get_or("APP_PORT", "8085");
        let bind_addr = if host == "localhost" {
            format!("127.0.0.1:{port}")
        } else {
            format!("{host}:{port}")
        };
        let listener = tokio::net::TcpListener::bind(&bind_addr)
            .await
            .map_err(|e| ApiError::internal(format!("cannot bind {bind_addr}: {e}")))?;

        let display_host = if host == "0.0.0.0" || host == "127.0.0.1" {
            "localhost"
        } else {
            &host
        };
        tracing::info!("Padi listening on http://{display_host}:{port}");

        axum::serve(listener, self.build())
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| ApiError::internal(e.to_string()))
    }
}

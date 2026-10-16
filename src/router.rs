//! Router engine and Axum routing helpers for Padi Rust Api Framework.
//!
//! Provides:
//! - Fluent Axum router extension methods (`get_route`, `post_route`, `put_route`, `patch_route`, `delete_route`, `group`, `version`)
//! - Dedicated `Router` builder:
//!     - `router.get("/path", handler)`
//!     - `router.post("/path", handler)`
//!     - `router.group("/prefix", callback)`
//!     - `router.version("v1", callback)`
//!     - `router.any("/path", handler)`
//! - Direct Axum router conversion (`into_axum()`)

use axum::{
    handler::Handler,
    routing::{delete, get, patch, post, put, MethodRouter},
    Router as AxumRouter,
};

/// Extension trait for Axum `Router` to support fluid route definitions.
pub trait RouterExt: Sized {
    /// Mount route group under a path prefix.
    fn group<F>(self, prefix: &str, configure: F) -> Self
    where
        F: FnOnce(AxumRouter) -> AxumRouter;

    /// Mount API versioned route group (e.g. `version("v1", ...)` mounts to `/v1`).
    fn version<F>(self, version: &str, configure: F) -> Self
    where
        F: FnOnce(AxumRouter) -> AxumRouter,
    {
        let v = version.trim_start_matches('v').trim_start_matches('V');
        let prefix = format!("/v{}", v);
        self.group(&prefix, configure)
    }

    /// Add a route responding to GET.
    fn get_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;

    /// Add a route responding to POST.
    fn post_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;

    /// Add a route responding to PUT.
    fn put_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;

    /// Add a route responding to PATCH.
    fn patch_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;

    /// Add a route responding to DELETE.
    fn delete_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static;

    /// Add a route responding to any HTTP standard method.
    fn any_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()> + Clone + Send + 'static,
        T: 'static;
}

impl RouterExt for AxumRouter {
    fn group<F>(self, prefix: &str, configure: F) -> Self
    where
        F: FnOnce(AxumRouter) -> AxumRouter,
    {
        let clean_prefix = format!("/{}", prefix.trim_matches('/'));
        let sub = configure(AxumRouter::new());
        self.nest(&clean_prefix, sub)
    }

    fn get_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route(path, get(handler))
    }

    fn post_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route(path, post(handler))
    }

    fn put_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route(path, put(handler))
    }

    fn patch_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route(path, patch(handler))
    }

    fn delete_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        self.route(path, delete(handler))
    }

    fn any_route<H, T>(self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()> + Clone + Send + 'static,
        T: 'static,
    {
        let mr = MethodRouter::new()
            .get(handler.clone())
            .post(handler.clone())
            .put(handler.clone())
            .patch(handler.clone())
            .delete(handler);
        self.route(path, mr)
    }
}

/// Router builder for fluent route construction.
#[derive(Debug, Default)]
pub struct Router {
    inner: AxumRouter,
    prefix: String,
}

impl Router {
    pub fn new() -> Self {
        Self {
            inner: AxumRouter::new(),
            prefix: String::new(),
        }
    }

    /// Add a GET route.
    pub fn get<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let full_path = self.resolve_path(path);
        self.inner = self.inner.route(&full_path, get(handler));
        self
    }

    /// Add a POST route.
    pub fn post<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let full_path = self.resolve_path(path);
        self.inner = self.inner.route(&full_path, post(handler));
        self
    }

    /// Add a PUT route.
    pub fn put<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let full_path = self.resolve_path(path);
        self.inner = self.inner.route(&full_path, put(handler));
        self
    }

    /// Add a PATCH route.
    pub fn patch<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let full_path = self.resolve_path(path);
        self.inner = self.inner.route(&full_path, patch(handler));
        self
    }

    /// Add a DELETE route.
    pub fn delete<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let full_path = self.resolve_path(path);
        self.inner = self.inner.route(&full_path, delete(handler));
        self
    }

    /// Add a route matching all HTTP methods (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`).
    pub fn any<H, T>(mut self, path: &str, handler: H) -> Self
    where
        H: Handler<T, ()> + Clone + Send + 'static,
        T: 'static,
    {
        let full_path = self.resolve_path(path);
        let mr = MethodRouter::new()
            .get(handler.clone())
            .post(handler.clone())
            .put(handler.clone())
            .patch(handler.clone())
            .delete(handler);
        self.inner = self.inner.route(&full_path, mr);
        self
    }

    /// Add versioned route group (e.g. `version("v1", |r| r.get(...))`).
    pub fn version<F>(self, version: &str, configure: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        let v = version.trim_start_matches('v').trim_start_matches('V');
        let prefix = format!("/v{}", v);
        self.group(&prefix, configure)
    }

    /// Add route group with path prefix.
    pub fn group<F>(mut self, prefix: &str, configure: F) -> Self
    where
        F: FnOnce(Self) -> Self,
    {
        let clean_prefix = format!("/{}", prefix.trim_matches('/'));
        let sub_router = configure(Router::new());
        self.inner = self.inner.nest(&clean_prefix, sub_router.into_inner());
        self
    }

    /// Consume builder and return axum::Router.
    pub fn into_inner(self) -> AxumRouter {
        self.inner
    }

    fn resolve_path(&self, path: &str) -> String {
        let p = path.trim();
        if p.is_empty() || p == "/" {
            if self.prefix.is_empty() {
                "/".to_string()
            } else {
                format!("/{}", self.prefix.trim_matches('/'))
            }
        } else {
            let clean = p.trim_start_matches('/');
            if self.prefix.is_empty() {
                format!("/{}", clean)
            } else {
                format!("/{}/{}", self.prefix.trim_matches('/'), clean)
            }
        }
    }
}

impl From<Router> for AxumRouter {
    fn from(r: Router) -> Self {
        r.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::{ok, ApiResult};

    async fn dummy_handler() -> ApiResult {
        ok("pong")
    }

    #[test]
    fn test_router_builder() {
        let r = Router::new()
            .get("/ping", dummy_handler)
            .post("/ping", dummy_handler)
            .version("v1", |v1| {
                v1.get("/status", dummy_handler)
            })
            .into_inner();

        // Ensure inner Axum router was built cleanly
        let _ = r;
    }

    #[test]
    fn test_router_ext() {
        let axum_r = AxumRouter::new()
            .get_route("/ping", dummy_handler)
            .version("v1", |sub| {
                sub.get_route("/users", dummy_handler)
            });

        let _ = axum_r;
    }
}

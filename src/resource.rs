//! Resource - transforms raw rows into API response shapes (port of Resource.php).
//!
//! Mirrors PHP's `Resource` class:
//! - `Resource::wrap(&item)` (Singular): transforms one item into API shape. Null becomes `{}`.
//! - `Resource::wraps(&collection)` (Plural): transforms a flat array or a paginated `{ data: [...], meta: {...} }` envelope.
//! - `when_loaded(&item, key)` / `when_loaded_map(&item, key, transform)`: conditionally includes relations only if loaded.
//! - `ItemResource<T>`: optional instance-wrapper supporting `new(item)`, `resolve()`, and `Serialize` / `jsonSerialize` parity.

use serde::Serialize;
use serde_json::{Map, Value};

/// Base trait for API resource transformers (matching PHP `Resource`).
pub trait Resource {
    /// Transform one item into target shape (PHP `toArray(mixed $request): array`).
    fn to_array(item: &Value) -> Value;

    /// Wrap a single resource instance (Singular) - PHP `Resource::wrap($resource)`.
    /// If item is null, returns an empty JSON object.
    fn wrap(item: &Value) -> Value {
        if item.is_null() {
            return Value::Object(Map::new());
        }
        Self::to_array(item)
    }

    /// Wrap a collection of resources (Plural) - PHP `Resource::wraps($resource)`.
    /// Handles both paginated results (containing a `"data"` array) and flat arrays of items.
    fn wraps(items: &Value) -> Value {
        match items {
            Value::Array(a) => Value::Array(a.iter().map(Self::to_array).collect()),
            Value::Object(o) if matches!(o.get("data"), Some(Value::Array(_))) => {
                let mut out = items.clone();
                if let Some(Value::Array(rows)) = out.get_mut("data") {
                    for r in rows.iter_mut() {
                        *r = Self::to_array(r);
                    }
                }
                out
            }
            _ => Value::Array(vec![]),
        }
    }
}

/// Conditionally include a relation or field if present and not null (PHP `whenLoaded($key)`).
pub fn when_loaded(item: &Value, key: &str) -> Option<Value> {
    item.get(key).filter(|v| !v.is_null()).cloned()
}

/// Conditionally transform a loaded relation/field (PHP `whenLoaded` with callback).
pub fn when_loaded_then<F>(item: &Value, key: &str, f: F) -> Option<Value>
where
    F: FnOnce(&Value) -> Value,
{
    item.get(key).filter(|v| !v.is_null()).map(f)
}

/// Instance wrapper matching PHP `$res = new UserResource($user); $res->resolve()`.
/// Implements `Serialize` so it can be passed directly to `ok()` or Axum responses.
#[derive(Debug, Clone)]
pub struct ItemResource<R: Resource> {
    pub resource: Value,
    _marker: std::marker::PhantomData<R>,
}

impl<R: Resource> ItemResource<R> {
    pub fn new(resource: impl Into<Value>) -> Self {
        Self {
            resource: resource.into(),
            _marker: std::marker::PhantomData,
        }
    }

    /// Resolve the resource to a transformed `Value` (PHP `resolve()`).
    pub fn resolve(&self) -> Value {
        R::wrap(&self.resource)
    }

    /// Access field dynamically from underlying resource (PHP `__get($name)`).
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.resource.get(name)
    }
}

impl<R: Resource> Serialize for ItemResource<R> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.resolve().serialize(serializer)
    }
}

/// Collection wrapper matching PHP `wraps` with `Serialize` support.
#[derive(Debug, Clone)]
pub struct CollectionResource<R: Resource> {
    pub resources: Value,
    _marker: std::marker::PhantomData<R>,
}

impl<R: Resource> CollectionResource<R> {
    pub fn new(resources: impl Into<Value>) -> Self {
        Self {
            resources: resources.into(),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn resolve(&self) -> Value {
        R::wraps(&self.resources)
    }
}

impl<R: Resource> Serialize for CollectionResource<R> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.resolve().serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct UserResource;

    impl Resource for UserResource {
        fn to_array(item: &Value) -> Value {
            let mut obj = json!({
                "id": item["id"],
                "name": item["name"],
                "email": item["email"],
            });

            if let Some(posts) = when_loaded(item, "posts") {
                obj["posts"] = posts;
            }

            obj
        }
    }

    #[test]
    fn test_wrap_single() {
        let user = json!({
            "id": 1,
            "name": "Budi",
            "email": "budi@example.com",
            "password_hash": "secret",
        });

        let out = UserResource::wrap(&user);
        assert_eq!(out["id"], 1);
        assert_eq!(out["name"], "Budi");
        assert_eq!(out["email"], "budi@example.com");
        assert!(out.get("password_hash").is_none());
        assert!(out.get("posts").is_none());
    }

    #[test]
    fn test_wrap_null() {
        let out = UserResource::wrap(&Value::Null);
        assert_eq!(out, json!({}));
    }

    #[test]
    fn test_wraps_flat_array() {
        let users = json!([
            {"id": 1, "name": "Budi", "email": "budi@example.com", "secret": "123"},
            {"id": 2, "name": "Siti", "email": "siti@example.com", "secret": "456"},
        ]);

        let out = UserResource::wraps(&users);
        assert!(out.is_array());
        assert_eq!(out.as_array().unwrap().len(), 2);
        assert_eq!(out[0]["name"], "Budi");
        assert!(out[0].get("secret").is_none());
        assert_eq!(out[1]["name"], "Siti");
    }

    #[test]
    fn test_wraps_paginated_envelope() {
        let paginated = json!({
            "data": [
                {"id": 1, "name": "Budi", "email": "budi@example.com", "pass": "x"},
            ],
            "meta": {
                "page": 1,
                "per_page": 15,
                "total": 1
            }
        });

        let out = UserResource::wraps(&paginated);
        assert_eq!(out["meta"]["page"], 1);
        assert_eq!(out["data"][0]["name"], "Budi");
        assert!(out["data"][0].get("pass").is_none());
    }

    #[test]
    fn test_when_loaded() {
        let user_with_posts = json!({
            "id": 1,
            "name": "Budi",
            "email": "budi@example.com",
            "posts": [{"title": "Post 1"}]
        });

        let out = UserResource::wrap(&user_with_posts);
        assert_eq!(out["posts"][0]["title"], "Post 1");
    }

    #[test]
    fn test_item_resource_serializer() {
        let user = json!({"id": 1, "name": "Budi", "email": "budi@example.com"});
        let item_res = ItemResource::<UserResource>::new(user);
        assert_eq!(item_res.get("name").unwrap(), "Budi");

        let serialized = serde_json::to_value(&item_res).unwrap();
        assert_eq!(serialized["id"], 1);
        assert_eq!(serialized["name"], "Budi");
    }
}

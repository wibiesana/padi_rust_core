//! Resource - transforms raw rows into API response shapes (port of Resource.php).
use serde_json::Value;

pub trait Resource {
    /// Transform one item (the equivalent of `toArray`).
    fn to_array(item: &Value) -> Value;

    /// Wrap a single item (PHP `Resource::wrap`). Null stays an empty object.
    fn wrap(item: &Value) -> Value {
        if item.is_null() {
            return Value::Object(Default::default());
        }
        Self::to_array(item)
    }

    /// Wrap a collection (PHP `Resource::wraps`): flat array or `{data: [...], meta}` envelope.
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

/// Conditionally read a field if present (PHP `whenLoaded`).
pub fn when_loaded(item: &Value, key: &str) -> Option<Value> {
    item.get(key).filter(|v| !v.is_null()).cloned()
}

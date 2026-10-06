//! Validator - input validation engine (port of Validator.php).
//!
//! Rules are pipe strings, e.g. `"required|email|unique:users,email"`.
use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Map, Value};

use crate::{
    db::{Database, Val},
    error::ApiError,
};

pub struct Validator<'a> {
    data: &'a Value,
    rules: Vec<(&'a str, &'a str)>,
    messages: BTreeMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
}

fn is_empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    }
}

fn as_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string().trim_matches('"').to_string(),
    }
}

fn as_num(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn parse_date(s: &str) -> Option<chrono::NaiveDateTime> {
    use chrono::{NaiveDate, NaiveDateTime};
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
        .ok()
        .or_else(|| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)))
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl<'a> Validator<'a> {
    pub fn new(data: &'a Value, rules: &[(&'a str, &'a str)]) -> Self {
        Self { data, rules: rules.to_vec(), messages: BTreeMap::new(), errors: BTreeMap::new() }
    }

    /// Custom messages keyed `field` or `field.rule`.
    pub fn messages(mut self, m: &[(&str, &str)]) -> Self {
        self.messages = m.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        self
    }

    /// Validate; returns only the validated fields or `ApiError::Validation` (422).
    pub async fn validate(mut self) -> Result<Value, ApiError> {
        let mut validated = Map::new();
        let rules = self.rules.clone();
        for (field, rule_str) in rules {
            let list: Vec<&str> = rule_str.split('|').filter(|r| !r.is_empty()).collect();
            if list.contains(&"sometimes") && self.data.get(field).is_none() {
                continue;
            }
            for rule in &list {
                let (name, arg) = match rule.split_once(':') {
                    Some((n, a)) => (n, Some(a)),
                    None => (*rule, None),
                };
                self.check(field, name, arg, &list).await?;
            }
            if !self.errors.contains_key(field) {
                if let Some(v) = self.data.get(field) {
                    validated.insert(field.to_string(), v.clone());
                }
            }
        }
        if self.errors.is_empty() {
            Ok(Value::Object(validated))
        } else {
            Err(ApiError::Validation(self.errors))
        }
    }

    fn err(&mut self, field: &str, rule: &str, default: String) {
        let msg = self
            .messages
            .get(&format!("{field}.{rule}"))
            .or_else(|| self.messages.get(field))
            .cloned()
            .unwrap_or(default)
            .replace("{field}", field);
        self.errors.entry(field.to_string()).or_default().push(msg);
    }

    fn is_numeric_field(&self, field: &str, v: &Value, list: &[&str]) -> bool {
        match v {
            Value::Number(_) => true,
            Value::String(s) if s.trim().parse::<f64>().is_ok() => {
                let _ = field;
                list.iter().any(|r| matches!(r.split(':').next(), Some("numeric" | "integer")))
            }
            _ => false,
        }
    }

    fn size_of(&self, field: &str, v: &Value, list: &[&str]) -> f64 {
        match v {
            Value::Array(a) => a.len() as f64,
            _ if self.is_numeric_field(field, v, list) => as_num(v).unwrap_or(0.0),
            _ => as_str(v).chars().count() as f64,
        }
    }

    async fn check(&mut self, f: &str, rule: &str, arg: Option<&str>, list: &[&str]) -> Result<(), ApiError> {
        let data = self.data;
        let val = data.get(f);
        let empty = is_empty(val);
        let v = val.cloned().unwrap_or(Value::Null);

        match rule {
            "required" => {
                let blank = empty || matches!(&v, Value::String(s) if s.trim().is_empty());
                if blank { self.err(f, rule, "The {field} field is required".into()); }
            }
            "required_if" => {
                if let Some((other, expected)) = arg.and_then(|a| a.split_once(',')) {
                    if data.get(other).map(as_str).as_deref() == Some(expected) && empty {
                        self.err(f, "required", "The {field} field is required".into());
                    }
                }
            }
            "required_with" => {
                if arg.unwrap_or("").split(',').any(|o| !is_empty(data.get(o.trim()))) && empty {
                    self.err(f, "required", "The {field} field is required".into());
                }
            }
            "required_without" => {
                if arg.unwrap_or("").split(',').any(|o| is_empty(data.get(o.trim()))) && empty {
                    self.err(f, "required", "The {field} field is required".into());
                }
            }
            _ if empty => {}
            "confirmed" => {
                if data.get(format!("{f}_confirmation")) != Some(&v) {
                    self.err(f, rule, "The {field} confirmation does not match".into());
                }
            }
            "string" => if !v.is_string() { self.err(f, rule, "The {field} must be a string".into()) },
            "numeric" => if as_num(&v).is_none() { self.err(f, rule, "The {field} must be a number".into()) },
            "integer" => {
                let ok = match &v {
                    Value::Number(n) => n.is_i64() || n.is_u64(),
                    Value::String(s) => s.trim().parse::<i64>().is_ok(),
                    _ => false,
                };
                if !ok { self.err(f, rule, "The {field} must be an integer".into()) }
            }
            "boolean" => {
                let ok = match &v {
                    Value::Bool(_) => true,
                    Value::Number(n) => matches!(n.as_i64(), Some(0 | 1)),
                    Value::String(s) => s == "0" || s == "1",
                    _ => false,
                };
                if !ok { self.err(f, rule, "The {field} must be true or false".into()) }
            }
            "array" => if !v.is_array() { self.err(f, rule, "The {field} must be an array".into()) },
            "json" => {
                let ok = v.as_str().map(|s| serde_json::from_str::<Value>(s).is_ok()).unwrap_or(false);
                if !ok { self.err(f, rule, "The {field} must be a valid JSON string".into()) }
            }
            "email" => {
                let re = Regex::new(r"^[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}$").unwrap();
                if !re.is_match(&as_str(&v)) { self.err(f, rule, "The {field} must be a valid email address".into()) }
            }
            "url" => {
                let re = Regex::new(r"^[a-zA-Z][a-zA-Z0-9+.\-]*://[^\s/?#]+[^\s]*$").unwrap();
                if !re.is_match(&as_str(&v)) { self.err(f, rule, "The {field} must be a valid URL".into()) }
            }
            "uuid" => {
                let re = Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").unwrap();
                if !re.is_match(&as_str(&v)) { self.err(f, rule, "The {field} must be a valid UUID".into()) }
            }
            "date" => if parse_date(&as_str(&v)).is_none() { self.err(f, rule, "The {field} must be a valid date".into()) },
            "date_format" => {
                // `arg` uses chrono/strftime syntax, e.g. %Y-%m-%d
                let fmt = arg.unwrap_or("%Y-%m-%d");
                let s = as_str(&v);
                let ok = chrono::NaiveDateTime::parse_from_str(&s, fmt).is_ok() || chrono::NaiveDate::parse_from_str(&s, fmt).is_ok();
                if !ok { self.err(f, rule, format!("The {{field}} does not match the format {fmt}")) }
            }
            "before" | "after" => {
                let (a, b) = (parse_date(&as_str(&v)), parse_date(arg.unwrap_or("")));
                let ok = match (a, b) {
                    (Some(a), Some(b)) => if rule == "before" { a < b } else { a > b },
                    _ => false,
                };
                if !ok { self.err(f, rule, format!("The {{field}} must be a date {rule} {}", arg.unwrap_or(""))) }
            }
            "alpha" => if !as_str(&v).chars().all(|c| c.is_ascii_alphabetic()) { self.err(f, rule, "The {field} must contain only letters".into()) },
            "alphanumeric" => if !as_str(&v).chars().all(|c| c.is_ascii_alphanumeric()) { self.err(f, rule, "The {field} must contain only letters and numbers".into()) },
            "alpha_dash" => if !as_str(&v).chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                self.err(f, rule, "The {field} must only contain letters, numbers, dashes, and underscores".into())
            },
            "regex" => {
                let ok = Regex::new(arg.unwrap_or("")).map(|r| r.is_match(&as_str(&v))).unwrap_or(false);
                if !ok { self.err(f, rule, "The {field} format is invalid".into()) }
            }
            "min" => {
                let n: f64 = arg.and_then(|a| a.parse().ok()).unwrap_or(0.0);
                if self.size_of(f, &v, list) < n {
                    let m = match &v {
                        Value::Array(_) => format!("The {{field}} must have at least {n} items"),
                        _ if self.is_numeric_field(f, &v, list) => format!("The {{field}} must be at least {n}"),
                        _ => format!("The {{field}} must be at least {n} characters"),
                    };
                    self.err(f, rule, m);
                }
            }
            "max" => {
                let n: f64 = arg.and_then(|a| a.parse().ok()).unwrap_or(0.0);
                if self.size_of(f, &v, list) > n {
                    let m = match &v {
                        Value::Array(_) => format!("The {{field}} must not have more than {n} items"),
                        _ if self.is_numeric_field(f, &v, list) => format!("The {{field}} must not exceed {n}"),
                        _ => format!("The {{field}} must not exceed {n} characters"),
                    };
                    self.err(f, rule, m);
                }
            }
            "between" => {
                let (a, b) = arg.and_then(|a| a.split_once(',')).unwrap_or(("0", "0"));
                let (a, b): (f64, f64) = (a.parse().unwrap_or(0.0), b.parse().unwrap_or(0.0));
                let s = self.size_of(f, &v, list);
                if s < a || s > b { self.err(f, rule, format!("The {{field}} must be between {a} and {b}")) }
            }
            "size" => {
                let n: f64 = arg.and_then(|a| a.parse().ok()).unwrap_or(0.0);
                if (self.size_of(f, &v, list) - n).abs() > f64::EPSILON { self.err(f, rule, format!("The {{field}} must be exactly {n}")) }
            }
            "in" => {
                let a = arg.unwrap_or("");
                if !a.split(',').any(|x| x == as_str(&v)) { self.err(f, rule, format!("The {{field}} must be one of: {a}")) }
            }
            "not_in" => {
                if arg.unwrap_or("").split(',').any(|x| x == as_str(&v)) { self.err(f, rule, "The selected {field} is invalid".into()) }
            }
            "exists" => {
                let p: Vec<&str> = arg.unwrap_or("").split(',').collect();
                let (table, col) = (p.first().copied().unwrap_or(""), p.get(1).copied().unwrap_or("id"));
                if !self.count_where(table, col, &v, None, "id").await? { self.err(f, rule, "The selected {field} does not exist".into()) }
            }
            "unique" => {
                // unique:table,column,ignoreId,idColumn
                let p: Vec<&str> = arg.unwrap_or("").split(',').collect();
                let (table, col) = (p.first().copied().unwrap_or(""), p.get(1).copied().unwrap_or(f));
                let ignore = p.get(2).copied().filter(|s| !s.is_empty());
                let idc = p.get(3).copied().unwrap_or("id");
                if self.count_where(table, col, &v, ignore, idc).await? { self.err(f, rule, "The {field} has already been taken".into()) }
            }
            _ => {} // sometimes, nullable, unknown rules are ignored
        }
        Ok(())
    }

    async fn count_where(&self, table: &str, col: &str, v: &Value, ignore: Option<&str>, idc: &str) -> Result<bool, ApiError> {
        if !ident_ok(table) || !ident_ok(col) || !ident_ok(idc) {
            return Err(ApiError::internal(format!("Invalid identifier in validation rule: {table}.{col}")));
        }
        let mut sql = format!("SELECT COUNT(*) AS c FROM {table} WHERE {col} = ?");
        let mut params = vec![Val::from(v)];
        if let Some(id) = ignore {
            sql.push_str(&format!(" AND {idc} != ?"));
            params.push(Val::from(id));
        }
        let row = Database::fetch_one(&sql, &params).await?;
        Ok(row.and_then(|r| r["c"].as_i64()).unwrap_or(0) > 0)
    }
}

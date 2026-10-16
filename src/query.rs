//! Query - Fluent, injection-safe SQL query builder for Padi Rust Api Framework.
//!
//! Security:
//! - All values are bound via parameterized queries (`?`).
//! - Column and table identifiers are strictly validated.
//! - Pessimistic row locking (`FOR UPDATE`, `FOR SHARE`) supported.

use serde_json::{json, Value};

use crate::{
    db::{Database, Val},
    error::ApiError,
};

/// Query builder version.
pub const VERSION: &str = "0.1.0";

/// Identifiers may contain letters, digits, `_`, `.`, `*` and a single ` as ` alias.
fn safe_ident(s: &str) -> String {
    let ok = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '*'));
    let lower = s.to_lowercase();
    if let Some(pos) = lower.find(" as ") {
        let (a, b) = (s[..pos].trim(), s[pos + 4..].trim());
        if ok(a) && ok(b) {
            return format!("{a} AS {b}");
        }
    } else if ok(s.trim()) {
        return s.trim().to_string();
    }
    panic!("Invalid SQL identifier: {s}");
}

fn safe_op(op: &str) -> String {
    let o = op.trim().to_uppercase();
    match o.as_str() {
        "=" | "!=" | "<>" | "<" | ">" | "<=" | ">=" | "LIKE" | "NOT LIKE" | "ILIKE" | "NOT ILIKE" => o,
        _ => panic!("Invalid SQL operator: {op}"),
    }
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    table: String,
    columns: Vec<String>,
    raw_select: Option<String>,
    joins: Vec<String>,
    wheres: Vec<(&'static str, String)>,
    params: Vec<Val>,
    group: Vec<String>,
    having: Option<(String, Vec<Val>)>,
    order: Vec<String>,
    limit: Option<u64>,
    offset: Option<u64>,
    distinct: bool,
    lock: Option<String>,
}

impl Query {
    /// Create a new query for the given table.
    pub fn table(table: &str) -> Self {
        Self { table: safe_ident(table), ..Default::default() }
    }

    /// Alias for `Query::table` (matching `Query.php`).
    pub fn from(mut self, table: &str) -> Self {
        self.table = safe_ident(table);
        self
    }

    /// Select specific columns.
    pub fn select(mut self, cols: &[&str]) -> Self {
        self.columns = cols.iter().map(|c| safe_ident(c)).collect();
        self
    }

    /// Add additional columns to existing select.
    pub fn add_select(mut self, cols: &[&str]) -> Self {
        self.columns.extend(cols.iter().map(|c| safe_ident(c)));
        self
    }

    /// Raw select expression (e.g. `COUNT(DISTINCT id) AS total`).
    pub fn select_raw(mut self, raw: &str) -> Self {
        self.raw_select = Some(raw.to_string());
        self
    }

    /// Apply DISTINCT clause.
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    fn push_where(mut self, conn: &'static str, sql: String, p: Vec<Val>) -> Self {
        self.wheres.push((conn, sql));
        self.params.extend(p);
        self
    }

    pub fn where_op(self, col: &str, op: &str, v: impl Into<Val>) -> Self {
        let sql = format!("{} {} ?", safe_ident(col), safe_op(op));
        self.push_where("AND", sql, vec![v.into()])
    }

    pub fn where_eq(self, col: &str, v: impl Into<Val>) -> Self {
        self.where_op(col, "=", v)
    }

    pub fn or_where(self, col: &str, op: &str, v: impl Into<Val>) -> Self {
        let sql = format!("{} {} ?", safe_ident(col), safe_op(op));
        self.push_where("OR", sql, vec![v.into()])
    }

    pub fn or_where_eq(self, col: &str, v: impl Into<Val>) -> Self {
        self.or_where(col, "=", v)
    }

    pub fn where_like(self, col: &str, pattern: &str) -> Self {
        self.where_op(col, "LIKE", pattern)
    }

    pub fn where_not_like(self, col: &str, pattern: &str) -> Self {
        self.where_op(col, "NOT LIKE", pattern)
    }

    pub fn or_where_like(self, col: &str, pattern: &str) -> Self {
        self.or_where(col, "LIKE", pattern)
    }

    pub fn or_where_not_like(self, col: &str, pattern: &str) -> Self {
        self.or_where(col, "NOT LIKE", pattern)
    }

    pub fn where_in<V: Into<Val>>(self, col: &str, vals: Vec<V>) -> Self {
        if vals.is_empty() {
            return self.push_where("AND", "1 = 0".into(), vec![]);
        }
        let ph = vec!["?"; vals.len()].join(", ");
        self.push_where("AND", format!("{} IN ({ph})", safe_ident(col)), vals.into_iter().map(Into::into).collect())
    }

    pub fn or_where_in<V: Into<Val>>(self, col: &str, vals: Vec<V>) -> Self {
        if vals.is_empty() {
            return self;
        }
        let ph = vec!["?"; vals.len()].join(", ");
        self.push_where("OR", format!("{} IN ({ph})", safe_ident(col)), vals.into_iter().map(Into::into).collect())
    }

    pub fn where_not_in<V: Into<Val>>(self, col: &str, vals: Vec<V>) -> Self {
        if vals.is_empty() {
            return self;
        }
        let ph = vec!["?"; vals.len()].join(", ");
        self.push_where("AND", format!("{} NOT IN ({ph})", safe_ident(col)), vals.into_iter().map(Into::into).collect())
    }

    pub fn or_where_not_in<V: Into<Val>>(self, col: &str, vals: Vec<V>) -> Self {
        if vals.is_empty() {
            return self;
        }
        let ph = vec!["?"; vals.len()].join(", ");
        self.push_where("OR", format!("{} NOT IN ({ph})", safe_ident(col)), vals.into_iter().map(Into::into).collect())
    }

    pub fn where_between(self, col: &str, a: impl Into<Val>, b: impl Into<Val>) -> Self {
        self.push_where("AND", format!("{} BETWEEN ? AND ?", safe_ident(col)), vec![a.into(), b.into()])
    }

    pub fn where_not_between(self, col: &str, a: impl Into<Val>, b: impl Into<Val>) -> Self {
        self.push_where("AND", format!("{} NOT BETWEEN ? AND ?", safe_ident(col)), vec![a.into(), b.into()])
    }

    pub fn or_where_between(self, col: &str, a: impl Into<Val>, b: impl Into<Val>) -> Self {
        self.push_where("OR", format!("{} BETWEEN ? AND ?", safe_ident(col)), vec![a.into(), b.into()])
    }

    pub fn or_where_not_between(self, col: &str, a: impl Into<Val>, b: impl Into<Val>) -> Self {
        self.push_where("OR", format!("{} NOT BETWEEN ? AND ?", safe_ident(col)), vec![a.into(), b.into()])
    }

    pub fn where_null(self, col: &str) -> Self {
        self.push_where("AND", format!("{} IS NULL", safe_ident(col)), vec![])
    }

    pub fn or_where_null(self, col: &str) -> Self {
        self.push_where("OR", format!("{} IS NULL", safe_ident(col)), vec![])
    }

    pub fn where_not_null(self, col: &str) -> Self {
        self.push_where("AND", format!("{} IS NOT NULL", safe_ident(col)), vec![])
    }

    pub fn or_where_not_null(self, col: &str) -> Self {
        self.push_where("OR", format!("{} IS NOT NULL", safe_ident(col)), vec![])
    }

    /// Raw condition with `?` placeholders.
    pub fn where_raw(self, sql: &str, params: Vec<Val>) -> Self {
        self.push_where("AND", sql.to_string(), params)
    }

    pub fn or_where_raw(self, sql: &str, params: Vec<Val>) -> Self {
        self.push_where("OR", sql.to_string(), params)
    }

    fn join_with(mut self, kind: &str, table: &str, a: &str, op: &str, b: &str) -> Self {
        self.joins.push(format!(
            "{kind} JOIN {} ON {} {} {}",
            safe_ident(table), safe_ident(a), safe_op(op), safe_ident(b)
        ));
        self
    }

    pub fn join(self, table: &str, a: &str, op: &str, b: &str) -> Self { self.join_with("INNER", table, a, op, b) }
    pub fn inner_join(self, table: &str, a: &str, op: &str, b: &str) -> Self { self.join_with("INNER", table, a, op, b) }
    pub fn left_join(self, table: &str, a: &str, op: &str, b: &str) -> Self { self.join_with("LEFT", table, a, op, b) }
    pub fn right_join(self, table: &str, a: &str, op: &str, b: &str) -> Self { self.join_with("RIGHT", table, a, op, b) }

    pub fn order_by(mut self, col: &str, dir: &str) -> Self {
        let d = if dir.eq_ignore_ascii_case("desc") { "DESC" } else { "ASC" };
        self.order.push(format!("{} {d}", safe_ident(col)));
        self
    }

    pub fn group_by(mut self, col: &str) -> Self {
        self.group.push(safe_ident(col));
        self
    }

    pub fn having(mut self, sql: &str, params: Vec<Val>) -> Self {
        self.having = Some((sql.to_string(), params));
        self
    }

    pub fn limit(mut self, n: u64) -> Self { self.limit = Some(n); self }
    pub fn offset(mut self, n: u64) -> Self { self.offset = Some(n); self }

    /// Pessimistic row locking: `FOR UPDATE`.
    pub fn for_update(mut self) -> Self {
        self.lock = Some("FOR UPDATE".to_string());
        self
    }

    /// Pessimistic row locking: `FOR SHARE`.
    pub fn for_share(mut self) -> Self {
        self.lock = Some("FOR SHARE".to_string());
        self
    }

    /// Set custom row locking clause.
    pub fn lock(mut self, lock_clause: &str) -> Self {
        self.lock = Some(lock_clause.to_string());
        self
    }

    fn where_sql(&self) -> String {
        let mut s = String::new();
        for (i, (conn, w)) in self.wheres.iter().enumerate() {
            if i == 0 {
                s.push_str(" WHERE ");
            } else {
                s.push_str(&format!(" {conn} "));
            }
            s.push_str(&format!("({w})"));
        }
        s
    }

    /// Build the SELECT statement and ordered bind values.
    pub fn to_sql(&self) -> (String, Vec<Val>) {
        let cols = if let Some(r) = &self.raw_select {
            r.clone()
        } else if self.columns.is_empty() {
            "*".into()
        } else {
            self.columns.join(", ")
        };
        let mut sql = format!(
            "SELECT {}{} FROM {}",
            if self.distinct { "DISTINCT " } else { "" },
            cols,
            self.table
        );
        for j in &self.joins {
            sql.push(' ');
            sql.push_str(j);
        }
        sql.push_str(&self.where_sql());
        let mut params = self.params.clone();
        if !self.group.is_empty() {
            sql.push_str(&format!(" GROUP BY {}", self.group.join(", ")));
        }
        if let Some((h, p)) = &self.having {
            sql.push_str(&format!(" HAVING {h}"));
            params.extend(p.clone());
        }
        if !self.order.is_empty() {
            sql.push_str(&format!(" ORDER BY {}", self.order.join(", ")));
        }
        if let Some(l) = self.limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        if let Some(o) = self.offset {
            sql.push_str(&format!(" OFFSET {o}"));
        }
        if let Some(ref l) = self.lock {
            sql.push(' ');
            sql.push_str(l);
        }
        (sql, params)
    }

    /// Get raw SQL with interpolated values (for debugging only).
    pub fn raw_sql(&self) -> String {
        let (sql, params) = self.to_sql();
        let mut result = String::new();
        let mut param_idx = 0;

        for ch in sql.chars() {
            if ch == '?' && param_idx < params.len() {
                match &params[param_idx] {
                    Val::Null => result.push_str("NULL"),
                    Val::Bool(b) => result.push_str(if *b { "1" } else { "0" }),
                    Val::Int(i) => result.push_str(&i.to_string()),
                    Val::Float(f) => result.push_str(&f.to_string()),
                    Val::Str(s) => {
                        let escaped = s.replace('\'', "''");
                        result.push('\'');
                        result.push_str(&escaped);
                        result.push('\'');
                    }
                }
                param_idx += 1;
            } else {
                result.push(ch);
            }
        }
        result
    }

    // ── execution ────────────────────────────────────────────────────────

    pub async fn get(&self) -> Result<Vec<Value>, ApiError> {
        let (sql, p) = self.to_sql();
        Database::fetch_all(&sql, &p).await
    }

    pub async fn first(&self) -> Result<Option<Value>, ApiError> {
        let mut q = self.clone();
        q.limit = Some(1);
        let (sql, p) = q.to_sql();
        Database::fetch_one(&sql, &p).await
    }

    /// Execute query and return a single scalar value from the first row.
    pub async fn scalar(&self) -> Result<Option<Value>, ApiError> {
        let row = self.first().await?;
        Ok(row.and_then(|r| r.as_object().and_then(|obj| obj.values().next().cloned())))
    }

    /// Execute query and return the value of a specific column from the first row.
    pub async fn value(&self, col: &str) -> Result<Option<Value>, ApiError> {
        let row = self.first().await?;
        Ok(row.and_then(|r| r.get(col).cloned()))
    }

    /// Execute query and return all values of a single column as a list (pluck/column).
    pub async fn column(&self, col: &str) -> Result<Vec<Value>, ApiError> {
        let mut q = self.clone();
        q.columns = vec![safe_ident(col)];
        let rows = q.get().await?;
        Ok(rows.into_iter().filter_map(|r| r.get(col).cloned()).collect())
    }

    /// Alias for `column` (Laravel/Padi parity).
    pub async fn pluck(&self, col: &str) -> Result<Vec<Value>, ApiError> {
        self.column(col).await
    }

    pub async fn count(&self) -> Result<i64, ApiError> {
        self.count_col("*").await
    }

    pub async fn count_col(&self, col: &str) -> Result<i64, ApiError> {
        let safe = if col == "*" { "*".to_string() } else { safe_ident(col) };
        let sql = format!("SELECT COUNT({}) AS aggregate FROM {}{}{}", safe, self.table, self.joins_sql(), self.where_sql());
        let row = Database::fetch_one(&sql, &self.params).await?;
        Ok(row.and_then(|r| r["aggregate"].as_i64()).unwrap_or(0))
    }

    /// Calculate SUM of a column.
    pub async fn sum(&self, col: &str) -> Result<Option<f64>, ApiError> {
        let sql = format!("SELECT SUM({}) AS aggregate FROM {}{}{}", safe_ident(col), self.table, self.joins_sql(), self.where_sql());
        let row = Database::fetch_one(&sql, &self.params).await?;
        Ok(row.and_then(|r| r["aggregate"].as_f64()))
    }

    /// Calculate AVG of a column.
    pub async fn avg(&self, col: &str) -> Result<Option<f64>, ApiError> {
        let sql = format!("SELECT AVG({}) AS aggregate FROM {}{}{}", safe_ident(col), self.table, self.joins_sql(), self.where_sql());
        let row = Database::fetch_one(&sql, &self.params).await?;
        Ok(row.and_then(|r| r["aggregate"].as_f64()))
    }

    pub async fn average(&self, col: &str) -> Result<Option<f64>, ApiError> {
        self.avg(col).await
    }

    /// Find minimum value of a column.
    pub async fn min(&self, col: &str) -> Result<Option<Value>, ApiError> {
        let sql = format!("SELECT MIN({}) AS aggregate FROM {}{}{}", safe_ident(col), self.table, self.joins_sql(), self.where_sql());
        let row = Database::fetch_one(&sql, &self.params).await?;
        Ok(row.and_then(|r| r.get("aggregate").cloned()))
    }

    /// Find maximum value of a column.
    pub async fn max(&self, col: &str) -> Result<Option<Value>, ApiError> {
        let sql = format!("SELECT MAX({}) AS aggregate FROM {}{}{}", safe_ident(col), self.table, self.joins_sql(), self.where_sql());
        let row = Database::fetch_one(&sql, &self.params).await?;
        Ok(row.and_then(|r| r.get("aggregate").cloned()))
    }

    fn joins_sql(&self) -> String {
        self.joins.iter().map(|j| format!(" {j}")).collect()
    }

    pub async fn exists(&self) -> Result<bool, ApiError> {
        Ok(self.count().await? > 0)
    }

    /// Returns `{ data: [...], meta: { current_page, per_page, total, last_page, from, to } }`.
    pub async fn paginate(&self, page: u64, per_page: u64) -> Result<Value, ApiError> {
        let page = page.max(1);
        let per_page = per_page.clamp(1, 100);
        let total = self.count().await? as u64;
        let offset = (page - 1) * per_page;
        let data = self.clone().limit(per_page).offset(offset).get().await?;

        let from = if total > 0 { offset + 1 } else { 0 };
        let to = (offset + data.len() as u64).min(total);

        Ok(json!({
            "data": data,
            "meta": {
                "current_page": page,
                "per_page": per_page,
                "total": total,
                "last_page": total.div_ceil(per_page).max(1),
                "from": from,
                "to": to,
            }
        }))
    }

    /// Insert a JSON object; returns last insert id when available.
    pub async fn insert(&self, data: &Value) -> Result<Option<i64>, ApiError> {
        let obj = data.as_object().ok_or_else(|| ApiError::bad_request("insert data must be an object"))?;
        let cols: Vec<String> = obj.keys().map(|k| safe_ident(k)).collect();
        let ph = vec!["?"; cols.len()].join(", ");
        let sql = format!("INSERT INTO {} ({}) VALUES ({ph})", self.table, cols.join(", "));
        let params: Vec<Val> = obj.values().map(Val::from).collect();
        Ok(Database::execute(&sql, &params).await?.1)
    }

    /// Insert and return the new primary key (RETURNING on SQLite/Postgres, last_insert_id on MySQL).
    pub async fn insert_get_id(&self, data: &Value, pk: &str) -> Result<Option<i64>, ApiError> {
        if !(Database::is_sqlite() || Database::is_postgres()) {
            return self.insert(data).await;
        }
        let obj = data.as_object().ok_or_else(|| ApiError::bad_request("insert data must be an object"))?;
        let cols: Vec<String> = obj.keys().map(|k| safe_ident(k)).collect();
        let ph = vec!["?"; cols.len()].join(", ");
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({ph}) RETURNING {}",
            self.table, cols.join(", "), safe_ident(pk)
        );
        let params: Vec<Val> = obj.values().map(Val::from).collect();
        let row = Database::fetch_one(&sql, &params).await?;
        Ok(row.and_then(|r| r[pk].as_i64()))
    }

    pub async fn update(&self, data: &Value) -> Result<u64, ApiError> {
        let obj = data.as_object().ok_or_else(|| ApiError::bad_request("update data must be an object"))?;
        if obj.is_empty() {
            return Ok(0);
        }
        let sets: Vec<String> = obj.keys().map(|k| format!("{} = ?", safe_ident(k))).collect();
        let mut params: Vec<Val> = obj.values().map(Val::from).collect();
        params.extend(self.params.clone());
        let sql = format!("UPDATE {} SET {}{}", self.table, sets.join(", "), self.where_sql());
        Ok(Database::execute(&sql, &params).await?.0)
    }

    /// Increment a numeric column atomically.
    pub async fn increment(&self, col: &str, amount: i64) -> Result<u64, ApiError> {
        let sql = format!("UPDATE {} SET {} = {} + ?{}", self.table, safe_ident(col), safe_ident(col), self.where_sql());
        let mut params = vec![Val::Int(amount)];
        params.extend(self.params.clone());
        Ok(Database::execute(&sql, &params).await?.0)
    }

    /// Decrement a numeric column atomically.
    pub async fn decrement(&self, col: &str, amount: i64) -> Result<u64, ApiError> {
        self.increment(col, -amount).await
    }

    pub async fn delete(&self) -> Result<u64, ApiError> {
        let sql = format!("DELETE FROM {}{}", self.table, self.where_sql());
        Ok(Database::execute(&sql, &self.params).await?.0)
    }

    /// Truncate table.
    pub async fn truncate(&self) -> Result<(), ApiError> {
        let sql = format!("DELETE FROM {}", self.table);
        Database::execute(&sql, &[]).await?;
        Ok(())
    }

    /// Process results in chunks of `size` with low memory usage.
    pub async fn chunk<F, Fut>(&self, size: u64, mut callback: F) -> Result<(), ApiError>
    where
        F: FnMut(Vec<Value>) -> Fut,
        Fut: std::future::Future<Output = Result<bool, ApiError>>,
    {
        let size = size.max(1);
        let mut page = 1;
        loop {
            let rows = self.clone().limit(size).offset((page - 1) * size).get().await?;
            if rows.is_empty() {
                break;
            }
            let count = rows.len() as u64;
            let should_continue = callback(rows).await?;
            if !should_continue || count < size {
                break;
            }
            page += 1;
        }
        Ok(())
    }

    /// Iterate over query results one by one in chunks.
    pub async fn each<F, Fut>(&self, size: u64, mut callback: F) -> Result<(), ApiError>
    where
        F: FnMut(Value) -> Fut,
        Fut: std::future::Future<Output = Result<bool, ApiError>>,
    {
        let size = size.max(1);
        let mut page = 1;
        loop {
            let rows = self.clone().limit(size).offset((page - 1) * size).get().await?;
            if rows.is_empty() {
                break;
            }
            let count = rows.len() as u64;
            for row in rows {
                if !callback(row).await? {
                    return Ok(());
                }
            }
            if count < size {
                break;
            }
            page += 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_sql_and_raw_sql() {
        let q = Query::table("users")
            .select(&["id", "name", "email"])
            .where_eq("status", "active")
            .where_between("age", 18, 65)
            .where_in("role_id", vec![1, 2, 3])
            .order_by("created_at", "DESC")
            .limit(10)
            .offset(20);

        let (sql, params) = q.to_sql();
        assert_eq!(
            sql,
            "SELECT id, name, email FROM users WHERE (status = ?) AND (age BETWEEN ? AND ?) AND (role_id IN (?, ?, ?)) ORDER BY created_at DESC LIMIT 10 OFFSET 20"
        );
        assert_eq!(params.len(), 6);

        let raw = q.raw_sql();
        assert_eq!(
            raw,
            "SELECT id, name, email FROM users WHERE (status = 'active') AND (age BETWEEN 18 AND 65) AND (role_id IN (1, 2, 3)) ORDER BY created_at DESC LIMIT 10 OFFSET 20"
        );
    }

    #[test]
    fn test_lock_clauses() {
        let q = Query::table("accounts").where_eq("id", 1).for_update();
        let (sql, _) = q.to_sql();
        assert!(sql.ends_with("FOR UPDATE"));

        let q_share = Query::table("accounts").where_eq("id", 1).for_share();
        let (sql_share, _) = q_share.to_sql();
        assert!(sql_share.ends_with("FOR SHARE"));
    }

    #[test]
    fn test_where_like_and_null() {
        let q = Query::table("products")
            .where_like("name", "%laptop%")
            .where_not_null("price");
        let (sql, _) = q.to_sql();
        assert!(sql.contains("name LIKE ?"));
        assert!(sql.contains("price IS NOT NULL"));
    }
}

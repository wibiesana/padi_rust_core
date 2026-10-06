//! Query - fluent, injection-safe query builder (port of Query.php core API).
use serde_json::{json, Value};

use crate::{
    db::{Database, Val},
    error::ApiError,
};

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
        "=" | "!=" | "<>" | "<" | ">" | "<=" | ">=" | "LIKE" | "NOT LIKE" => o,
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
}

impl Query {
    pub fn table(table: &str) -> Self {
        Self { table: safe_ident(table), ..Default::default() }
    }

    pub fn select(mut self, cols: &[&str]) -> Self {
        self.columns = cols.iter().map(|c| safe_ident(c)).collect();
        self
    }
    pub fn select_raw(mut self, raw: &str) -> Self {
        self.raw_select = Some(raw.to_string());
        self
    }
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
    pub fn where_in<V: Into<Val>>(self, col: &str, vals: Vec<V>) -> Self {
        if vals.is_empty() {
            return self.push_where("AND", "1 = 0".into(), vec![]);
        }
        let ph = vec!["?"; vals.len()].join(", ");
        self.push_where("AND", format!("{} IN ({ph})", safe_ident(col)), vals.into_iter().map(Into::into).collect())
    }
    pub fn where_not_in<V: Into<Val>>(self, col: &str, vals: Vec<V>) -> Self {
        if vals.is_empty() {
            return self;
        }
        let ph = vec!["?"; vals.len()].join(", ");
        self.push_where("AND", format!("{} NOT IN ({ph})", safe_ident(col)), vals.into_iter().map(Into::into).collect())
    }
    pub fn where_between(self, col: &str, a: impl Into<Val>, b: impl Into<Val>) -> Self {
        self.push_where("AND", format!("{} BETWEEN ? AND ?", safe_ident(col)), vec![a.into(), b.into()])
    }
    pub fn where_null(self, col: &str) -> Self {
        self.push_where("AND", format!("{} IS NULL", safe_ident(col)), vec![])
    }
    pub fn where_not_null(self, col: &str) -> Self {
        self.push_where("AND", format!("{} IS NOT NULL", safe_ident(col)), vec![])
    }
    /// Raw condition with `?` placeholders (you are responsible for the SQL text).
    pub fn where_raw(self, sql: &str, params: Vec<Val>) -> Self {
        self.push_where("AND", sql.to_string(), params)
    }

    fn join_with(mut self, kind: &str, table: &str, a: &str, op: &str, b: &str) -> Self {
        self.joins.push(format!(
            "{kind} JOIN {} ON {} {} {}",
            safe_ident(table), safe_ident(a), safe_op(op), safe_ident(b)
        ));
        self
    }
    pub fn join(self, table: &str, a: &str, op: &str, b: &str) -> Self { self.join_with("INNER", table, a, op, b) }
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

    fn where_sql(&self) -> String {
        let mut s = String::new();
        for (i, (conn, w)) in self.wheres.iter().enumerate() {
            if i == 0 {
                s.push_str(" WHERE ");
            } else {
                s.push_str(&format!(" {conn} "));
            }
            // Wrap to keep AND/OR precedence predictable.
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
        (sql, params)
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

    pub async fn count(&self) -> Result<i64, ApiError> {
        let sql = format!("SELECT COUNT(*) AS aggregate FROM {}{}{}", self.table, self.joins_sql(), self.where_sql());
        let row = Database::fetch_one(&sql, &self.params).await?;
        Ok(row.and_then(|r| r["aggregate"].as_i64()).unwrap_or(0))
    }

    fn joins_sql(&self) -> String {
        self.joins.iter().map(|j| format!(" {j}")).collect()
    }

    pub async fn exists(&self) -> Result<bool, ApiError> {
        Ok(self.count().await? > 0)
    }

    /// Returns `{ data: [...], meta: { current_page, per_page, total, last_page } }`.
    pub async fn paginate(&self, page: u64, per_page: u64) -> Result<Value, ApiError> {
        let page = page.max(1);
        let per_page = per_page.clamp(1, 100);
        let total = self.count().await? as u64;
        let data = self.clone().limit(per_page).offset((page - 1) * per_page).get().await?;
        Ok(json!({
            "data": data,
            "meta": {
                "current_page": page,
                "per_page": per_page,
                "total": total,
                "last_page": total.div_ceil(per_page).max(1),
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

    pub async fn delete(&self) -> Result<u64, ApiError> {
        let sql = format!("DELETE FROM {}{}", self.table, self.where_sql());
        Ok(Database::execute(&sql, &self.params).await?.0)
    }
}

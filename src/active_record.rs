//! ActiveRecord / Model - full port of `ActiveRecord.php`.
//!
//! PHP -> Rust mapping (all instance/static methods are covered):
//!
//! | PHP                                   | Rust                                           |
//! |---------------------------------------|------------------------------------------------|
//! | `$table/$primaryKey/$fillable/...`    | const builder: `Model::new("t").fillable(..)`  |
//! | `getTable / getPrimaryKeyName`        | `table_name / primary_key_name`                |
//! | `clearColumnsCache / getTableColumns` | `clear_columns_cache / get_table_columns`      |
//! | `with / clearWith / getWith`          | `ModelQuery::with` / (immutable) / `get_with`  |
//! | `search`                              | `search` (async -> `ModelQuery`)               |
//! | `find / findBuilder / findQuery`      | `find_query`                                   |
//! | `get / all`                           | `all_with` / `all`                             |
//! | `loadRelations / getRelationConfig`   | `load_relations` / `get_relation_config`       |
//! | `hasMany/hasOne/belongsTo/...`        | `Relation::has_many/has_one/belongs_to/...`    |
//! | `findByPk / findOne / findAll`        | `find_by_pk / find_one / find_all`             |
//! | `findOrFail / getPkConditions`        | `find_or_fail / get_pk_conditions`             |
//! | `where / filterWhere`                 | `where_conditions / filter_where`              |
//! | `create+insertRecord`                 | `create` (row) / `insert_record` (id)          |
//! | `update+updateRecord`                 | `update` (row) / `update_record` (bool)        |
//! | `batchInsert / updateAll`             | `batch_insert / update_all`                    |
//! | `delete+deleteRecord / deleteAll`     | `delete` / `delete_record` / `delete_all`      |
//! | `paginate`                            | `paginate` / `paginate_where`                  |
//! | `query` (raw SQL)                     | `raw`                                          |
//! | `count / upsert`                      | `count / upsert`                               |
//! | `hideFields / filterFillable`         | `hide_fields` (+`present`) / `filter_fillable` |
//! | `beforeSave/afterLoad/afterSave/...`  | hook fn pointers (`before_save`, ...)          |
//! | audit fields (`Auth::userId()`)       | `with_user(id, fut)` task scope                |
//!
//! Not ported: `$connection` / `getDb()` (the Rust core uses one shared pool).
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{OnceLock, RwLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{json, Map, Value};

use crate::{
    cache::Cache,
    db::{Database, Val},
    env,
    error::ApiError,
    query::Query,
};

// ───────────────────────── helpers ─────────────────────────

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn check_ident(s: &str, what: &str) -> Result<(), ApiError> {
    if ident_ok(s) {
        Ok(())
    } else {
        Err(ApiError::bad_request(format!("Invalid {what}: {s}")))
    }
}

fn check_cols(cols: &[&str]) -> Result<(), ApiError> {
    for c in cols {
        if *c != "*" {
            check_ident(c, "column name")?;
        }
    }
    Ok(())
}

fn key_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn is_empty_value(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// Apply a hash-format condition object (`{col: value}`) to a query.
/// `null` => IS NULL, array => IN (...), scalar => `=`.
fn apply_conditions(mut q: Query, cond: &Value) -> Result<Query, ApiError> {
    if let Some(obj) = cond.as_object() {
        for (k, v) in obj {
            check_ident(k, "condition key")?;
            q = match v {
                Value::Null => q.where_null(k),
                Value::Array(a) => q.where_in(k, a.iter().map(Val::from).collect::<Vec<Val>>()),
                other => q.where_eq(k, Val::from(other)),
            };
        }
    }
    Ok(q)
}

/// Validate an ORDER BY clause (`col`, `t.col`, optional ASC/DESC, comma separated).
fn sanitize_order_by(order_by: &str) -> Result<Vec<(String, String)>, ApiError> {
    let mut out = Vec::new();
    for seg in order_by.split(',') {
        let seg = seg.trim();
        let mut it = seg.split_whitespace();
        let err = || {
            ApiError::bad_request(format!(
                "Invalid ORDER BY segment: '{seg}'. Only column names and ASC/DESC direction are allowed."
            ))
        };
        let col = it.next().ok_or_else(err)?;
        let dir = it.next();
        if it.next().is_some()
            || !ident_ok(col)
            || col.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        {
            return Err(err());
        }
        let dir = match dir {
            None => "ASC".to_string(),
            Some(d) if d.eq_ignore_ascii_case("asc") => "ASC".into(),
            Some(d) if d.eq_ignore_ascii_case("desc") => "DESC".into(),
            _ => return Err(err()),
        };
        out.push((col.to_string(), dir));
    }
    Ok(out)
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ───────────────────────── audit user scope ─────────────────────────

tokio::task_local! {
    static CURRENT_USER: Option<i64>;
}

/// Run `fut` with an authenticated user id so `created_by` / `updated_by`
/// are filled automatically (equivalent of `Auth::userId()` in PHP).
pub async fn with_user<F: Future>(user_id: Option<i64>, fut: F) -> F::Output {
    CURRENT_USER.scope(user_id, fut).await
}

/// User id of the current `with_user` scope.
pub fn current_user_id() -> Option<i64> {
    CURRENT_USER.try_with(|u| *u).ok().flatten()
}

// ───────────────────────── columns cache ─────────────────────────

type ColumnsCache = RwLock<HashMap<String, (Vec<String>, u64)>>;

fn columns_cache() -> &'static ColumnsCache {
    static C: OnceLock<ColumnsCache> = OnceLock::new();
    C.get_or_init(|| RwLock::new(HashMap::new()))
}

// ───────────────────────── Pk ─────────────────────────

/// Primary key value: scalar, `{col: val}` object or `"a_b"` string for composite keys.
#[derive(Debug, Clone)]
pub struct Pk(pub Value);

impl From<i64> for Pk { fn from(v: i64) -> Self { Pk(json!(v)) } }
impl From<i32> for Pk { fn from(v: i32) -> Self { Pk(json!(v)) } }
impl From<u32> for Pk { fn from(v: u32) -> Self { Pk(json!(v)) } }
impl From<u64> for Pk { fn from(v: u64) -> Self { Pk(json!(v)) } }
impl From<&str> for Pk { fn from(v: &str) -> Self { Pk(json!(v)) } }
impl From<String> for Pk { fn from(v: String) -> Self { Pk(Value::String(v)) } }
impl From<Value> for Pk { fn from(v: Value) -> Self { Pk(v) } }
impl From<&Value> for Pk { fn from(v: &Value) -> Self { Pk(v.clone()) } }

// ───────────────────────── relations ─────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationType {
    HasOne,
    HasMany,
    BelongsTo,
    BelongsToMany,
}

/// Relation definition. `model` is a fn pointer (`|| POSTS`) so models may
/// reference each other without const cycles.
#[derive(Debug, Clone)]
pub struct Relation {
    pub name: &'static str,
    pub rel_type: RelationType,
    pub model: fn() -> Model,
    /// Key in the related table (belongsTo: owner key; belongsToMany: this model's key in pivot).
    pub foreign_key: &'static str,
    /// Key in this table.
    pub local_key: &'static str,
    pub pivot_table: Option<&'static str>,
    pub related_key: Option<&'static str>,
    pub columns: Option<&'static [&'static str]>,
}

impl Relation {
    const fn base(
        name: &'static str,
        rel_type: RelationType,
        model: fn() -> Model,
        foreign_key: &'static str,
        local_key: &'static str,
    ) -> Self {
        Self { name, rel_type, model, foreign_key, local_key, pivot_table: None, related_key: None, columns: None }
    }

    pub const fn has_many(name: &'static str, model: fn() -> Model, foreign_key: &'static str, local_key: &'static str) -> Self {
        Self::base(name, RelationType::HasMany, model, foreign_key, local_key)
    }

    pub const fn has_one(name: &'static str, model: fn() -> Model, foreign_key: &'static str, local_key: &'static str) -> Self {
        Self::base(name, RelationType::HasOne, model, foreign_key, local_key)
    }

    /// `foreign_key` lives in this table, `owner_key` in the related table.
    pub const fn belongs_to(name: &'static str, model: fn() -> Model, foreign_key: &'static str, owner_key: &'static str) -> Self {
        Self::base(name, RelationType::BelongsTo, model, owner_key, foreign_key)
    }

    pub const fn belongs_to_many(
        name: &'static str,
        model: fn() -> Model,
        pivot_table: &'static str,
        foreign_key: &'static str,
        related_key: &'static str,
        local_key: &'static str,
    ) -> Self {
        let mut r = Self::base(name, RelationType::BelongsToMany, model, foreign_key, local_key);
        r.pivot_table = Some(pivot_table);
        r.related_key = Some(related_key);
        r
    }

    /// Restrict selected columns of the related table.
    pub const fn columns(mut self, c: &'static [&'static str]) -> Self {
        self.columns = Some(c);
        self
    }
}

/// Extra relation joined & searched by `Model::search`.
#[derive(Debug, Clone)]
pub struct SearchableRelation {
    pub alias: &'static str,
    pub table: &'static str,
    pub foreign_key: &'static str,
    pub owner_key: &'static str,
    pub columns: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimestampFormat {
    /// `Y-m-d H:i:s`
    Datetime,
    /// Unix timestamp (integer)
    Unix,
}

// Lifecycle hooks.
pub type BeforeSave = fn(&mut Map<String, Value>, bool) -> bool;
pub type AfterLoad = fn(&mut Vec<Value>);
pub type AfterSave = fn(bool, &Map<String, Value>);
pub type BeforeDelete = fn(&Pk) -> bool;
pub type AfterDelete = fn(&Pk);

// ───────────────────────── Model ─────────────────────────

#[derive(Debug, Clone)]
pub struct Model {
    pub table: &'static str,
    pub pk: &'static str,
    /// Non-empty => composite primary key (overrides `pk`).
    pub composite_pk: &'static [&'static str],
    pub fillable: &'static [&'static str],
    pub hidden: &'static [&'static str],
    pub use_audit: bool,
    /// [created_at, updated_at, created_by, updated_by]
    pub audit_fields: [&'static str; 4],
    pub timestamp_format: TimestampFormat,
    pub default_order: Option<&'static str>,
    pub default_with: &'static [&'static str],
    pub relations: &'static [Relation],
    pub searchable_relations: &'static [SearchableRelation],
    pub before_save: Option<BeforeSave>,
    pub after_load: Option<AfterLoad>,
    pub after_save: Option<AfterSave>,
    pub before_delete: Option<BeforeDelete>,
    pub after_delete: Option<AfterDelete>,
}

/// 1:1 name with the PHP class.
pub type ActiveRecord = Model;

impl Model {
    pub const fn new(table: &'static str) -> Self {
        Self {
            table,
            pk: "id",
            composite_pk: &[],
            fillable: &[],
            hidden: &[],
            use_audit: true,
            audit_fields: ["created_at", "updated_at", "created_by", "updated_by"],
            timestamp_format: TimestampFormat::Datetime,
            default_order: None,
            default_with: &[],
            relations: &[],
            searchable_relations: &[],
            before_save: None,
            after_load: None,
            after_save: None,
            before_delete: None,
            after_delete: None,
        }
    }

    pub const fn primary_key(mut self, pk: &'static str) -> Self { self.pk = pk; self }
    pub const fn composite_key(mut self, k: &'static [&'static str]) -> Self { self.composite_pk = k; self }
    pub const fn fillable(mut self, f: &'static [&'static str]) -> Self { self.fillable = f; self }
    pub const fn hidden(mut self, h: &'static [&'static str]) -> Self { self.hidden = h; self }
    /// Alias of [`Model::audit`] (kept for backwards compatibility).
    pub const fn timestamps(mut self, t: bool) -> Self { self.use_audit = t; self }
    pub const fn audit(mut self, t: bool) -> Self { self.use_audit = t; self }
    pub const fn audit_fields(mut self, created_at: &'static str, updated_at: &'static str, created_by: &'static str, updated_by: &'static str) -> Self {
        self.audit_fields = [created_at, updated_at, created_by, updated_by];
        self
    }
    pub const fn timestamp_format(mut self, f: TimestampFormat) -> Self { self.timestamp_format = f; self }
    pub const fn default_order(mut self, o: &'static str) -> Self { self.default_order = Some(o); self }
    pub const fn default_with(mut self, w: &'static [&'static str]) -> Self { self.default_with = w; self }
    pub const fn relations(mut self, r: &'static [Relation]) -> Self { self.relations = r; self }
    pub const fn searchable_relations(mut self, s: &'static [SearchableRelation]) -> Self { self.searchable_relations = s; self }
    pub const fn before_save(mut self, h: BeforeSave) -> Self { self.before_save = Some(h); self }
    pub const fn after_load(mut self, h: AfterLoad) -> Self { self.after_load = Some(h); self }
    pub const fn after_save(mut self, h: AfterSave) -> Self { self.after_save = Some(h); self }
    pub const fn before_delete(mut self, h: BeforeDelete) -> Self { self.before_delete = Some(h); self }
    pub const fn after_delete(mut self, h: AfterDelete) -> Self { self.after_delete = Some(h); self }

    // ── metadata ──

    pub fn table_name(&self) -> &'static str { self.table }

    /// Primary key column names (single => 1 element).
    pub fn primary_key_name(&self) -> Vec<&'static str> {
        if self.composite_pk.is_empty() { vec![self.pk] } else { self.composite_pk.to_vec() }
    }

    fn is_composite(&self) -> bool { !self.composite_pk.is_empty() }

    /// Default eager loads (`$with`).
    pub fn get_with(&self) -> Vec<String> {
        self.default_with.iter().map(|s| s.to_string()).collect()
    }

    pub fn get_relation_config(&self, name: &str) -> Option<&Relation> {
        self.relations.iter().find(|r| r.name == name)
    }

    pub fn query(&self) -> Query { Query::table(self.table) }

    /// Fluent model-aware builder (`static::find()`).
    pub fn find_query(&self) -> ModelQuery { ModelQuery::new(self.clone()) }

    fn now_value(&self) -> Value {
        match self.timestamp_format {
            TimestampFormat::Unix => json!(chrono::Utc::now().timestamp()),
            TimestampFormat::Datetime => Value::String(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        }
    }

    // ── columns cache ──

    /// `clearColumnsCache()`
    pub fn clear_columns_cache() {
        if let Ok(mut c) = columns_cache().write() { c.clear(); }
    }

    /// Table columns, cached with TTL (`COLUMNS_CACHE_TTL`, default 3600s, 0 = forever).
    pub async fn get_table_columns(&self) -> Vec<String> {
        let ttl = env::get_int("COLUMNS_CACHE_TTL", 3600).max(0) as u64;
        let now = now_secs();
        if let Ok(c) = columns_cache().read() {
            if let Some((cols, at)) = c.get(self.table) {
                if ttl == 0 || now.saturating_sub(*at) < ttl {
                    return cols.clone();
                }
            }
        }
        let cols = self.fetch_columns().await.unwrap_or_default();
        if !cols.is_empty() {
            if let Ok(mut c) = columns_cache().write() {
                c.insert(self.table.to_string(), (cols.clone(), now));
            }
        }
        cols
    }

    async fn fetch_columns(&self) -> Result<Vec<String>, ApiError> {
        check_ident(self.table, "table name")?;
        let rows = if Database::is_sqlite() {
            Database::fetch_all(&format!("PRAGMA table_info({})", self.table), &[]).await?
        } else if Database::is_postgres() {
            Database::fetch_all(
                "SELECT column_name AS name FROM information_schema.columns WHERE table_name = ?",
                &[Val::from(self.table)],
            )
            .await?
        } else {
            Database::fetch_all(
                "SELECT CAST(column_name AS CHAR) AS name FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ?",
                &[Val::from(self.table)],
            )
            .await?
        };
        Ok(rows.iter().filter_map(|r| r["name"].as_str().map(String::from)).collect())
    }

    // ── presentation ──

    /// Strip hidden attributes from one row.
    pub fn present(&self, mut row: Value) -> Value {
        if let Some(o) = row.as_object_mut() {
            for h in self.hidden { o.remove(*h); }
        }
        row
    }

    /// `hideFields()` for a list of rows.
    pub fn hide_fields(&self, rows: Vec<Value>) -> Vec<Value> {
        if self.hidden.is_empty() { return rows; }
        rows.into_iter().map(|r| self.present(r)).collect()
    }

    /// `filterFillable()` - empty `fillable` allows everything.
    pub fn filter_fillable(&self, data: &Value) -> Map<String, Value> {
        let Some(o) = data.as_object() else { return Map::new() };
        if self.fillable.is_empty() { return o.clone(); }
        o.iter()
            .filter(|(k, _)| self.fillable.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// hide -> afterLoad hook -> eager load relations (`with`).
    pub async fn process_rows(&self, rows: Vec<Value>, with: &[String]) -> Result<Vec<Value>, ApiError> {
        let mut rows = self.hide_fields(rows);
        if let Some(h) = self.after_load { h(&mut rows); }
        if !with.is_empty() && !rows.is_empty() {
            self.load_relations(&mut rows, with).await?;
        }
        Ok(rows)
    }

    // ── primary key helpers ──

    /// `getPkConditions()`
    pub fn get_pk_conditions(&self, id: &Pk) -> Vec<(String, Value)> {
        let id = &id.0;
        if self.is_composite() {
            if let Some(o) = id.as_object() {
                return self.composite_pk.iter().map(|k| (k.to_string(), o.get(*k).cloned().unwrap_or(Value::Null))).collect();
            }
            if let Some(s) = id.as_str().filter(|s| s.contains('_')) {
                let vals: Vec<&str> = s.split('_').collect();
                return self.composite_pk.iter().enumerate()
                    .map(|(i, k)| (k.to_string(), vals.get(i).map(|v| json!(v)).unwrap_or(Value::Null)))
                    .collect();
            }
            return vec![(self.composite_pk[0].to_string(), id.clone())];
        }
        let v = id.as_object().map(|o| o.get(self.pk).cloned().unwrap_or(Value::Null)).unwrap_or_else(|| id.clone());
        vec![(self.pk.to_string(), v)]
    }

    fn pk_query(&self, id: &Pk) -> Query {
        let mut q = self.query();
        for (c, v) in self.get_pk_conditions(id) {
            q = q.where_eq(&c, Val::from(&v));
        }
        q
    }

    /// Resolve ORDER BY: explicit > `default_order` > `pk DESC` (single pk only).
    fn order_pairs(&self, order_by: Option<&str>) -> Result<Vec<(String, String)>, ApiError> {
        match order_by.or(self.default_order) {
            Some(o) => sanitize_order_by(o),
            None if !self.is_composite() => Ok(vec![(self.pk.to_string(), "DESC".into())]),
            None => Ok(vec![]),
        }
    }

    fn with_order(&self, mut q: Query, order_by: Option<&str>) -> Result<Query, ApiError> {
        for (c, d) in self.order_pairs(order_by)? { q = q.order_by(&c, &d); }
        Ok(q)
    }

    // ── audit ──

    /// Fill created_at/updated_at/created_by/updated_by (`beforeSave` default behaviour).
    async fn apply_audit(&self, data: &mut Map<String, Value>, insert: bool) {
        if !self.use_audit { return; }
        let cols = self.get_table_columns().await;
        let [ca, ua, cb, ub] = self.audit_fields;
        // If introspection failed (empty), still stamp timestamps so NOT NULL columns work.
        let has_ts = |c: &str| cols.is_empty() || cols.iter().any(|x| x == c);
        let has = |c: &str| cols.iter().any(|x| x == c);
        let unset = |d: &Map<String, Value>, c: &str| d.get(c).map_or(true, Value::is_null);
        let now = self.now_value();
        let user = current_user_id();

        if insert {
            if has_ts(ca) && unset(data, ca) { data.insert(ca.into(), now.clone()); }
            if has_ts(ua) && unset(data, ua) { data.insert(ua.into(), now.clone()); }
            if let Some(u) = user {
                if has(cb) && unset(data, cb) { data.insert(cb.into(), json!(u)); }
                if has(ub) && unset(data, ub) { data.insert(ub.into(), json!(u)); }
            }
        } else {
            if has_ts(ua) { data.insert(ua.into(), now); }
            if let Some(u) = user {
                if has(ub) { data.insert(ub.into(), json!(u)); }
            }
        }
    }

    /// audit + `beforeSave` hook. Returns false when the hook aborts.
    async fn before_save_all(&self, data: &mut Map<String, Value>, insert: bool) -> bool {
        self.apply_audit(data, insert).await;
        match self.before_save { Some(h) => h(data, insert), None => true }
    }

    fn invalidate_count(&self) {
        Cache::forget(&format!("table_count:{}", self.table));
    }

    // ── read ──

    /// `all($columns, $orderBy)`
    pub async fn all_with(&self, columns: &[&str], order_by: Option<&str>) -> Result<Vec<Value>, ApiError> {
        check_cols(columns)?;
        let mut q = self.with_order(self.query(), order_by)?;
        if !columns.is_empty() { q = q.select(columns); }
        let rows = q.get().await?;
        self.process_rows(rows, &self.get_with()).await
    }

    pub async fn all(&self) -> Result<Vec<Value>, ApiError> {
        self.all_with(&["*"], None).await
    }

    /// `findByPk($id, $columns)` (supports composite keys).
    pub async fn find_by_pk(&self, id: impl Into<Pk>, columns: &[&str]) -> Result<Option<Value>, ApiError> {
        check_cols(columns)?;
        let mut q = self.pk_query(&id.into());
        if !columns.is_empty() { q = q.select(columns); }
        match q.first().await? {
            Some(r) => Ok(self.process_rows(vec![r], &self.get_with()).await?.pop()),
            None => Ok(None),
        }
    }

    pub async fn find(&self, id: impl Into<Pk>) -> Result<Option<Value>, ApiError> {
        self.find_by_pk(id, &["*"]).await
    }

    /// Alias of `find_by_pk` (Yii2 style).
    pub async fn find_one(&self, id: impl Into<Pk>) -> Result<Option<Value>, ApiError> {
        self.find_by_pk(id, &["*"]).await
    }

    /// 404 when not found.
    pub async fn find_or_fail(&self, id: impl Into<Pk>) -> Result<Value, ApiError> {
        self.find(id).await?.ok_or_else(|| ApiError::not_found(format!("Record not found in {}", self.table)))
    }

    /// Raw row incl. hidden fields, no hooks (e.g. credential checks).
    pub async fn find_by(&self, col: &str, v: impl Into<Val>) -> Result<Option<Value>, ApiError> {
        check_ident(col, "column name")?;
        self.query().where_eq(col, v).first().await
    }

    /// `findAll($condition, $columns)`: list of primary keys (`[1,2,3]`) or hash conditions.
    pub async fn find_all(&self, condition: &Value, columns: &[&str]) -> Result<Vec<Value>, ApiError> {
        check_cols(columns)?;
        let mut q = self.find_query();
        if !columns.is_empty() { q = q.select(columns); }
        if let Some(list) = condition.as_array() {
            if self.is_composite() {
                return Err(ApiError::bad_request("findAll() with list of IDs is not supported for composite primary keys."));
            }
            q = q.where_in(self.pk, list.iter().map(Val::from).collect::<Vec<Val>>());
        } else {
            q = q.where_conditions(condition)?;
        }
        q.all().await
    }

    /// `where($conditions, $columns)` - hash conditions with AND.
    pub async fn where_conditions(&self, conditions: &Value, columns: &[&str]) -> Result<Vec<Value>, ApiError> {
        check_cols(columns)?;
        let mut q = apply_conditions(self.query(), conditions)?;
        if !columns.is_empty() { q = q.select(columns); }
        let rows = q.get().await?;
        self.process_rows(rows, &self.get_with()).await
    }

    /// `filterWhere()` - like `where_conditions` but skips null / "" / [] values,
    /// ordered like `all()`.
    pub async fn filter_where(&self, conditions: &Value, columns: &[&str]) -> Result<Vec<Value>, ApiError> {
        check_cols(columns)?;
        let mut q = self.with_order(apply_conditions(self.query(), &filter_conditions(conditions))?, None)?;
        if !columns.is_empty() { q = q.select(columns); }
        let rows = q.get().await?;
        self.process_rows(rows, &self.get_with()).await
    }

    /// `count($conditions)`
    pub async fn count(&self, conditions: &Value) -> Result<i64, ApiError> {
        apply_conditions(self.query(), conditions)?.count().await
    }

    pub async fn paginate(&self, page: u64, per_page: u64) -> Result<Value, ApiError> {
        self.paginate_where(page, per_page, &Value::Null, None).await
    }

    /// `paginate($page, $perPage, $conditions, $orderBy)` - count cached 300s.
    pub async fn paginate_where(&self, page: u64, per_page: u64, conditions: &Value, order_by: Option<&str>) -> Result<Value, ApiError> {
        let page = page.max(1);
        let per_page = per_page.max(1);
        let offset = (page - 1) * per_page;
        let q = apply_conditions(self.query(), conditions)?;

        let has_cond = conditions.as_object().map_or(false, |o| !o.is_empty());
        let key = if has_cond { format!("table_count:{}:{}", self.table, conditions) } else { format!("table_count:{}", self.table) };
        let total = match Cache::get(&key).and_then(|v| v.as_i64()) {
            Some(t) => t,
            None => {
                let t = q.count().await?;
                Cache::set(&key, json!(t), Some(300));
                t
            }
        };

        let rows = self.with_order(q, order_by)?.limit(per_page).offset(offset).get().await?;
        let data = self.process_rows(rows, &self.get_with()).await?;
        let t = total.max(0) as u64;
        Ok(json!({
            "data": data,
            "meta": {
                "total": total,
                "per_page": per_page,
                "current_page": page,
                "last_page": t.div_ceil(per_page),
                "from": offset + 1,
                "to": (offset + per_page).min(t),
            }
        }))
    }

    /// Raw SQL (`query($sql, $params)`).
    pub async fn raw(sql: &str, params: &[Val]) -> Result<Vec<Value>, ApiError> {
        Database::fetch_all(sql, params).await
    }

    // ── search ──

    /// `search($keyword)`: LIKE over fillable columns, audit users (creator/updater)
    /// and `searchable_relations`.
    pub async fn search(&self, keyword: &str) -> Result<ModelQuery, ApiError> {
        let term = format!("%{keyword}%");
        let like = if Database::is_postgres() { "ILIKE" } else { "LIKE" };
        let t = self.table;
        let mut parts: Vec<String> = Vec::new();
        let mut params: Vec<Val> = Vec::new();
        let mut push = |col: String| { parts.push(format!("{col} {like} ?")); params.push(Val::Str(term.clone())); };

        for f in self.fillable { push(format!("{t}.{f}")); }

        let mut q = self.find_query().select(&[&format!("{t}.*")]);

        if self.use_audit {
            let cols = self.get_table_columns().await;
            let [_, _, cb, ub] = self.audit_fields;
            if cols.is_empty() || cols.iter().any(|c| c == cb) {
                q = q.left_join("users AS creator", &format!("{t}.{cb}"), "=", "creator.id");
                push("creator.username".into());
            }
            if cols.is_empty() || cols.iter().any(|c| c == ub) {
                q = q.left_join("users AS updater", &format!("{t}.{ub}"), "=", "updater.id");
                push("updater.username".into());
            }
        }

        for r in self.searchable_relations {
            q = q.left_join(&format!("{} AS {}", r.table, r.alias), &format!("{t}.{}", r.foreign_key), "=", &format!("{}.{}", r.alias, r.owner_key));
            for c in r.columns { push(format!("{}.{c}", r.alias)); }
        }

        Ok(q.where_raw(&parts.join(" OR "), params))
    }

    // ── eager loading ──

    /// `loadRelations()`: supports `rel`, `rel:col1,col2`, `rel.nested`, comma separated.
    pub fn load_relations<'a>(
        &'a self,
        rows: &'a mut Vec<Value>,
        with: &'a [String],
    ) -> Pin<Box<dyn Future<Output = Result<(), ApiError>> + Send + 'a>> {
        Box::pin(async move {
            if rows.is_empty() { return Ok(()); }

            // flatten "a,b" (only when no ':' column spec) and group by base name
            let mut flat: Vec<String> = Vec::new();
            for w in with {
                if !w.contains(':') && w.contains(',') {
                    flat.extend(w.split(',').map(|s| s.trim().to_string()));
                } else {
                    flat.push(w.trim().to_string());
                }
            }
            let mut order: Vec<String> = Vec::new();
            let mut groups: HashMap<String, (Option<Vec<String>>, Vec<String>)> = HashMap::new();
            for item in &flat {
                let mut base = item.as_str();
                let mut nested = None;
                let mut cols = None;
                if let Some((b, n)) = base.split_once('.') { base = b; nested = Some(n); }
                if let Some((b, c)) = base.split_once(':') { base = b; cols = Some(c); }
                let base = base.trim().to_string();
                if !groups.contains_key(&base) {
                    order.push(base.clone());
                    groups.insert(base.clone(), (cols.map(|c| c.split(',').map(|s| s.trim().to_string()).collect()), vec![]));
                }
                if let Some(n) = nested { groups.get_mut(&base).unwrap().1.push(n.to_string()); }
            }

            for name in order {
                let Some(rel) = self.get_relation_config(&name) else { continue };
                let (cols_override, nested) = groups.remove(&name).unwrap();
                let related = (rel.model)();

                // unique scalar local keys
                let mut seen = HashSet::new();
                let mut ids: Vec<Val> = Vec::new();
                for r in rows.iter() {
                    if let Some(v) = r.get(rel.local_key) {
                        if matches!(v, Value::Null | Value::Array(_) | Value::Object(_)) || is_empty_value(v) { continue; }
                        if seen.insert(key_str(v)) { ids.push(Val::from(v)); }
                    }
                }
                if ids.is_empty() { continue; }

                // columns
                let mut cols: Option<Vec<String>> = cols_override.or_else(|| rel.columns.map(|c| c.iter().map(|s| s.to_string()).collect()));
                if let Some(c) = &cols {
                    if c.iter().any(|x| x == "*") { cols = None; }
                }
                if let Some(c) = &mut cols {
                    for x in c.iter() { check_ident(x, "column name")?; }
                    if rel.rel_type != RelationType::BelongsToMany && !c.iter().any(|x| x == rel.foreign_key) {
                        c.push(rel.foreign_key.to_string());
                    }
                }

                let is_many = rel.rel_type == RelationType::BelongsToMany;
                let mut data: Vec<Value> = if is_many {
                    let pivot = rel.pivot_table.unwrap_or_default();
                    let rkey = rel.related_key.unwrap_or_default();
                    for (v, w) in [(pivot, "pivot table"), (rkey, "related key"), (rel.foreign_key, "foreign key")] { check_ident(v, w)?; }
                    let sel = match &cols {
                        Some(c) => c.iter().map(|x| format!("rt.{x}")).collect::<Vec<_>>().join(", "),
                        None => "rt.*".into(),
                    };
                    let ph = vec!["?"; ids.len()].join(", ");
                    let sql = format!(
                        "SELECT pt.{fk} AS _pivot_key, {sel} FROM {pivot} pt JOIN {rt} rt ON pt.{rkey} = rt.{rpk} WHERE pt.{fk} IN ({ph})",
                        fk = rel.foreign_key, rt = related.table, rpk = related.pk
                    );
                    Database::fetch_all(&sql, &ids).await?
                } else {
                    let mut q = Query::table(related.table);
                    if let Some(c) = &cols {
                        let refs: Vec<&str> = c.iter().map(String::as_str).collect();
                        q = q.select(&refs);
                    }
                    q.where_in(rel.foreign_key, ids).get().await?
                };

                if let Some(h) = related.after_load { h(&mut data); }
                if !nested.is_empty() {
                    related.load_relations(&mut data, &nested).await?;
                } else if !related.default_with.is_empty() {
                    let dw = related.get_with();
                    related.load_relations(&mut data, &dw).await?;
                }

                // group by key
                let group_col = if is_many { "_pivot_key" } else { rel.foreign_key };
                let mut map: HashMap<String, Vec<Value>> = HashMap::new();
                for mut item in data {
                    let k = item.get(group_col).map(key_str).unwrap_or_default();
                    if is_many { if let Some(o) = item.as_object_mut() { o.remove("_pivot_key"); } }
                    map.entry(k).or_default().push(related.present(item));
                }

                let single = matches!(rel.rel_type, RelationType::BelongsTo | RelationType::HasOne);
                for row in rows.iter_mut() {
                    let Some(obj) = row.as_object_mut() else { continue };
                    let Some(lk) = obj.get(rel.local_key).filter(|v| !v.is_null()).map(key_str) else { continue };
                    let list = map.get(&lk).cloned().unwrap_or_default();
                    let v = if single { list.into_iter().next().unwrap_or(Value::Null) } else { Value::Array(list) };
                    obj.insert(rel.name.to_string(), v);
                }
            }
            Ok(())
        })
    }

    // ── write ──

    /// `insertRecord()`: returns the new id (None when unavailable / aborted by hook).
    pub async fn insert_record(&self, data: &Value) -> Result<Option<i64>, ApiError> {
        Ok(self.insert_inner(data).await?.map(|(_, id)| id).unwrap_or(None))
    }

    async fn insert_inner(&self, data: &Value) -> Result<Option<(Map<String, Value>, Option<i64>)>, ApiError> {
        let mut attrs = self.filter_fillable(data);
        if !self.before_save_all(&mut attrs, true).await { return Ok(None); }
        let payload = Value::Object(attrs.clone());
        let id = if self.is_composite() {
            self.query().insert(&payload).await?
        } else {
            self.query().insert_get_id(&payload, self.pk).await?
        };
        if let (Some(id), false) = (id, self.is_composite()) { attrs.insert(self.pk.into(), json!(id)); }
        if let Some(h) = self.after_save { h(true, &attrs); }
        self.invalidate_count();
        Ok(Some((attrs, id)))
    }

    /// `create()`: insert and return the stored row.
    pub async fn create(&self, data: &Value) -> Result<Value, ApiError> {
        let Some((attrs, id)) = self.insert_inner(data).await? else {
            return Err(ApiError::bad_request("Save aborted by beforeSave hook"));
        };
        match id {
            Some(id) if id > 0 => self.find_or_fail(id).await,
            _ => Ok(self.present(Value::Object(attrs))),
        }
    }

    /// `updateRecord()`: false when aborted by `before_save`.
    pub async fn update_record(&self, id: impl Into<Pk>, data: &Value) -> Result<bool, ApiError> {
        let id = id.into();
        let mut attrs = self.filter_fillable(data);
        if !self.before_save_all(&mut attrs, false).await { return Ok(false); }
        self.pk_query(&id).update(&Value::Object(attrs.clone())).await?;
        if let Some(h) = self.after_save {
            let mut save = attrs;
            for (c, v) in self.get_pk_conditions(&id) { save.insert(c, v); }
            h(false, &save);
        }
        Ok(true)
    }

    /// `update()`: 404 if missing, returns the updated row.
    pub async fn update(&self, id: impl Into<Pk>, data: &Value) -> Result<Value, ApiError> {
        let id = id.into();
        self.find_or_fail(id.clone()).await?;
        self.update_record(id.clone(), data).await?;
        self.find_or_fail(id).await
    }

    /// `updateAll($data, $conditions)`: returns affected rows (no conditions => all rows).
    pub async fn update_all(&self, data: &Value, conditions: &Value) -> Result<u64, ApiError> {
        let mut attrs = self.filter_fillable(data);
        if !self.before_save_all(&mut attrs, false).await { return Ok(0); }
        apply_conditions(self.query(), conditions)?.update(&Value::Object(attrs)).await
    }

    /// `deleteRecord()`: false when aborted by `before_delete`.
    pub async fn delete_record(&self, id: impl Into<Pk>) -> Result<bool, ApiError> {
        let id = id.into();
        if let Some(h) = self.before_delete { if !h(&id) { return Ok(false); } }
        self.pk_query(&id).delete().await?;
        if let Some(h) = self.after_delete { h(&id); }
        self.invalidate_count();
        Ok(true)
    }

    /// `delete()`: 404 if missing.
    pub async fn delete(&self, id: impl Into<Pk>) -> Result<(), ApiError> {
        let id = id.into();
        self.find_or_fail(id.clone()).await?;
        self.delete_record(id).await?;
        Ok(())
    }

    /// `deleteAll()`: list of primary keys (`[1,2,3]`) or hash conditions
    /// (empty => deletes every row, like PHP).
    pub async fn delete_all(&self, conditions: &Value) -> Result<u64, ApiError> {
        let q = match conditions.as_array() {
            Some(list) if !list.is_empty() => {
                if self.is_composite() {
                    return Err(ApiError::bad_request("deleteAll() with list of IDs is not supported for composite primary keys."));
                }
                self.query().where_in(self.pk, list.iter().map(Val::from).collect::<Vec<Val>>())
            }
            _ => apply_conditions(self.query(), conditions)?,
        };
        let n = q.delete().await?;
        if n > 0 { self.invalidate_count(); }
        Ok(n)
    }

    /// `batchInsert()`: chunked multi-row INSERT (default chunk 500). False when no rows.
    pub async fn batch_insert(&self, rows: &[Value], chunk_size: usize) -> Result<bool, ApiError> {
        if rows.is_empty() { return Ok(false); }
        let chunk_size = if chunk_size == 0 { 500 } else { chunk_size };
        for chunk in rows.chunks(chunk_size) {
            let mut prepared: Vec<Map<String, Value>> = Vec::new();
            for r in chunk {
                let mut a = self.filter_fillable(r);
                if self.before_save_all(&mut a, true).await { prepared.push(a); }
            }
            let Some(first) = prepared.first() else { continue };
            let cols: Vec<String> = first.keys().cloned().collect();
            if cols.is_empty() { continue; }
            for c in &cols { check_ident(c, "column name")?; }
            let row_ph = format!("({})", vec!["?"; cols.len()].join(", "));
            let values = vec![row_ph; prepared.len()].join(", ");
            let mut params: Vec<Val> = Vec::with_capacity(cols.len() * prepared.len());
            for row in &prepared {
                for c in &cols { params.push(row.get(c).map(Val::from).unwrap_or(Val::Null)); }
            }
            let sql = format!("INSERT INTO {} ({}) VALUES {}", self.table, cols.join(", "), values);
            Database::execute(&sql, &params).await?;
        }
        self.invalidate_count();
        Ok(true)
    }

    /// `upsert()`: MySQL `ON DUPLICATE KEY UPDATE`, SQLite/Postgres `ON CONFLICT (pk) DO UPDATE`.
    pub async fn upsert(&self, data: &Value, update_columns: &[&str]) -> Result<Option<i64>, ApiError> {
        let mut attrs = self.filter_fillable(data);
        if !self.before_save_all(&mut attrs, true).await { return Ok(None); }
        let cols: Vec<String> = attrs.keys().cloned().collect();
        if cols.is_empty() { return Ok(None); }
        for c in &cols { check_ident(c, "column name")?; }
        let upd: Vec<String> = if update_columns.is_empty() { cols.clone() } else { update_columns.iter().map(|s| s.to_string()).collect() };
        for c in &upd { check_ident(c, "column name")?; }

        let clause = if Database::is_sqlite() || Database::is_postgres() {
            let target = self.primary_key_name().join(", ");
            let sets: Vec<String> = upd.iter().filter(|c| !self.primary_key_name().contains(&c.as_str())).map(|c| format!("{c} = excluded.{c}")).collect();
            if sets.is_empty() { format!("ON CONFLICT ({target}) DO NOTHING") } else { format!("ON CONFLICT ({target}) DO UPDATE SET {}", sets.join(", ")) }
        } else {
            format!("ON DUPLICATE KEY UPDATE {}", upd.iter().map(|c| format!("{c} = VALUES({c})")).collect::<Vec<_>>().join(", "))
        };
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) {}",
            self.table, cols.join(", "), vec!["?"; cols.len()].join(", "), clause
        );
        let params: Vec<Val> = attrs.values().map(Val::from).collect();
        let (_, id) = Database::execute(&sql, &params).await?;
        self.invalidate_count();
        Ok(id)
    }
}

/// Drop null / "" / [] entries (`filterWhere` semantics).
fn filter_conditions(cond: &Value) -> Value {
    match cond.as_object() {
        Some(o) => Value::Object(o.iter().filter(|(_, v)| !is_empty_value(v)).map(|(k, v)| (k.clone(), v.clone())).collect()),
        None => Value::Null,
    }
}

// ───────────────────────── ModelQuery ─────────────────────────

/// Fluent model-aware query (`MyModel::find()->with()->where()->all()`).
#[derive(Debug, Clone)]
pub struct ModelQuery {
    pub model: Model,
    pub inner: Query,
    pub with_relations: Vec<String>,
}

impl ModelQuery {
    pub fn new(model: Model) -> Self {
        let inner = Query::table(model.table);
        let with_relations = model.get_with();
        let mut q = Self { model, inner, with_relations };
        if let Some(o) = q.model.default_order {
            if let Ok(pairs) = sanitize_order_by(o) {
                for (c, d) in pairs { q.inner = q.inner.order_by(&c, &d); }
            }
        }
        q
    }

    pub fn select(mut self, cols: &[&str]) -> Self { self.inner = self.inner.select(cols); self }
    pub fn select_raw(mut self, raw: &str) -> Self { self.inner = self.inner.select_raw(raw); self }

    pub fn with(mut self, relation: &str) -> Self { self.with_relations.push(relation.to_string()); self }
    pub fn with_many(mut self, relations: &[&str]) -> Self {
        self.with_relations.extend(relations.iter().map(|r| r.to_string()));
        self
    }
    /// `clearWith()`
    pub fn clear_with(mut self) -> Self { self.with_relations.clear(); self }

    pub fn where_eq(mut self, col: &str, v: impl Into<Val>) -> Self { self.inner = self.inner.where_eq(col, v); self }
    pub fn where_op(mut self, col: &str, op: &str, v: impl Into<Val>) -> Self { self.inner = self.inner.where_op(col, op, v); self }
    pub fn or_where(mut self, col: &str, op: &str, v: impl Into<Val>) -> Self { self.inner = self.inner.or_where(col, op, v); self }
    pub fn where_in<V: Into<Val>>(mut self, col: &str, vals: Vec<V>) -> Self { self.inner = self.inner.where_in(col, vals); self }
    pub fn where_not_in<V: Into<Val>>(mut self, col: &str, vals: Vec<V>) -> Self { self.inner = self.inner.where_not_in(col, vals); self }
    pub fn where_null(mut self, col: &str) -> Self { self.inner = self.inner.where_null(col); self }
    pub fn where_not_null(mut self, col: &str) -> Self { self.inner = self.inner.where_not_null(col); self }
    pub fn where_between(mut self, col: &str, a: impl Into<Val>, b: impl Into<Val>) -> Self { self.inner = self.inner.where_between(col, a, b); self }
    pub fn where_raw(mut self, sql: &str, params: Vec<Val>) -> Self { self.inner = self.inner.where_raw(sql, params); self }

    /// Hash conditions `{col: value}`.
    pub fn where_conditions(mut self, cond: &Value) -> Result<Self, ApiError> {
        self.inner = apply_conditions(self.inner, cond)?;
        Ok(self)
    }

    /// Hash conditions skipping null / "" / [].
    pub fn filter_where(self, cond: &Value) -> Result<Self, ApiError> {
        self.where_conditions(&filter_conditions(cond))
    }

    pub fn join(mut self, table: &str, a: &str, op: &str, b: &str) -> Self { self.inner = self.inner.join(table, a, op, b); self }
    pub fn left_join(mut self, table: &str, a: &str, op: &str, b: &str) -> Self { self.inner = self.inner.left_join(table, a, op, b); self }

    /// `joinWith()`: LEFT JOIN the tables of the named relations.
    pub fn join_with(mut self, relations: &[&str]) -> Result<Self, ApiError> {
        let t = self.model.table;
        for name in relations {
            let rel = self.model.get_relation_config(name)
                .ok_or_else(|| ApiError::bad_request(format!("Unknown relation: {name}")))?
                .clone();
            let related = (rel.model)();
            if rel.rel_type == RelationType::BelongsToMany {
                let pivot = rel.pivot_table.unwrap_or_default();
                self.inner = self.inner
                    .left_join(pivot, &format!("{pivot}.{}", rel.foreign_key), "=", &format!("{t}.{}", rel.local_key))
                    .left_join(related.table, &format!("{}.{}", related.table, related.pk), "=", &format!("{pivot}.{}", rel.related_key.unwrap_or_default()));
            } else {
                self.inner = self.inner.left_join(
                    related.table,
                    &format!("{}.{}", related.table, rel.foreign_key),
                    "=",
                    &format!("{t}.{}", rel.local_key),
                );
            }
        }
        Ok(self)
    }

    pub fn order_by(mut self, col: &str, dir: &str) -> Self { self.inner = self.inner.order_by(col, dir); self }
    pub fn group_by(mut self, col: &str) -> Self { self.inner = self.inner.group_by(col); self }
    pub fn limit(mut self, n: u64) -> Self { self.inner = self.inner.limit(n); self }
    pub fn offset(mut self, n: u64) -> Self { self.inner = self.inner.offset(n); self }

    pub async fn count(&self) -> Result<i64, ApiError> { self.inner.count().await }
    pub async fn exists(&self) -> Result<bool, ApiError> { self.inner.exists().await }

    pub async fn all(&self) -> Result<Vec<Value>, ApiError> {
        let rows = self.inner.get().await?;
        self.model.process_rows(rows, &self.with_relations).await
    }

    pub async fn one(&self) -> Result<Option<Value>, ApiError> {
        let mut q = self.clone();
        q.inner = q.inner.limit(1);
        Ok(q.all().await?.into_iter().next())
    }

    pub async fn one_or_fail(&self) -> Result<Value, ApiError> {
        self.one().await?.ok_or_else(|| ApiError::not_found(format!("Record not found in {}", self.model.table)))
    }

    pub async fn paginate(&self, page: u64, per_page: u64) -> Result<Value, ApiError> {
        let mut r = self.inner.paginate(page, per_page).await?;
        if let Some(Value::Array(rows)) = r.get_mut("data") {
            let taken = std::mem::take(rows);
            *rows = self.model.process_rows(taken, &self.with_relations).await?;
        }
        Ok(r)
    }
}

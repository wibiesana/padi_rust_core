//! Code Generator for Padi Rust Api Framework.
//!
//! Generates Model, Controller, Resource, Routes, and SQL Migrations.
//! Supports:
//! - Protected tables guard (`users`, `migrations`, `password_resets`, `_sqlx_migrations`, `jobs`)
//! - Database table column introspection (SQLite PRAGMA, MySQL INFORMATION_SCHEMA, PostgreSQL columns)
//! - Bulk generation (`generate_crud_all`) for all tables in the database
//! - Automatic registration into `mod.rs` files (models, resources, controllers, routes with route prefix nesting)

use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Local;
use serde_json::{json, Value};

use crate::{
    database::{Database, Val},
    error::ApiError,
};

/// Internal tables protected from accidental generator overwrites.
pub const PROTECTED_TABLES: &[&str] = &[
    "users",
    "password_resets",
    "migrations",
    "_sqlx_migrations",
    "jobs",
];

#[derive(Debug, Clone)]
pub struct DetectedRelation {
    pub column: String,
    pub relation_name: String,
    pub target_table: String,
    pub target_const: String,
    pub target_pk: String,
    pub display_col: String,
}

pub struct Generator {
    pub base_dir: PathBuf,
}

impl Generator {
    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    /// Check if a table name is in the protected core tables list.
    pub fn is_protected_table(table_name: &str) -> bool {
        let lower = table_name.to_ascii_lowercase();
        PROTECTED_TABLES.contains(&lower.as_str())
    }

    /// Helper to ensure parent directory exists and write file content.
    fn write_file(path: &Path, content: impl AsRef<[u8]>) -> Result<(), ApiError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content)?;
        Ok(())
    }

    /// Introspect table columns from database (SQLite / MySQL / Postgres).
    pub async fn get_table_columns(&self, table_name: &str) -> Vec<String> {
        let (sql, key, params) = if Database::is_sqlite() {
            (format!("PRAGMA table_info({})", table_name), "name", vec![])
        } else if Database::is_postgres() {
            (
                "SELECT column_name FROM information_schema.columns WHERE table_name = ? ORDER BY ordinal_position".to_string(),
                "column_name",
                vec![Val::from(table_name)],
            )
        } else {
            // MySQL / MariaDB
            (
                "SELECT column_name FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? ORDER BY ordinal_position".to_string(),
                "column_name",
                vec![Val::from(table_name)],
            )
        };

        Database::fetch_all(&sql, &params)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|r| r[key].as_str().map(String::from))
            .collect()
    }

    /// Detect whether table uses unix timestamp (integer) or datetime for created_at/updated_at.
    pub async fn detect_timestamp_format(&self, table_name: &str) -> Option<&'static str> {
        let (sql, key, type_key, params) = if Database::is_sqlite() {
            (format!("PRAGMA table_info({})", table_name), "name", "type", vec![])
        } else if Database::is_postgres() {
            (
                "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = ?".to_string(),
                "column_name",
                "data_type",
                vec![Val::from(table_name)],
            )
        } else {
            (
                "SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ?".to_string(),
                "column_name",
                "data_type",
                vec![Val::from(table_name)],
            )
        };
        let rows = Database::fetch_all(&sql, &params).await.unwrap_or_default();
        for r in rows {
            if let Some(col) = r[key].as_str() {
                if col == "created_at" || col == "updated_at" {
                    let dt = r.get(type_key).and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                    if dt.contains("int") {
                        return Some("Unix");
                    }
                }
            }
        }
        None
    }

    /// Introspect primary key column(s) from database (SQLite / MySQL / Postgres).
    pub async fn get_table_primary_keys(&self, table_name: &str) -> Vec<String> {
        let (sql, key, params) = if Database::is_sqlite() {
            (format!("PRAGMA table_info({})", table_name), "name", vec![])
        } else if Database::is_postgres() {
            (
                "SELECT kcu.column_name FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu ON tc.constraint_name = kcu.constraint_name \
                 WHERE tc.constraint_type = 'PRIMARY KEY' AND tc.table_name = ? \
                 ORDER BY kcu.ordinal_position".to_string(),
                "column_name",
                vec![Val::from(table_name)],
            )
        } else {
            // MySQL / MariaDB
            (
                "SELECT COLUMN_NAME FROM information_schema.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND CONSTRAINT_NAME = 'PRIMARY' \
                 ORDER BY ORDINAL_POSITION".to_string(),
                "COLUMN_NAME",
                vec![Val::from(table_name)],
            )
        };

        if Database::is_sqlite() {
            let rows = Database::fetch_all(&sql, &params).await.unwrap_or_default();
            rows.into_iter()
                .filter(|r| r.get("pk").and_then(|v| v.as_i64()).unwrap_or(0) > 0)
                .filter_map(|r| r.get("name").and_then(|v| v.as_str()).map(String::from))
                .collect()
        } else {
            let rows = Database::fetch_all(&sql, &params).await.unwrap_or_default();
            rows.into_iter()
                .filter_map(|r| r.get(key).and_then(|v| v.as_str()).map(String::from))
                .collect()
        }
    }

    /// Get all user tables in the database (SQLite / MySQL / Postgres).
    pub async fn get_all_tables(&self) -> Vec<String> {
        let (sql, key): (&str, Option<&str>) = if Database::is_sqlite() {
            (
                "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                Some("name"),
            )
        } else if Database::is_postgres() {
            (
                "SELECT tablename FROM pg_tables WHERE schemaname = 'public'",
                Some("tablename"),
            )
        } else {
            // MySQL
            ("SHOW TABLES", None)
        };

        let rows = Database::fetch_all(sql, &[]).await.unwrap_or_default();
        rows.into_iter()
            .filter_map(|r| match key {
                Some(k) => r[k].as_str().map(String::from),
                None => r.as_object().and_then(|obj| {
                    obj.values().next().and_then(|v| v.as_str()).map(String::from)
                }),
            })
            .collect()
    }

    /// Introspect table foreign key relations based on DB foreign key constraints AND table structure.
    pub async fn get_table_relations(&self, table_name: &str) -> Vec<DetectedRelation> {
        let mut relations = Vec::new();

        // 1. Check database foreign key metadata from DB schema
        let fk_rows = if Database::is_sqlite() {
            Database::fetch_all(&format!("PRAGMA foreign_key_list({})", table_name), &[]).await.unwrap_or_default()
        } else if Database::is_postgres() {
            let sql = "SELECT kcu.column_name AS \"from\", ccu.table_name AS \"table\", ccu.column_name AS \"to\" \
                       FROM information_schema.table_constraints AS tc \
                       JOIN information_schema.key_column_usage AS kcu ON tc.constraint_name = kcu.constraint_name AND tc.table_schema = kcu.table_schema \
                       JOIN information_schema.constraint_column_usage AS ccu ON ccu.constraint_name = tc.constraint_name AND ccu.table_schema = tc.table_schema \
                       WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_name = ?";
            Database::fetch_all(sql, &[Val::from(table_name)]).await.unwrap_or_default()
        } else {
            // MySQL
            let sql = "SELECT COLUMN_NAME AS `from`, REFERENCED_TABLE_NAME AS `table`, REFERENCED_COLUMN_NAME AS `to` \
                       FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE \
                       WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND REFERENCED_TABLE_NAME IS NOT NULL";
            Database::fetch_all(sql, &[Val::from(table_name)]).await.unwrap_or_default()
        };

        let mut handled_cols = std::collections::HashSet::new();

        for r in fk_rows {
            let from_col = r.get("from").and_then(|v| v.as_str()).unwrap_or("");
            let target_tbl = r.get("table").and_then(|v| v.as_str()).unwrap_or("");
            let to_col = r.get("to").and_then(|v| v.as_str()).unwrap_or("id");

            if !from_col.is_empty() && !target_tbl.is_empty() {
                let rel_name = from_col.strip_suffix("_id").unwrap_or(from_col).to_string();
                let target_const = Self::screaming_snake_case(target_tbl);
                
                // Introspect target table to pick best display column (username > name > title > id)
                let target_cols = self.get_table_columns(target_tbl).await;
                let display_col = Self::pick_display_column(target_tbl, &target_cols);

                relations.push(DetectedRelation {
                    column: from_col.to_string(),
                    relation_name: rel_name,
                    target_table: target_tbl.to_string(),
                    target_const,
                    target_pk: to_col.to_string(),
                    display_col: display_col.to_string(),
                });
                handled_cols.insert(from_col.to_string());
            }
        }

        relations
    }

    /// Pick appropriate display column for a table (e.g. username, name, title)
    pub fn pick_display_column(target_tbl: &str, target_cols: &[String]) -> &'static str {
        let is_user_table = target_tbl == "users" || target_tbl.ends_with("_users");
        if is_user_table && target_cols.iter().any(|c| c == "username") {
            return "username";
        }
        let candidates = [
            "username", "name", "nama", "title", "judul", "email", "full_name", "code", "id",
        ];
        for cand in candidates {
            if target_cols.iter().any(|c| c == cand) {
                return cand;
            }
        }
        "id"
    }

    /// Normalize an input name (e.g. `Product`, `products`, `OrderItem`, `order_items`)
    /// into `(table_name, singular_file_stem, struct_name, const_name, plural_file_stem)`.
    pub fn normalize_names(input: &str) -> (String, String, String, String, String) {
        let is_pascal = input.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
        let snake = Self::snake_case(input);

        let (table_name, singular) = if is_pascal {
            let sing = Self::singularize(&snake);
            let tbl = Self::pluralize(&sing);
            (tbl, sing)
        } else {
            let sing = Self::singularize(&snake);
            (snake.clone(), sing)
        };

        let struct_name = Self::pascal_case(&singular);
        let const_name = Self::screaming_snake_case(&table_name);
        let plural = Self::snake_case(&table_name);

        (table_name, singular, struct_name, const_name, plural)
    }

    /// Generate Model with Two-Tier Architecture (base/ and concrete).
    pub async fn generate_model(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, singular, _struct_name, const_name, _plural) = Self::normalize_names(name);

        if Self::is_protected_table(&table_name) && !force {
            eprintln!(
                "\x1b[33m⚠️ Table '{}' is a protected core table. Skipping model generation.\x1b[0m",
                table_name
            );
            return Ok(());
        }

        // Introspect fillable columns (exclude id, created_at, updated_at, created_by, updated_by)
        let cols = self.get_table_columns(&table_name).await;
        let fillable: Vec<String> = cols
            .iter()
            .filter(|c| {
                !matches!(
                    c.as_str(),
                    "id" | "created_at" | "updated_at" | "created_by" | "updated_by"
                )
            })
            .cloned()
            .collect();

        // Introspect relations
        let relations = self.get_table_relations(&table_name).await;
        let mut rel_chains = String::new();
        if !relations.is_empty() {
            let rel_lines: Vec<String> = relations
                .iter()
                .map(|r| {
                    format!(
                        "        Relation::belongs_to(\"{}\", || crate::models::{}, \"{}\", \"{}\")\n            .columns(&[\"id\", \"{}\"]),",
                        r.relation_name, r.target_const, r.column, r.target_pk, r.display_col
                    )
                })
                .collect();
            let with_items: Vec<String> = relations
                .iter()
                .map(|r| format!("\"{}\"", r.relation_name))
                .collect();
            rel_chains = format!(
                "\n    .relations(&[\n{}\n    ])\n    .default_with(&[{}])",
                rel_lines.join("\n"),
                with_items.join(", ")
            );
        }

        // Detect sensitive fields for .hidden(...)
        let sensitive_fields: Vec<String> = cols
            .iter()
            .filter(|c| {
                let lower = c.to_lowercase();
                lower.contains("password")
                    || lower.contains("token")
                    || lower.contains("secret")
                    || lower.contains("key")
            })
            .cloned()
            .collect();

        let hidden_chain = if !sensitive_fields.is_empty() {
            let hidden_repr = sensitive_fields
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", ");
            format!("\n    .hidden(&[{}])", hidden_repr)
        } else {
            String::new()
        };

        // Introspect primary keys
        let pks = self.get_table_primary_keys(&table_name).await;
        let pk_chain = if pks.len() > 1 {
            let pk_repr = pks.iter().map(|k| format!("\"{}\"", k)).collect::<Vec<_>>().join(", ");
            format!("\n    .composite_key(&[{}])", pk_repr)
        } else if pks.len() == 1 && pks[0] != "id" {
            format!("\n    .primary_key(\"{}\")", pks[0])
        } else {
            String::new()
        };

        // Detect timestamp format (unix vs datetime)
        let ts_chain = if self.detect_timestamp_format(&table_name).await == Some("Unix") {
            "\n    .timestamp_format(TimestampFormat::Unix)"
        } else {
            ""
        };

        // 1. Tier 1: Base Model (ALWAYS regenerated/overwritten)
        let base_path = self
            .base_dir
            .join("src/models/base")
            .join(format!("{}_base.rs", singular));

        let base_content = if fillable.is_empty() {
            format!(
                r#"use padi_core::prelude::*;

/// Base model for table `{table_name}`.
/// Auto-generated by Padi Framework. DO NOT EDIT DIRECTLY.
/// This file is regenerated on every code generation.
#[allow(dead_code)]
pub const BASE_{const_name}: Model = Model::new("{table_name}"){pk_chain}{ts_chain}{hidden_chain}{rel_chains};
"#
            )
        } else {
            let fillable_repr = fillable
                .iter()
                .map(|c| format!("\"{}\"", c))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                r#"use padi_core::prelude::*;

/// Base model for table `{table_name}`.
/// Auto-generated by Padi Framework. DO NOT EDIT DIRECTLY.
/// This file is regenerated on every code generation.
#[allow(dead_code)]
pub const BASE_{const_name}: Model = Model::new("{table_name}"){pk_chain}
    .fillable(&[{fillable_repr}]){ts_chain}{hidden_chain}{rel_chains};
"#
            )
        };

        Self::write_file(&base_path, base_content)?;
        Self::update_models_mod_rs(&self.base_dir, &singular, &const_name)?;
        println!("\x1b[32m✓ Base Model generated (overwritten):\x1b[0m {:?}", base_path);

        // 2. Tier 2: Concrete Model (Generated once, NEVER overwritten)
        let concrete_path = self
            .base_dir
            .join("src/models")
            .join(format!("{}.rs", singular));

        if concrete_path.exists() {
            println!(
                "\x1b[33mℹ Concrete model already exists at {:?}. Preserving custom logic.\x1b[0m",
                concrete_path
            );
        } else {
            let concrete_content = format!(
                r#"pub use super::base::{singular}_base::BASE_{const_name} as {const_name};

// Add custom business logic, custom queries, hooks, and relationships below:
"#
            );
            Self::write_file(&concrete_path, concrete_content)?;
            println!("\x1b[32m✓ Concrete Model created:\x1b[0m {:?}", concrete_path);
        }

        Ok(())
    }

    /// Generate API Resource without base; if already exists, NEVER overwrite it.
    pub async fn generate_resource(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, singular, struct_name, _const_name, _plural) = Self::normalize_names(name);

        if Self::is_protected_table(&table_name) && !force {
            eprintln!(
                "\x1b[33m⚠️ Table '{}' is a protected core table. Skipping resource generation.\x1b[0m",
                table_name
            );
            return Ok(());
        }

        let resource_struct = format!("{}Resource", struct_name);
        let file_stem = &singular;
        let path = self
            .base_dir
            .join("src/resources")
            .join(format!("{}.rs", file_stem));

        if path.exists() {
            println!(
                "\x1b[33mℹ Resource already exists at {:?}. Preserving existing custom resource.\x1b[0m",
                path
            );
            return Ok(());
        }

        let cols = self.get_table_columns(&table_name).await;
        let relations = self.get_table_relations(&table_name).await;

        let content = if cols.is_empty() {
            format!(
                r#"use padi_core::prelude::*;

#[allow(dead_code)]
pub struct {resource_struct};

impl Resource for {resource_struct} {{
    fn to_array(item: &Value) -> Value {{
        item.clone()
    }}
}}
"#
            )
        } else {
            let mut entries = Vec::new();
            let sensitive_patterns = ["password", "token", "secret", "key"];
            for c in &cols {
                let lower = c.to_lowercase();
                if sensitive_patterns.iter().any(|p| lower.contains(p)) {
                    continue;
                }
                entries.push(format!("            \"{}\": item[\"{}\"],", c, c));
            }

            if !relations.is_empty() {
                entries.push("\n            // Relations".to_string());
                for rel in &relations {
                    entries.push(format!(
                        "            \"{}\": when_loaded(item, \"{}\").unwrap_or_else(|| json!({{ \"id\": item[\"{}\"] }})),",
                        rel.relation_name, rel.relation_name, rel.column
                    ));
                }
            }

            let shape_entries = entries.join("\n");

            format!(
                r#"use padi_core::prelude::*;

#[allow(dead_code)]
pub struct {resource_struct};

impl Resource for {resource_struct} {{
    fn to_array(item: &Value) -> Value {{
        json!({{
{shape_entries}
        }})
    }}
}}
"#
            )
        };

        Self::write_file(&path, content)?;
        Self::update_resources_mod_rs(&self.base_dir, file_stem, &resource_struct)?;
        println!("\x1b[32m✓ Resource created:\x1b[0m {:?}", path);
        Ok(())
    }

    /// Generate Controller with Two-Tier Architecture (base/ and concrete) and automatic input validation.
    pub async fn generate_controller(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, _singular, struct_name, const_name, plural) = Self::normalize_names(name);

        if Self::is_protected_table(&table_name) && !force {
            eprintln!(
                "\x1b[33m⚠️ Table '{}' is a protected core table. Skipping controller generation.\x1b[0m",
                table_name
            );
            return Ok(());
        }

        // Introspect table columns to build validation rules
        let cols = self.get_table_columns(&table_name).await;
        let mut store_rules = Vec::new();
        let mut update_rules = Vec::new();

        for col in &cols {
            let c_lower = col.to_ascii_lowercase();
            if matches!(c_lower.as_str(), "id" | "created_at" | "updated_at" | "created_by" | "updated_by") {
                continue;
            }

            let (store_rule, update_rule) = if c_lower.contains("email") {
                ("required|email", "sometimes|email")
            } else if c_lower.contains("password") {
                ("required|string|min:8", "sometimes|string|min:8")
            } else if c_lower.ends_with("_id") || c_lower.contains("count") || c_lower.contains("qty") || c_lower.contains("stock") || c_lower.contains("age") {
                ("required|integer", "sometimes|integer")
            } else if c_lower.contains("price") || c_lower.contains("amount") || c_lower.contains("cost") {
                ("required|numeric", "sometimes|numeric")
            } else if c_lower.contains("title") || c_lower.contains("name") {
                ("required|string|min:2|max:255", "sometimes|string|min:2|max:255")
            } else if c_lower.contains("slug") {
                ("sometimes|string", "sometimes|string")
            } else if c_lower.contains("status") {
                ("sometimes|string", "sometimes|string")
            } else {
                ("required|string", "sometimes|string")
            };

            store_rules.push(format!("            (\"{}\", \"{}\"),", col, store_rule));
            update_rules.push(format!("            (\"{}\", \"{}\"),", col, update_rule));
        }

        let store_body = if store_rules.is_empty() {
            format!(
                r#"    let item = {const_name}.create(&body).await?;
    created({struct_name}Resource::wrap(&item))"#
            )
        } else {
            let store_rules_str = store_rules.join("\n");
            format!(
                r#"    let data = Validator::new(
        &body,
        &[
{store_rules_str}
        ],
    )
    .validate()
    .await?;
    let item = {const_name}.create(&data).await?;
    created({struct_name}Resource::wrap(&item))"#
            )
        };

        let update_body = if update_rules.is_empty() {
            format!(
                r#"    let item = {const_name}.update(id, &body).await?;
    ok({struct_name}Resource::wrap(&item))"#
            )
        } else {
            let update_rules_str = update_rules.join("\n");
            format!(
                r#"    let data = Validator::new(
        &body,
        &[
{update_rules_str}
        ],
    )
    .validate()
    .await?;
    let item = {const_name}.update(id, &data).await?;
    ok({struct_name}Resource::wrap(&item))"#
            )
        };

        // 1. Tier 1: Base Controller (ALWAYS regenerated/overwritten)
        let base_path = self
            .base_dir
            .join("src/controllers/base")
            .join(format!("{}_base.rs", plural));

        let base_content = format!(
            r#"#![allow(dead_code, unused_imports)]
use padi_core::prelude::*;

use crate::{{models::{const_name}, resources::{struct_name}Resource}};

/// Base controller for `{table_name}`.
/// Auto-generated by Padi Framework. DO NOT EDIT DIRECTLY.
/// This file is regenerated on every code generation.

pub async fn index(_auth: AuthUser, p: Params) -> ApiResult {{
    ok({struct_name}Resource::wraps(&{const_name}.paginate_params(&p).await?))
}}

pub async fn all(_auth: AuthUser) -> ApiResult {{
    ok({struct_name}Resource::wraps(&json!({const_name}.all().await?)))
}}

pub async fn show(_auth: AuthUser, Path(id): Path<i64>) -> ApiResult {{
    ok({struct_name}Resource::wrap(&{const_name}.find_or_fail(id).await?))
}}

pub async fn store(_auth: AuthUser, Body(body): Body) -> ApiResult {{
{store_body}
}}

pub async fn update(_auth: AuthUser, Path(id): Path<i64>, Body(body): Body) -> ApiResult {{
{update_body}
}}

pub async fn destroy(_auth: AuthUser, Path(id): Path<i64>) -> ApiResult {{
    {const_name}.delete(id).await?;
    no_content()
}}
"#
        );

        Self::write_file(&base_path, base_content)?;
        Self::update_controllers_mod_rs(&self.base_dir, &plural)?;
        println!("\x1b[32m✓ Base Controller generated (overwritten):\x1b[0m {:?}", base_path);

        // 2. Tier 2: Concrete Controller (Generated once, NEVER overwritten)
        let concrete_path = self
            .base_dir
            .join("src/controllers")
            .join(format!("{}.rs", plural));

        if concrete_path.exists() {
            println!(
                "\x1b[33mℹ Concrete controller already exists at {:?}. Preserving custom actions.\x1b[0m",
                concrete_path
            );
        } else {
            let concrete_content = format!(
                r#"#[allow(unused_imports)]
pub use super::base::{plural}_base::*;

// Add or override custom action handlers below:
"#
            );
            Self::write_file(&concrete_path, concrete_content)?;
            println!("\x1b[32m✓ Concrete Controller created:\x1b[0m {:?}", concrete_path);
        }

        Ok(())
    }

    /// Generate Route and mount it into `src/routes/mod.rs` API router.
    pub fn generate_route(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (_table_name, _singular, _struct_name, _const_name, plural) = Self::normalize_names(name);
        let route_prefix = Self::kebab_case(&plural);

        let path = self
            .base_dir
            .join("src/routes")
            .join(format!("{}.rs", plural));

        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Route already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        let content = format!(
            r#"use padi_core::prelude::*;

use crate::controllers::{plural};

pub fn routes() -> Router {{
    Router::new()
        .route("/", get({plural}::index).post({plural}::store))
        .route("/all", get({plural}::all))
        .route("/{{id}}", get({plural}::show).put({plural}::update).delete({plural}::destroy))
}}
"#
        );

        Self::write_file(&path, content)?;
        Self::update_routes_mod_rs(&self.base_dir, &plural, &route_prefix)?;
        println!("\x1b[32m✓ Route created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate SQL Migration
    pub fn generate_migration(&self, name: &str) -> Result<(), ApiError> {
        let timestamp = Local::now().format("%Y_%m_%d_%H%M%S").to_string();
        let file_name = format!("{}_{}.sql", timestamp, Self::snake_case(name));
        let dir = self.base_dir.join("database/migrations");
        let path = dir.join(file_name);

        let table_name = Self::infer_table_name_from_migration(name);
        let template = format!(
            r#"-- Migration: {name}
CREATE TABLE IF NOT EXISTS {table} (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
);

-- +down
DROP TABLE IF EXISTS {table};
"#,
            name = name,
            table = table_name
        );

        Self::write_file(&path, template)?;
        println!("\x1b[32m✓ Migration created:\x1b[0m {:?}", path);
        Ok(())
    }

    /// Generate complete CRUD (Model + Resource + Controller + Route) for a single table.
    pub async fn generate_crud(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, _singular, _struct_name, _const_name, _plural) = Self::normalize_names(name);

        if Self::is_protected_table(&table_name) && !force {
            eprintln!(
                "\x1b[33m⚠️ Table '{}' is a protected core table. Skipping CRUD generation.\x1b[0m",
                table_name
            );
            return Ok(());
        }

        println!("Generating CRUD for table: {}", table_name);
        println!("{}", "=".repeat(60));

        println!("\n1. Generating Model...");
        self.generate_model(name, force).await?;

        println!("\n2. Generating Resource...");
        self.generate_resource(name, force).await?;

        println!("\n3. Generating Controller...");
        self.generate_controller(name, force).await?;

        println!("\n4. Generating Route...");
        self.generate_route(name, force)?;

        println!("\n5. Updating API Collection...");
        let _ = self.generate_api_collection(name, force).await;

        println!("\n{}", "=".repeat(60));
        println!("\x1b[32m✓ CRUD generated successfully for table: {}\x1b[0m\n", table_name);

        Ok(())
    }

    /// Generate complete CRUD for ALL tables discovered in the database.
    pub async fn generate_crud_all(&self, force: bool) -> Result<(), ApiError> {
        println!("\x1b[33mDiscovering database tables...\x1b[0m");
        let tables = self.get_all_tables().await;

        if tables.is_empty() {
            println!("\x1b[33mNo tables found in database or unable to connect.\x1b[0m");
            return Ok(());
        }

        println!("Found {} table(s) in database.\n", tables.len());
        let mut count = 0;
        for table in &tables {
            if Self::is_protected_table(table) && !force {
                println!("\x1b[90mSkipping protected/internal table '{}'\x1b[0m", table);
                continue;
            }
            self.generate_crud(table, force).await?;
            count += 1;
        }

        println!(
            "\x1b[32m✓ Bulk CRUD generation completed! ({} table(s) processed)\x1b[0m",
            count
        );
        Ok(())
    }

    /// Generate or update API Collection for a table (api_collection/<table_name>_collection.json).
    pub async fn generate_api_collection(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, singular, struct_name, _const_name, plural) = Self::normalize_names(name);
        
        // Clean up legacy combined api_collection.json files if they exist
        let legacy_path = self.base_dir.join("api_collection.json");
        if legacy_path.exists() {
            let _ = fs::remove_file(&legacy_path);
        }
        let legacy_coll = self.base_dir.join("api_collection/api_collection.json");
        if legacy_coll.exists() {
            let _ = fs::remove_file(&legacy_coll);
        }

        let single_path = self.base_dir.join(format!("api_collection/{}_collection.json", table_name));
        if single_path.exists() && !force {
            println!("\x1b[33mℹ API Collection for '{}' already exists at {:?}. Skipping (use --force to overwrite).\x1b[0m", table_name, single_path);
            return Ok(());
        }

        let route_prefix = table_name.replace('_', "-");
        let id_var_key = format!("{}_id", singular);

        // Get table columns to create realistic dummy sample JSON body
        let cols = self.get_table_columns(&table_name).await;
        let mut sample_create_obj = serde_json::Map::new();
        let mut sample_update_obj = serde_json::Map::new();

        for col in &cols {
            let c_lower = col.to_ascii_lowercase();
            if matches!(c_lower.as_str(), "id" | "created_at" | "updated_at" | "created_by" | "updated_by") {
                continue;
            }
            let val = if c_lower.contains("email") {
                json!("sample@example.com")
            } else if c_lower.contains("password") {
                json!("Secret123!")
            } else if c_lower.contains("price") || c_lower.contains("amount") || c_lower.contains("cost") {
                json!(99.99)
            } else if c_lower.contains("qty") || c_lower.contains("stock") || c_lower.contains("count") || c_lower.contains("age") {
                json!(10)
            } else if c_lower.starts_with("is_") || c_lower.starts_with("has_") {
                json!(true)
            } else if c_lower.contains("phone") {
                json!("08123456789")
            } else if c_lower.contains("date") {
                json!("2026-01-01")
            } else {
                json!(format!("Sample {}", Self::pascal_case(&col)))
            };

            sample_create_obj.insert(col.clone(), val.clone());
            if sample_update_obj.len() < 2 {
                sample_update_obj.insert(col.clone(), val);
            }
        }

        let create_body_raw = if sample_create_obj.is_empty() {
            format!("{{\n    \"name\": \"Sample {}\"\n}}", struct_name)
        } else {
            serde_json::to_string_pretty(&Value::Object(sample_create_obj)).unwrap_or_default()
        };

        let update_body_raw = if sample_update_obj.is_empty() {
            format!("{{\n    \"name\": \"Updated {}\"\n}}", struct_name)
        } else {
            serde_json::to_string_pretty(&Value::Object(sample_update_obj)).unwrap_or_default()
        };

        let test_script = format!(
            "try {{\n    if (pm.response.code === 201 || pm.response.code === 200) {{\n        const res = pm.response.json();\n        const id = res.data?.id || res.data?.{}?.id;\n        if (id) pm.collectionVariables.set('{}_id', id);\n    }}\n}} catch (e) {{ console.warn('Script bypassed'); }}",
            singular, singular
        );

        let items = json!([
            {
                "name": format!("List {}", Self::pascal_case(&plural)),
                "request": {
                    "method": "GET",
                    "url": {
                        "raw": format!("{{{{base_url}}}}/{}?page=1&per-page=25&search=&sort_by=id&order=desc", route_prefix)
                    }
                }
            },
            {
                "name": format!("List All {}", Self::pascal_case(&plural)),
                "request": {
                    "method": "GET",
                    "url": {
                        "raw": format!("{{{{base_url}}}}/{}/all?search=&sort_by=id&order=desc", route_prefix)
                    }
                }
            },
            {
                "name": format!("Get {} By ID", struct_name),
                "request": {
                    "method": "GET",
                    "url": {
                        "raw": format!("{{{{base_url}}}}/{}/{{{{{}_id}}}}", route_prefix, singular)
                    }
                }
            },
            {
                "name": format!("Create {}", struct_name),
                "event": [
                    {
                        "listen": "test",
                        "script": {
                            "exec": test_script.lines().map(String::from).collect::<Vec<_>>(),
                            "type": "text/javascript"
                        }
                    }
                ],
                "request": {
                    "method": "POST",
                    "header": [
                        {
                            "key": "Content-Type",
                            "value": "application/json"
                        }
                    ],
                    "body": {
                        "mode": "raw",
                        "raw": create_body_raw
                    },
                    "url": {
                        "raw": format!("{{{{base_url}}}}/{}", route_prefix)
                    }
                }
            },
            {
                "name": format!("Update {}", struct_name),
                "request": {
                    "method": "PUT",
                    "header": [
                        {
                            "key": "Content-Type",
                            "value": "application/json"
                        }
                    ],
                    "body": {
                        "mode": "raw",
                        "raw": update_body_raw
                    },
                    "url": {
                        "raw": format!("{{{{base_url}}}}/{}/{{{{{}_id}}}}", route_prefix, singular)
                    }
                }
            },
            {
                "name": format!("Delete {}", struct_name),
                "request": {
                    "method": "DELETE",
                    "url": {
                        "raw": format!("{{{{base_url}}}}/{}/{{{{{}_id}}}}", route_prefix, singular)
                    }
                }
            }
        ]);

        // Generate separate individual collection file (1 file per table)
        let single_collection = json!({
            "info": {
                "_postman_id": uuid::Uuid::new_v4().to_string(),
                "name": format!("{} API", Self::pascal_case(&plural)),
                "description": format!("API Collection for {} generated by Padi Rust Api Framework.", plural),
                "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
            },
            "auth": {
                "type": "bearer",
                "bearer": [
                    {
                        "key": "token",
                        "value": "{{auth_token}}",
                        "type": "string"
                    }
                ]
            },
            "variable": [
                { "key": "base_url", "value": "http://localhost:8085" },
                { "key": "auth_token", "value": "" },
                { "key": id_var_key, "value": "1" }
            ],
            "item": items
        });

        let single_formatted = serde_json::to_string_pretty(&single_collection).unwrap_or_default();
        Self::write_file(&single_path, single_formatted)?;
        println!("\x1b[32m✓ Single Table Collection saved:\x1b[0m {:?}", single_path);

        Ok(())
    }

    /// Generate or update API Collection for ALL tables discovered in the database.
    pub async fn generate_api_collection_all(&self, force: bool) -> Result<(), ApiError> {
        let legacy_path = self.base_dir.join("api_collection.json");
        if legacy_path.exists() {
            let _ = fs::remove_file(&legacy_path);
        }
        let legacy_coll = self.base_dir.join("api_collection/api_collection.json");
        if legacy_coll.exists() {
            let _ = fs::remove_file(&legacy_coll);
        }

        // Generate individual auth_collection.json and users_collection.json
        Self::generate_default_auth_collection(&self.base_dir)?;
        Self::generate_default_users_collection(&self.base_dir)?;

        let tables = self.get_all_tables().await;
        let mut count = 0;
        for table in &tables {
            if Self::is_protected_table(table) && !force {
                continue;
            }
            if self.generate_api_collection(table, force).await.is_ok() {
                count += 1;
            }
        }

        if count > 0 {
            println!("\x1b[32m✓ Individual API Collections generated for {} table(s)!\x1b[0m", count);
        }
        Ok(())
    }

    pub fn generate_default_auth_collection(base_dir: &Path) -> Result<(), ApiError> {
        let path = base_dir.join("api_collection/auth_collection.json");
        let default_all = Self::default_api_collection_json();
        let items = default_all
            .get("item")
            .and_then(|arr| arr.as_array())
            .and_then(|arr| arr.first())
            .and_then(|obj| obj.get("item"))
            .cloned()
            .unwrap_or_else(|| json!([]));

        let col = json!({
            "info": {
                "_postman_id": uuid::Uuid::new_v4().to_string(),
                "name": "Auth API",
                "description": "Authentication collection generated by Padi Rust Api Framework.",
                "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
            },
            "variable": [
                { "key": "base_url", "value": "http://localhost:8085" },
                { "key": "auth_token", "value": "" },
                { "key": "user_id", "value": "1" }
            ],
            "item": items
        });
        let _ = Self::write_file(&path, serde_json::to_string_pretty(&col).unwrap_or_default());
        println!("\x1b[32m✓ Single Table Collection saved:\x1b[0m {:?}", path);
        Ok(())
    }

    pub fn generate_default_users_collection(base_dir: &Path) -> Result<(), ApiError> {
        let path = base_dir.join("api_collection/users_collection.json");
        let default_all = Self::default_api_collection_json();
        let items = default_all
            .get("item")
            .and_then(|arr| arr.as_array())
            .and_then(|arr| arr.get(1))
            .and_then(|obj| obj.get("item"))
            .cloned()
            .unwrap_or_else(|| json!([]));

        let col = json!({
            "info": {
                "_postman_id": uuid::Uuid::new_v4().to_string(),
                "name": "Users API",
                "description": "Users CRUD collection generated by Padi Rust Api Framework.",
                "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
            },
            "auth": {
                "type": "bearer",
                "bearer": [
                    { "key": "token", "value": "{{auth_token}}", "type": "string" }
                ]
            },
            "variable": [
                { "key": "base_url", "value": "http://localhost:8085" },
                { "key": "auth_token", "value": "" },
                { "key": "user_id", "value": "1" }
            ],
            "item": items
        });
        let _ = Self::write_file(&path, serde_json::to_string_pretty(&col).unwrap_or_default());
        println!("\x1b[32m✓ Single Table Collection saved:\x1b[0m {:?}", path);
        Ok(())
    }

    pub fn next_item_index(root: &Value) -> usize {
        root.get("item")
            .and_then(|i| i.as_array())
            .map(|arr| arr.len() + 1)
            .unwrap_or(1)
    }

    /// Create a standard base Postman Collection JSON structure.
    pub fn default_api_collection_json() -> Value {
        json!({
            "info": {
                "_postman_id": uuid::Uuid::new_v4().to_string(),
                "name": "Padi REST API Collection",
                "description": "Complete collection generated by Padi Rust Api Framework.",
                "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"
            },
            "auth": {
                "type": "bearer",
                "bearer": [
                    {
                        "key": "token",
                        "value": "{{auth_token}}",
                        "type": "string"
                    }
                ]
            },
            "variable": [
                { "key": "base_url", "value": "http://localhost:8085" },
                { "key": "auth_token", "value": "" },
                { "key": "user_id", "value": "1" }
            ],
            "item": [
                {
                    "name": "1. Authentication",
                    "item": [
                        {
                            "name": "Register User",
                            "event": [
                                {
                                    "listen": "test",
                                    "script": {
                                        "exec": [
                                            "try {",
                                            "    if (pm.response.code === 201 || pm.response.code === 200) {",
                                            "        const res = pm.response.json();",
                                            "        const data = res.data || {};",
                                            "        if (data.token) pm.collectionVariables.set('auth_token', data.token);",
                                            "        const userId = data.user_id || (data.user ? data.user.id : null) || data.id;",
                                            "        if (userId) pm.collectionVariables.set('user_id', userId);",
                                            "    }",
                                            "} catch (e) { console.warn('Script bypassed'); }"
                                        ],
                                        "type": "text/javascript"
                                    }
                                }
                            ],
                            "request": {
                                "method": "POST",
                                "header": [
                                    { "key": "Content-Type", "value": "application/json" }
                                ],
                                "body": {
                                    "mode": "raw",
                                    "raw": "{\n    \"name\": \"Test User\",\n    \"email\": \"test@example.com\",\n    \"password\": \"Test123!@#\",\n    \"password_confirmation\": \"Test123!@#\"\n}"
                                },
                                "url": { "raw": "{{base_url}}/auth/register" }
                            }
                        },
                        {
                            "name": "Login",
                            "event": [
                                {
                                    "listen": "test",
                                    "script": {
                                        "exec": [
                                            "try {",
                                            "    if (pm.response.code === 200) {",
                                            "        const res = pm.response.json();",
                                            "        const data = res.data || {};",
                                            "        if (data.token) pm.collectionVariables.set('auth_token', data.token);",
                                            "        const userId = data.user_id || (data.user ? data.user.id : null) || data.id;",
                                            "        if (userId) pm.collectionVariables.set('user_id', userId);",
                                            "    }",
                                            "} catch (e) { console.warn('Script bypassed'); }"
                                        ],
                                        "type": "text/javascript"
                                    }
                                }
                            ],
                            "request": {
                                "method": "POST",
                                "header": [
                                    { "key": "Content-Type", "value": "application/json" }
                                ],
                                "body": {
                                    "mode": "raw",
                                    "raw": "{\n    \"email\": \"test@example.com\",\n    \"password\": \"Test123!@#\"\n}"
                                },
                                "url": { "raw": "{{base_url}}/auth/login" }
                            }
                        },
                        {
                            "name": "Get Me (Profile)",
                            "request": {
                                "method": "GET",
                                "url": { "raw": "{{base_url}}/auth/me" }
                            }
                        }
                    ]
                },
                {
                    "name": "2. Users",
                    "item": [
                        {
                            "name": "List Users",
                            "request": {
                                "method": "GET",
                                "url": { "raw": "{{base_url}}/users" }
                            }
                        },
                        {
                            "name": "List All Users",
                            "request": {
                                "method": "GET",
                                "url": { "raw": "{{base_url}}/users/all" }
                            }
                        },
                        {
                            "name": "Get User By ID",
                            "request": {
                                "method": "GET",
                                "url": { "raw": "{{base_url}}/users/{{user_id}}" }
                            }
                        },
                        {
                            "name": "Create User",
                            "event": [
                                {
                                    "listen": "test",
                                    "script": {
                                        "exec": [
                                            "try {",
                                            "    if (pm.response.code === 201 || pm.response.code === 200) {",
                                            "        const res = pm.response.json();",
                                            "        const id = res.data?.id || res.data?.user?.id;",
                                            "        if (id) pm.collectionVariables.set('user_id', id);",
                                            "    }",
                                            "} catch (e) { console.warn('Script bypassed'); }"
                                        ],
                                        "type": "text/javascript"
                                    }
                                }
                            ],
                            "request": {
                                "method": "POST",
                                "header": [
                                    { "key": "Content-Type", "value": "application/json" }
                                ],
                                "body": {
                                    "mode": "raw",
                                    "raw": "{\n    \"name\": \"New User\",\n    \"email\": \"newuser@example.com\",\n    \"password\": \"Password123!\",\n    \"role\": \"user\"\n}"
                                },
                                "url": { "raw": "{{base_url}}/users" }
                            }
                        },
                        {
                            "name": "Update User",
                            "request": {
                                "method": "PUT",
                                "header": [
                                    { "key": "Content-Type", "value": "application/json" }
                                ],
                                "body": {
                                    "mode": "raw",
                                    "raw": "{\n    \"name\": \"Updated Name\"\n}"
                                },
                                "url": { "raw": "{{base_url}}/users/{{user_id}}" }
                            }
                        },
                        {
                            "name": "Delete User",
                            "request": {
                                "method": "DELETE",
                                "url": { "raw": "{{base_url}}/users/{{user_id}}" }
                            }
                        }
                    ]
                }
            ]
        })
    }

    /// Ensure a `mod.rs` file declares `pub mod <module_name>;` and optionally `pub use <module_name>::<item>;`.
    fn ensure_mod_rs(
        mod_path: &Path,
        module_name: &str,
        reexport_item: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut content = if mod_path.exists() {
            fs::read_to_string(mod_path).unwrap_or_default()
        } else {
            let mut init = String::new();
            if mod_path.to_string_lossy().contains("base") {
                init.push_str("#![allow(unused_imports)]\n\n");
            }
            init
        };

        let pub_mod = format!("pub mod {};", module_name);
        let mut modified = false;

        if !content.contains(&pub_mod) {
            content.push_str(&pub_mod);
            content.push('\n');
            modified = true;
        }

        if let Some(item) = reexport_item {
            let pub_use = format!("pub use {}::{};", module_name, item);
            if !content.contains(&pub_use) {
                content.push_str(&pub_use);
                content.push('\n');
                modified = true;
            }
        }

        if modified {
            Self::write_file(mod_path, content)?;
        }

        Ok(())
    }

    fn update_models_mod_rs(
        base_dir: &Path,
        module_name: &str,
        const_name: &str,
    ) -> Result<(), ApiError> {
        // 1. Ensure `pub mod base;` is declared in src/models/mod.rs
        Self::ensure_mod_rs(&base_dir.join("src/models/mod.rs"), "base", None)?;
        // 2. Ensure `pub mod <singular>_base;` and re-export in src/models/base/mod.rs
        Self::ensure_mod_rs(
            &base_dir.join("src/models/base/mod.rs"),
            &format!("{}_base", module_name),
            Some(&format!("BASE_{}", const_name)),
        )?;
        // 3. Ensure `pub mod <singular>;` and re-export in src/models/mod.rs
        Self::ensure_mod_rs(&base_dir.join("src/models/mod.rs"), module_name, Some(const_name))
    }

    fn update_resources_mod_rs(
        base_dir: &Path,
        module_name: &str,
        struct_name: &str,
    ) -> Result<(), ApiError> {
        Self::ensure_mod_rs(&base_dir.join("src/resources/mod.rs"), module_name, Some(struct_name))
    }

    fn update_controllers_mod_rs(base_dir: &Path, module_name: &str) -> Result<(), ApiError> {
        // 1. Ensure `pub mod base;` is declared in src/controllers/mod.rs
        Self::ensure_mod_rs(&base_dir.join("src/controllers/mod.rs"), "base", None)?;
        // 2. Ensure `pub mod <plural>_base;` in src/controllers/base/mod.rs
        Self::ensure_mod_rs(
            &base_dir.join("src/controllers/base/mod.rs"),
            &format!("{}_base", module_name),
            None,
        )?;
        // 3. Ensure `pub mod <plural>;` in src/controllers/mod.rs
        Self::ensure_mod_rs(&base_dir.join("src/controllers/mod.rs"), module_name, None)
    }

    fn update_routes_mod_rs(
        base_dir: &Path,
        module_name: &str,
        route_prefix: &str,
    ) -> Result<(), ApiError> {
        let mod_path = base_dir.join("src/routes/mod.rs");
        let mut content = if mod_path.exists() {
            fs::read_to_string(&mod_path).unwrap_or_default()
        } else {
            String::new()
        };

        let mod_decl = format!("mod {};", module_name);
        let nest_call = format!(".nest(\"/{}\", {}::routes())", route_prefix, module_name);

        // 1. Add module declaration if not present
        if !content.contains(&mod_decl) {
            if let Some(last_mod_idx) = content.rfind("mod ") {
                if let Some(newline_idx) = content[last_mod_idx..].find('\n') {
                    let insert_pos = last_mod_idx + newline_idx + 1;
                    content.insert_str(insert_pos, &format!("{}\n", mod_decl));
                } else {
                    content.push_str(&format!("{}\n", mod_decl));
                }
            } else {
                content.push_str(&format!("{}\n", mod_decl));
            }
        }

        // 2. Add .nest(...) in api() if not present
        if !content.contains(&nest_call) {
            if let Some(api_idx) = content.find("pub fn api()") {
                if let Some(close_brace_idx) = content[api_idx..].rfind('}') {
                    let insert_pos = api_idx + close_brace_idx;
                    let to_insert = format!("        {}\n", nest_call);
                    content.insert_str(insert_pos, &to_insert);
                }
            }
        }

        Self::write_file(&mod_path, content)?;
        Ok(())
    }

    pub fn snake_case(s: &str) -> String {
        let mut res = String::new();
        for (i, c) in s.chars().enumerate() {
            if c.is_uppercase() {
                if i > 0 {
                    res.push('_');
                }
                res.push(c.to_ascii_lowercase());
            } else if c == '-' || c == ' ' {
                res.push('_');
            } else {
                res.push(c);
            }
        }
        res
    }

    pub fn screaming_snake_case(s: &str) -> String {
        Self::snake_case(s).to_ascii_uppercase()
    }

    pub fn pascal_case(s: &str) -> String {
        let mut res = String::new();
        let mut capitalize_next = true;
        for c in s.chars() {
            if c == '_' || c == '-' || c == ' ' {
                capitalize_next = true;
            } else if capitalize_next {
                res.push(c.to_ascii_uppercase());
                capitalize_next = false;
            } else {
                res.push(c);
            }
        }
        res
    }

    pub fn kebab_case(s: &str) -> String {
        Self::snake_case(s).replace('_', "-")
    }

    pub fn pluralize(s: &str) -> String {
        if let Some(stem) = s.strip_suffix('y') {
            if !stem.ends_with(['a', 'e', 'o', 'u']) {
                return format!("{stem}ies");
            }
        }

        if s.ends_with(['s', 'x', 'z']) || s.ends_with("ch") || s.ends_with("sh") {
            format!("{s}es")
        } else {
            format!("{s}s")
        }
    }

    pub fn singularize(s: &str) -> String {
        if let Some(prefix) = s.strip_suffix("ies") {
            format!("{prefix}y")
        } else if s.ends_with("shes") || s.ends_with("ches") || s.ends_with("sses") {
            s.strip_suffix("es").unwrap_or(s).to_string()
        } else if s.ends_with('s') && !s.ends_with("ss") {
            s.strip_suffix('s').unwrap_or(s).to_string()
        } else {
            s.to_string()
        }
    }

    pub fn infer_table_name_from_migration(name: &str) -> String {
        let snake = Self::snake_case(name);
        if let Some(rest) = snake.strip_prefix("create_") {
            if let Some(table) = rest.strip_suffix("_table") {
                return table.to_string();
            }
        }
        snake
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_protected_table() {
        assert!(Generator::is_protected_table("users"));
        assert!(Generator::is_protected_table("Users"));
        assert!(Generator::is_protected_table("MIGRATIONS"));
        assert!(Generator::is_protected_table("jobs"));
        assert!(!Generator::is_protected_table("products"));
        assert!(!Generator::is_protected_table("order_items"));
    }

    #[test]
    fn test_snake_case() {
        assert_eq!(Generator::snake_case("Product"), "product");
        assert_eq!(Generator::snake_case("OrderItem"), "order_item");
        assert_eq!(Generator::snake_case("order-item"), "order_item");
        assert_eq!(Generator::snake_case("order item"), "order_item");
    }

    #[test]
    fn test_screaming_snake_case() {
        assert_eq!(Generator::screaming_snake_case("Product"), "PRODUCT");
        assert_eq!(Generator::screaming_snake_case("order_items"), "ORDER_ITEMS");
    }

    #[test]
    fn test_pascal_case() {
        assert_eq!(Generator::pascal_case("product"), "Product");
        assert_eq!(Generator::pascal_case("order_item"), "OrderItem");
        assert_eq!(Generator::pascal_case("order-item"), "OrderItem");
    }

    #[test]
    fn test_kebab_case() {
        assert_eq!(Generator::kebab_case("OrderItem"), "order-item");
        assert_eq!(Generator::kebab_case("order_items"), "order-items");
    }

    #[test]
    fn test_pluralize() {
        assert_eq!(Generator::pluralize("product"), "products");
        assert_eq!(Generator::pluralize("category"), "categories");
        assert_eq!(Generator::pluralize("day"), "days");
        assert_eq!(Generator::pluralize("box"), "boxes");
        assert_eq!(Generator::pluralize("watch"), "watches");
    }

    #[test]
    fn test_singularize() {
        assert_eq!(Generator::singularize("products"), "product");
        assert_eq!(Generator::singularize("categories"), "category");
        assert_eq!(Generator::singularize("wishes"), "wish");
        assert_eq!(Generator::singularize("watches"), "watch");
        assert_eq!(Generator::singularize("bosses"), "boss");
    }

    #[test]
    fn test_infer_table_name_from_migration() {
        assert_eq!(
            Generator::infer_table_name_from_migration("create_products_table"),
            "products"
        );
        assert_eq!(
            Generator::infer_table_name_from_migration("add_index_to_users"),
            "add_index_to_users"
        );
    }

    #[test]
    fn test_normalize_names() {
        let (tbl, sing, struc, cnst, plur) = Generator::normalize_names("OrderItem");
        assert_eq!(tbl, "order_items");
        assert_eq!(sing, "order_item");
        assert_eq!(struc, "OrderItem");
        assert_eq!(cnst, "ORDER_ITEMS");
        assert_eq!(plur, "order_items");
    }
}

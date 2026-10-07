//! Code Generator (port of Generator.php)
//!
//! Generates Model, Controller, Resource, Routes, and SQL Migrations.
//! Supports:
//! - Protected tables guard (`users`, `migrations`, `password_resets`)
//! - Database table column introspection (SQLite PRAGMA, MySQL INFORMATION_SCHEMA, PostgreSQL columns)
//! - Automatic registration into `mod.rs` files
use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::Local;

use crate::{
    database::{Database, Val},
    error::ApiError,
};

pub struct Generator {
    pub base_dir: PathBuf,
}

impl Generator {
    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    pub fn is_protected_table(table_name: &str) -> bool {
        let protected = ["users", "password_resets", "migrations"];
        protected.contains(&table_name.to_lowercase().as_str())
    }

    /// Introspect table columns from database (SQLite / MySQL / Postgres).
    pub async fn get_table_columns(&self, table_name: &str) -> Vec<String> {
        let mut columns = Vec::new();

        if Database::is_sqlite() {
            let sql = format!("PRAGMA table_info({})", table_name);
            if let Ok(rows) = Database::fetch_all(&sql, &[]).await {
                for r in rows {
                    if let Some(col) = r["name"].as_str() {
                        columns.push(col.to_string());
                    }
                }
            }
        } else if Database::is_postgres() {
            let sql = "SELECT column_name FROM information_schema.columns WHERE table_name = ? ORDER BY ordinal_position";
            if let Ok(rows) = Database::fetch_all(sql, &[Val::from(table_name)]).await {
                for r in rows {
                    if let Some(col) = r["column_name"].as_str() {
                        columns.push(col.to_string());
                    }
                }
            }
        } else {
            // MySQL / MariaDB
            let sql = "SELECT column_name FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = ? ORDER BY ordinal_position";
            if let Ok(rows) = Database::fetch_all(sql, &[Val::from(table_name)]).await {
                for r in rows {
                    if let Some(col) = r["column_name"].as_str() {
                        columns.push(col.to_string());
                    }
                }
            }
        }

        columns
    }

    /// Generate Controller
    pub fn generate_controller(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let singular = Self::singularize(name);
        let file_stem = Self::snake_case(name);
        let plural_snake = Self::snake_case(name);

        if Self::is_protected_table(&file_stem) && !force {
            eprintln!("\x1b[33m⚠️ Table '{}' is a protected core table. Skipping controller generation.\x1b[0m", file_stem);
            return Ok(());
        }

        let path = self.base_dir.join("src/controllers").join(format!("{}.rs", file_stem));
        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Controller already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        let const_name = Self::screaming_snake_case(name);
        let struct_name = Self::pascal_case(&singular);

        let content = format!(
            r#"use padi_core::prelude::*;

use crate::{{models::{const_name}, resources::{struct_name}Resource}};

pub async fn index(_auth: AuthUser, p: Params) -> ApiResult {{
    let (page, per) = p.pagination();
    ok({struct_name}Resource::wraps(&{const_name}.paginate(page, per).await?))
}}

pub async fn all(_auth: AuthUser) -> ApiResult {{
    ok({struct_name}Resource::wraps(&json!({const_name}.all().await?)))
}}

pub async fn show(_auth: AuthUser, Path(id): Path<i64>) -> ApiResult {{
    ok({struct_name}Resource::wrap(&{const_name}.find_or_fail(id).await?))
}}

pub async fn store(_auth: AuthUser, Body(body): Body) -> ApiResult {{
    let item = {const_name}.create(&body).await?;
    created({struct_name}Resource::wrap(&item))
}}

pub async fn update(_auth: AuthUser, Path(id): Path<i64>, Body(body): Body) -> ApiResult {{
    let item = {const_name}.update(id, &body).await?;
    ok({struct_name}Resource::wrap(&item))
}}

pub async fn destroy(_auth: AuthUser, Path(id): Path<i64>) -> ApiResult {{
    {const_name}.delete(id).await?;
    no_content()
}}
"#
        );

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }

        fs::write(&path, content).map_err(|e| ApiError::internal(e.to_string()))?;
        Self::append_mod_rs(&self.base_dir.join("src/controllers/mod.rs"), &plural_snake)?;
        println!("\x1b[32m✓ Controller created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate Model with column introspection
    pub async fn generate_model(&self, table_name: &str, force: bool) -> Result<(), ApiError> {
        if Self::is_protected_table(table_name) && !force {
            eprintln!("\x1b[33m⚠️ Table '{}' is a protected core table. Skipping model generation.\x1b[0m", table_name);
            return Ok(());
        }

        let singular = Self::singularize(table_name);
        let file_stem = Self::snake_case(&singular);
        let const_name = Self::screaming_snake_case(table_name);

        let path = self.base_dir.join("src/models").join(format!("{}.rs", file_stem));
        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Model already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        // Introspect fillable columns (exclude id, created_at, updated_at)
        let cols = self.get_table_columns(table_name).await;
        let fillable: Vec<String> = cols
            .into_iter()
            .filter(|c| c != "id" && c != "created_at" && c != "updated_at")
            .collect();

        let content = if fillable.is_empty() {
            format!(
                r#"use padi_core::prelude::*;

pub const {const_name}: Model = Model::new("{table_name}");
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

pub const {const_name}: Model = Model::new("{table_name}")
    .fillable(&[{fillable_repr}]);
"#
            )
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }

        fs::write(&path, content).map_err(|e| ApiError::internal(e.to_string()))?;
        Self::append_mod_rs(&self.base_dir.join("src/models/mod.rs"), &file_stem)?;
        println!("\x1b[32m✓ Model created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate Resource
    pub async fn generate_resource(&self, table_name: &str, force: bool) -> Result<(), ApiError> {
        if Self::is_protected_table(table_name) && !force {
            eprintln!("\x1b[33m⚠️ Table '{}' is a protected core table. Skipping resource generation.\x1b[0m", table_name);
            return Ok(());
        }

        let singular = Self::singularize(table_name);
        let file_stem = Self::snake_case(&singular);
        let struct_name = Self::pascal_case(&singular);

        let path = self.base_dir.join("src/resources").join(format!("{}.rs", file_stem));
        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Resource already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        let cols = self.get_table_columns(table_name).await;
        let content = if cols.is_empty() {
            format!(
                r#"use padi_core::prelude::*;

pub struct {struct_name}Resource;

impl Resource for {struct_name}Resource {{
    fn to_array(item: &Value) -> Value {{
        item.clone()
    }}
}}
"#
            )
        } else {
            let shape_entries = cols
                .iter()
                .map(|c| format!("            \"{}\": item[\"{}\"],", c, c))
                .collect::<Vec<_>>()
                .join("\n");

            format!(
                r#"use padi_core::prelude::*;

pub struct {struct_name}Resource;

impl Resource for {struct_name}Resource {{
    fn to_array(item: &Value) -> Value {{
        json!({{
{shape_entries}
        }})
    }}
}}
"#
            )
        };

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }

        fs::write(&path, content).map_err(|e| ApiError::internal(e.to_string()))?;
        Self::append_mod_rs(&self.base_dir.join("src/resources/mod.rs"), &file_stem)?;
        println!("\x1b[32m✓ Resource created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate Route
    pub fn generate_route(&self, table_name: &str, force: bool) -> Result<(), ApiError> {
        let plural_snake = Self::snake_case(table_name);
        let path = self.base_dir.join("src/routes").join(format!("{}.rs", plural_snake));

        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Route already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        let content = format!(
            r#"use padi_core::prelude::*;

use crate::controllers::{plural_snake};

pub fn routes() -> Router {{
    Router::new()
        .route("/", get({plural_snake}::index).post({plural_snake}::store))
        .route("/all", get({plural_snake}::all))
        .route("/{{id}}", get({plural_snake}::show).put({plural_snake}::update).delete({plural_snake}::destroy))
}}
"#
        );

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }

        fs::write(&path, content).map_err(|e| ApiError::internal(e.to_string()))?;
        Self::append_mod_rs(&self.base_dir.join("src/routes/mod.rs"), &plural_snake)?;
        println!("\x1b[32m✓ Route created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate SQL Migration
    pub fn generate_migration(&self, name: &str) -> Result<(), ApiError> {
        let timestamp = Local::now().format("%Y_%m_%d_%H%M%S").to_string();
        let file_name = format!("{}_{}.sql", timestamp, Self::snake_case(name));
        let dir = self.base_dir.join("database/migrations");
        let path = dir.join(file_name);

        fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;

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
            table = Self::snake_case(name)
        );

        fs::write(&path, template).map_err(|e| ApiError::internal(e.to_string()))?;
        println!("\x1b[32m✓ Migration created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate complete CRUD for table
    pub async fn generate_crud(&self, table_name: &str, force: bool) -> Result<(), ApiError> {
        if Self::is_protected_table(table_name) && !force {
            eprintln!("\x1b[33m⚠️ Table '{}' is a protected core table. Skipping CRUD generation.\x1b[0m", table_name);
            return Ok(());
        }

        self.generate_model(table_name, force).await?;
        self.generate_resource(table_name, force).await?;
        self.generate_controller(table_name, force)?;
        self.generate_route(table_name, force)?;

        println!("\x1b[32m✓ CRUD generated successfully for table: {}\x1b[0m", table_name);
        Ok(())
    }

    fn append_mod_rs(mod_path: &Path, module_name: &str) -> Result<(), ApiError> {
        let mut content = if mod_path.exists() {
            fs::read_to_string(mod_path).unwrap_or_default()
        } else {
            String::new()
        };

        let pub_mod = format!("pub mod {};\n", module_name);
        if !content.contains(&pub_mod) {
            content.push_str(&pub_mod);
            fs::write(mod_path, content).map_err(|e| ApiError::internal(e.to_string()))?;
        }
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

    pub fn singularize(s: &str) -> String {
        if let Some(prefix) = s.strip_suffix("ies") {
            format!("{}y", prefix)
        } else if s.ends_with("es") && (s.ends_with("shes") || s.ends_with("ches") || s.ends_with("sses")) {
            s.strip_suffix("es").unwrap_or(s).to_string()
        } else if s.ends_with('s') && !s.ends_with("ss") {
            s.strip_suffix('s').unwrap_or(s).to_string()
        } else {
            s.to_string()
        }
    }
}

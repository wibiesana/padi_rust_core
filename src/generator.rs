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

    /// Generate Model with column introspection and automatic `mod.rs` re-export.
    pub async fn generate_model(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, singular, _struct_name, const_name, _plural) = Self::normalize_names(name);

        if Self::is_protected_table(&table_name) && !force {
            eprintln!(
                "\x1b[33m⚠️ Table '{}' is a protected core table. Skipping model generation.\x1b[0m",
                table_name
            );
            return Ok(());
        }

        let file_stem = &singular;
        let path = self
            .base_dir
            .join("src/models")
            .join(format!("{}.rs", file_stem));

        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Model already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        // Introspect fillable columns (exclude id, created_at, updated_at, created_by, updated_by)
        let cols = self.get_table_columns(&table_name).await;
        let fillable: Vec<String> = cols
            .into_iter()
            .filter(|c| {
                !matches!(
                    c.as_str(),
                    "id" | "created_at" | "updated_at" | "created_by" | "updated_by"
                )
            })
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

        Self::write_file(&path, content)?;
        Self::update_models_mod_rs(&self.base_dir, file_stem, &const_name)?;
        println!("\x1b[32m✓ Model created:\x1b[0m {:?}", path);

        Ok(())
    }

    /// Generate API Resource with automatic `mod.rs` re-export.
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

        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Resource already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

        let cols = self.get_table_columns(&table_name).await;
        let content = if cols.is_empty() {
            format!(
                r#"use padi_core::prelude::*;

pub struct {resource_struct};

impl Resource for {resource_struct} {{
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

    /// Generate Controller with standard CRUD operations and automatic `mod.rs` registration.
    pub fn generate_controller(&self, name: &str, force: bool) -> Result<(), ApiError> {
        let (table_name, _singular, struct_name, const_name, plural) = Self::normalize_names(name);

        if Self::is_protected_table(&table_name) && !force {
            eprintln!(
                "\x1b[33m⚠️ Table '{}' is a protected core table. Skipping controller generation.\x1b[0m",
                table_name
            );
            return Ok(());
        }

        let file_stem = &plural;
        let path = self
            .base_dir
            .join("src/controllers")
            .join(format!("{}.rs", file_stem));

        if path.exists() && !force {
            eprintln!("\x1b[33mWarning: Controller already exists at {:?}\x1b[0m", path);
            return Ok(());
        }

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

        Self::write_file(&path, content)?;
        Self::update_controllers_mod_rs(&self.base_dir, file_stem)?;
        println!("\x1b[32m✓ Controller created:\x1b[0m {:?}", path);

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
        self.generate_controller(name, force)?;

        println!("\n4. Generating Route...");
        self.generate_route(name, force)?;

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

    /// Ensure a `mod.rs` file declares `pub mod <module_name>;` and optionally `pub use <module_name>::<item>;`.
    fn ensure_mod_rs(
        mod_path: &Path,
        module_name: &str,
        reexport_item: Option<&str>,
    ) -> Result<(), ApiError> {
        let mut content = if mod_path.exists() {
            fs::read_to_string(mod_path).unwrap_or_default()
        } else {
            String::new()
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

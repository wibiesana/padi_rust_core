//! Migrator - database migration runner (Rust port of Migrator.php).
//!
//! Supports SQLite, MySQL/MariaDB, and PostgreSQL.
//! Supports plain `.sql` migrations with optional rollback section:
//! ```sql
//! CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
//! -- +down
//! DROP TABLE users;
//! ```
//!
//! Features:
//! - Multi-database schema support (`sqlite`, `pgsql`, `mysql`)
//! - Optional table filtering (`run_filtered`)
//! - Step-based rollback (`rollback_steps` / `rollback`)
//! - Migration status inspection and pretty printing (`status` / `print_status`)

use std::{fs, path::Path};

use serde::Serialize;

use crate::{
    db::{Database, Val},
    error::ApiError,
};

/// Represents the execution status of a single migration.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationItem {
    pub name: String,
    pub executed: bool,
    pub batch: Option<i64>,
}

pub struct Migrator;

fn split_file(content: &str) -> (String, String) {
    match content.split_once("-- +down") {
        Some((up, down)) => (up.to_string(), down.to_string()),
        None => (content.to_string(), String::new()),
    }
}

async fn run_sql(script: &str) -> Result<(), ApiError> {
    for stmt in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        Database::execute(stmt, &[]).await?;
    }
    Ok(())
}

impl Migrator {
    /// Ensure the migrations tracking table exists for the active database engine.
    pub async fn ensure_table() -> Result<(), ApiError> {
        let sql = if Database::is_sqlite() {
            "CREATE TABLE IF NOT EXISTS migrations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                migration VARCHAR(255) UNIQUE NOT NULL,
                batch INTEGER NOT NULL,
                executed_at INTEGER DEFAULT (strftime('%s', 'now'))
            )"
        } else if Database::is_postgres() {
            "CREATE TABLE IF NOT EXISTS migrations (
                id SERIAL PRIMARY KEY,
                migration VARCHAR(255) UNIQUE NOT NULL,
                batch INTEGER NOT NULL,
                executed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )"
        } else {
            "CREATE TABLE IF NOT EXISTS migrations (
                id INT AUTO_INCREMENT PRIMARY KEY,
                migration VARCHAR(255) UNIQUE NOT NULL,
                batch INT NOT NULL,
                executed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            )"
        };

        if let Err(e) = Database::execute(sql, &[]).await {
            // Fallback for older existing schema if already present
            let fallback_sql = "CREATE TABLE IF NOT EXISTS migrations (migration VARCHAR(255) PRIMARY KEY, batch INTEGER NOT NULL)";
            let _ = Database::execute(fallback_sql, &[]).await;
            if Database::execute("SELECT migration, batch FROM migrations LIMIT 1", &[]).await.is_err() {
                return Err(e);
            }
        }

        Ok(())
    }

    /// Read and sort all `.sql` migration files in directory.
    pub fn files(dir: &str) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = fs::read_dir(Path::new(dir))
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().extension().map(|x| x == "sql").unwrap_or(false))
                    .filter_map(|e| {
                        let name = e.path().file_stem()?.to_string_lossy().to_string();
                        Some((name, fs::read_to_string(e.path()).ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    /// Get list of already executed migration names from database.
    pub async fn get_executed() -> Result<Vec<String>, ApiError> {
        Self::ensure_table().await?;
        let rows = Database::fetch_all("SELECT migration FROM migrations", &[]).await?;
        Ok(rows
            .iter()
            .filter_map(|r| r["migration"].as_str().map(String::from))
            .collect())
    }

    /// Get next batch number for running migrations.
    pub async fn get_next_batch() -> Result<i64, ApiError> {
        Self::ensure_table().await?;
        let batch = Database::fetch_one("SELECT MAX(batch) AS b FROM migrations", &[])
            .await?
            .and_then(|r| r["b"].as_i64())
            .unwrap_or(0);
        Ok(batch + 1)
    }

    /// Run all pending migrations. Returns list of newly applied migration names.
    pub async fn run(dir: &str) -> Result<Vec<String>, ApiError> {
        Self::run_filtered(dir, None).await
    }

    /// Run pending migrations with optional table filtering (matching `Migrator.php`).
    ///
    /// If `table_filter` is specified (e.g. `Some(&["users", "posts"])`),
    /// only migration files whose names contain any of the filter tokens are executed.
    pub async fn run_filtered(
        dir: &str,
        table_filter: Option<&[&str]>,
    ) -> Result<Vec<String>, ApiError> {
        Self::ensure_table().await?;
        let executed = Self::get_executed().await?;
        let all_files = Self::files(dir);

        let mut to_execute = vec![];
        for (name, content) in all_files {
            if executed.contains(&name) {
                continue;
            }

            if let Some(filters) = table_filter {
                let matches_filter = filters.iter().any(|f| {
                    name.to_ascii_lowercase()
                        .contains(&f.to_ascii_lowercase())
                });
                if !matches_filter {
                    continue;
                }
            }

            to_execute.push((name, content));
        }

        if to_execute.is_empty() {
            return Ok(vec![]);
        }

        let batch = Self::get_next_batch().await?;
        let mut applied = vec![];

        for (name, content) in to_execute {
            let (up_sql, _) = split_file(&content);
            run_sql(&up_sql).await?;

            Database::execute(
                "INSERT INTO migrations (migration, batch) VALUES (?, ?)",
                &[Val::from(name.as_str()), Val::Int(batch)],
            )
            .await?;

            applied.push(name);
        }

        Ok(applied)
    }

    /// Roll back the most recent batch of migrations. Returns reverted migration names.
    pub async fn rollback(dir: &str) -> Result<Vec<String>, ApiError> {
        Self::rollback_steps(dir, 1).await
    }

    /// Roll back multiple batches of migrations (`steps`).
    ///
    /// Port of `Migrator::rollback(int $steps = 1)`.
    pub async fn rollback_steps(dir: &str, steps: usize) -> Result<Vec<String>, ApiError> {
        Self::ensure_table().await?;
        let files = Self::files(dir);
        let mut all_reverted = vec![];

        for _ in 0..steps {
            let max_batch = Database::fetch_one("SELECT MAX(batch) AS b FROM migrations", &[])
                .await?
                .and_then(|r| r["b"].as_i64());

            let Some(batch) = max_batch else {
                break;
            };

            let rows = match Database::fetch_all(
                "SELECT migration FROM migrations WHERE batch = ? ORDER BY id DESC, migration DESC",
                &[Val::Int(batch)],
            )
            .await {
                Ok(r) => r,
                Err(_) => {
                    Database::fetch_all(
                        "SELECT migration FROM migrations WHERE batch = ? ORDER BY migration DESC",
                        &[Val::Int(batch)],
                    )
                    .await?
                }
            };

            if rows.is_empty() {
                break;
            }

            for r in rows {
                let name = r["migration"].as_str().unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }

                if let Some((_, content)) = files.iter().find(|(n, _)| *n == name) {
                    let (_, down_sql) = split_file(content);
                    if !down_sql.trim().is_empty() {
                        run_sql(&down_sql).await?;
                    }
                }

                Database::execute(
                    "DELETE FROM migrations WHERE migration = ?",
                    &[Val::from(name.as_str())],
                )
                .await?;

                all_reverted.push(name);
            }
        }

        Ok(all_reverted)
    }

    /// Get current migration status list (file name, executed status, batch).
    pub async fn status(dir: &str) -> Result<Vec<MigrationItem>, ApiError> {
        Self::ensure_table().await?;
        let rows = Database::fetch_all("SELECT migration, batch FROM migrations", &[]).await?;
        let mut executed_map = std::collections::HashMap::new();
        for r in rows {
            if let Some(m) = r["migration"].as_str() {
                let batch = r["batch"].as_i64();
                executed_map.insert(m.to_string(), batch);
            }
        }

        let all_files = Self::files(dir);
        let mut result = vec![];

        for (name, _) in all_files {
            let batch = executed_map.get(&name).copied().flatten();
            let is_done = batch.is_some();
            result.push(MigrationItem {
                name,
                executed: is_done,
                batch,
            });
        }

        Ok(result)
    }

    /// Print formatted migration status table to console (matching PHP `Migrator::status()`).
    pub async fn print_status(dir: &str) -> Result<(), ApiError> {
        let items = Self::status(dir).await?;

        println!("\nMigration Status:");
        println!("{}", "-".repeat(70));
        println!("{:<50} Status", "Migration");
        println!("{}", "-".repeat(70));

        let mut executed_count = 0;
        let total_count = items.len();

        for item in &items {
            let status_text = if item.executed {
                executed_count += 1;
                format!("\x1b[32m✓ Migrated (batch {})\x1b[0m", item.batch.unwrap_or(0))
            } else {
                "\x1b[33m✗ Pending\x1b[0m".to_string()
            };
            println!("{:<50} {}", item.name, status_text);
        }

        println!("{}", "-".repeat(70));
        println!("Total:    {} migrations", total_count);
        println!("Executed: {} migrations", executed_count);
        println!("Pending:  {} migrations\n", total_count - executed_count);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_file() {
        let sql = "CREATE TABLE users (id INT);\n-- +down\nDROP TABLE users;";
        let (up, down) = split_file(sql);
        assert_eq!(up.trim(), "CREATE TABLE users (id INT);");
        assert_eq!(down.trim(), "DROP TABLE users;");

        let no_down = "CREATE TABLE posts (id INT);";
        let (up2, down2) = split_file(no_down);
        assert_eq!(up2.trim(), "CREATE TABLE posts (id INT);");
        assert_eq!(down2, "");
    }
}

//! Migrator - plain SQL migrations (port of Migrator.php).
//!
//! Files live in `database/migrations/*.sql`. Optional rollback section:
//! ```sql
//! CREATE TABLE ...;
//! -- +down
//! DROP TABLE ...;
//! ```
use std::{fs, path::Path};

use crate::{
    db::{Database, Val},
    error::ApiError,
};

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
    async fn ensure_table() -> Result<(), ApiError> {
        Database::execute(
            "CREATE TABLE IF NOT EXISTS migrations (migration VARCHAR(255) PRIMARY KEY, batch INTEGER NOT NULL)",
            &[],
        )
        .await?;
        Ok(())
    }

    fn files(dir: &str) -> Vec<(String, String)> {
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

    /// Run all pending migrations; returns names applied.
    pub async fn run(dir: &str) -> Result<Vec<String>, ApiError> {
        Self::ensure_table().await?;
        let done: Vec<String> = Database::fetch_all("SELECT migration FROM migrations", &[])
            .await?
            .iter()
            .filter_map(|r| r["migration"].as_str().map(String::from))
            .collect();
        let batch = Database::fetch_one("SELECT MAX(batch) AS b FROM migrations", &[])
            .await?
            .and_then(|r| r["b"].as_i64())
            .unwrap_or(0)
            + 1;

        let mut applied = vec![];
        for (name, content) in Self::files(dir) {
            if done.contains(&name) {
                continue;
            }
            run_sql(&split_file(&content).0).await?;
            Database::execute("INSERT INTO migrations (migration, batch) VALUES (?, ?)", &[Val::from(name.as_str()), Val::Int(batch)]).await?;
            applied.push(name);
        }
        Ok(applied)
    }

    /// Roll back the most recent batch; returns names reverted.
    pub async fn rollback(dir: &str) -> Result<Vec<String>, ApiError> {
        Self::ensure_table().await?;
        let batch = Database::fetch_one("SELECT MAX(batch) AS b FROM migrations", &[])
            .await?
            .and_then(|r| r["b"].as_i64());
        let Some(batch) = batch else { return Ok(vec![]) };
        let rows = Database::fetch_all("SELECT migration FROM migrations WHERE batch = ? ORDER BY migration DESC", &[Val::Int(batch)]).await?;
        let files = Self::files(dir);
        let mut reverted = vec![];
        for r in rows {
            let name = r["migration"].as_str().unwrap_or("").to_string();
            if let Some((_, content)) = files.iter().find(|(n, _)| *n == name) {
                run_sql(&split_file(content).1).await?;
            }
            Database::execute("DELETE FROM migrations WHERE migration = ?", &[Val::from(name.as_str())]).await?;
            reverted.push(name);
        }
        Ok(reverted)
    }
}

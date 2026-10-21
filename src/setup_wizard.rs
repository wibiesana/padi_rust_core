//! Setup Wizard for Padi Rust Api Framework.
//!
//! Interactive CLI wizard for first-time project configuration:
//! - Environment setup & `.env` generation
//! - Database driver selection (SQLite, MySQL/MariaDB, PostgreSQL)
//! - Cryptographically secure JWT secret generation (OS CSPRNG)
//! - Database connection testing
//! - Running migrations & CRUD generation prompts

use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use crate::{
    db::Database,
    env,
    error::ApiError,
    generator::Generator,
    migrator::Migrator,
};

pub struct SetupWizard {
    project_root: PathBuf,
}

impl SetupWizard {
    pub fn new<P: AsRef<Path>>(project_root: P) -> Self {
        Self {
            project_root: project_root.as_ref().to_path_buf(),
        }
    }

    /// Colorize text using standard ANSI escape codes.
    pub fn colorize(text: &str, color: &str) -> String {
        let code = match color {
            "red" => "\x1b[31m",
            "green" => "\x1b[32m",
            "yellow" => "\x1b[33m",
            "blue" => "\x1b[34m",
            "cyan" => "\x1b[36m",
            "bold" => "\x1b[1m",
            _ => "\x1b[0m",
        };
        format!("{}{}\x1b[0m", code, text)
    }

    fn info(&self, msg: &str) {
        println!("{} {}", Self::colorize("ℹ", "blue"), msg);
    }

    fn success(&self, msg: &str) {
        println!("{} {}", Self::colorize("✓", "green"), msg);
    }

    fn error(&self, msg: &str) {
        println!("{} {}", Self::colorize("✗", "red"), msg);
    }

    fn warning(&self, msg: &str) {
        println!("{} {}", Self::colorize("⚠", "yellow"), msg);
    }

    /// Read user input from stdin with optional default fallback.
    fn ask(&self, question: &str, default: &str) -> String {
        let mut prompt = Self::colorize(question, "cyan");
        if !default.is_empty() {
            prompt.push_str(&format!(" {}", Self::colorize(&format!("[{}]", default), "yellow")));
        }
        prompt.push_str(": ");
        print!("{}", prompt);
        let _ = io::stdout().flush();

        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                default.to_string()
            } else {
                trimmed.to_string()
            }
        } else {
            default.to_string()
        }
    }

    /// Render a numbered choices menu and prompt the user.
    fn choice(&self, question: &str, options: &[(usize, &str)], default: usize) -> usize {
        println!("\n{}", Self::colorize(question, "cyan"));
        println!("{}", "-".repeat(60));
        for (key, label) in options {
            let marker = if *key == default { "→" } else { " " };
            println!("  {} {}. {}", marker, key, label);
        }
        println!("{}", "-".repeat(60));

        let ans = self.ask("Enter your choice", &default.to_string());
        ans.parse::<usize>().unwrap_or(default)
    }

    fn banner(&self) {
        let banner = r#"
╔════════════════════════════════════════════════════════════════╗
║             Padi Rust Api Framework - Setup Wizard             ║
║                     Rust Edition (axum/sqlx)                   ║
║                     Powered by Padi Console                    ║
╚════════════════════════════════════════════════════════════════╝
"#;
        println!("{}", Self::colorize(banner, "blue"));
    }

    /// Pure helper to update or insert a key-value pair in `.env` formatted content.
    pub fn update_env_content(content: &str, key: &str, value: &str) -> String {
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let target_prefix = format!("{}=", key);
        let mut found = false;

        for line in &mut lines {
            if line.starts_with(&target_prefix) {
                *line = format!("{}={}", key, value);
                found = true;
                break;
            }
        }

        if !found {
            lines.push(format!("{}={}", key, value));
        }

        lines.join("\n") + "\n"
    }

    /// Update or insert a key-value pair in project `.env` file.
    pub fn update_env(&self, key: &str, value: &str) -> bool {
        let env_file = self.project_root.join(".env");
        let content = if env_file.exists() {
            fs::read_to_string(&env_file).unwrap_or_default()
        } else {
            String::new()
        };

        let new_content = Self::update_env_content(&content, key, value);
        fs::write(&env_file, new_content).is_ok()
    }

    /// Construct database connection URL in a clean, DRY manner.
    pub fn build_database_url(
        driver: &str,
        user: &str,
        pass: &str,
        host: &str,
        port: &str,
        name: &str,
    ) -> String {
        let auth = if pass.is_empty() {
            user.to_string()
        } else {
            format!("{}:{}", user, pass)
        };
        format!("{}://{}@{}:{}/{}", driver, auth, host, port, name)
    }

    /// Generate a 256-bit cryptographically secure JWT secret using OS CSPRNG.
    pub fn generate_jwt_secret() -> String {
        let u1 = uuid::Uuid::new_v4();
        let u2 = uuid::Uuid::new_v4();
        format!("{}{}", u1.simple(), u2.simple())
    }

    /// Run the interactive setup wizard.
    pub async fn run(&self) -> Result<(), ApiError> {
        self.banner();

        // Step 1: Check .env and .env.example
        println!("{}", Self::colorize("[1/7] Checking environment file...", "yellow"));
        let env_path = self.project_root.join(".env");
        let example_path = self.project_root.join(".env.example");

        if env_path.exists() {
            let overwrite = self.ask("File .env already exists. Overwrite? (y/n)", "n");
            if overwrite.eq_ignore_ascii_case("y") {
                if example_path.exists() {
                    let _ = fs::copy(&example_path, &env_path);
                }
                self.success(".env file initialized");
            } else {
                self.warning("Skipping .env overwrite");
            }
        } else if example_path.exists() {
            let _ = fs::copy(&example_path, &env_path);
            self.success(".env file created from .env.example");
        } else {
            let _ = fs::write(
                &env_path,
                "# Padi Environment\nAPP_NAME=\"Padi Rust Api Framework\"\nAPP_ENV=development\nAPP_PORT=8085\n",
            );
            self.success(".env file created with defaults");
        }

        println!();

        // Step 2: Choose Database Driver
        println!("{}", Self::colorize("[2/7] Select Database Driver", "yellow"));
        let db_choice = self.choice(
            "Please select your database:",
            &[
                (1, "SQLite (Embedded, Zero-Setup - Recommended)"),
                (2, "MySQL / MariaDB"),
                (3, "PostgreSQL"),
            ],
            1,
        );

        let (driver, default_port) = match db_choice {
            2 => ("mysql", "3306"),
            3 => ("postgres", "5432"),
            _ => ("sqlite", ""),
        };
        self.success(&format!("Selected database: {}", driver));
        println!();

        // Step 3: Database Configuration
        println!("{}", Self::colorize("[3/7] Database Configuration", "yellow"));
        if driver == "sqlite" {
            let db_path = self.ask("SQLite database path", "database/database.sqlite");
            let full_db_path = self.project_root.join(&db_path);
            if let Some(parent) = full_db_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if !full_db_path.exists() {
                let _ = fs::File::create(&full_db_path);
            }
            self.update_env("DB_CONNECTION", "sqlite");
            self.update_env("DATABASE_URL", &format!("sqlite://{}", db_path.replace('\\', "/")));
            self.success(&format!("SQLite configured: {}", db_path));
        } else {
            let db_host = self.ask("Database Host", "127.0.0.1");
            let db_port = self.ask("Database Port", default_port);
            let db_name = self.ask("Database Name", "rest_api_db");
            let default_user = if driver == "postgres" { "postgres" } else { "root" };
            let db_user = self.ask("Database Username", default_user);
            let db_pass = self.ask("Database Password (empty for none)", "");

            self.update_env("DB_CONNECTION", driver);
            let db_url = Self::build_database_url(driver, &db_user, &db_pass, &db_host, &db_port, &db_name);
            self.update_env("DATABASE_URL", &db_url);
            self.success("Database parameters saved to .env");
        }

        println!();

        // Step 4: Test Database Connection
        println!("{}", Self::colorize("[4/7] Testing Database Connection...", "yellow"));
        env::load(env_path.to_str().unwrap_or(".env"));
        match Database::connect().await {
            Ok(_) => {
                self.success("Database connection successful!");
            }
            Err(e) => {
                self.error(&format!("Database connection failed: {}", e));
                let cont = self.ask("Continue anyway? (y/n)", "n");
                if !cont.eq_ignore_ascii_case("y") {
                    return Err(ApiError::internal("Setup aborted due to database connection failure."));
                }
            }
        }

        println!();

        // Step 5: Generate JWT Secret
        println!("{}", Self::colorize("[5/7] Generating Cryptographic JWT Secret...", "yellow"));
        let jwt_secret = Self::generate_jwt_secret();
        self.update_env("JWT_SECRET", &jwt_secret);
        self.success("JWT_SECRET generated (256-bit cryptographic hex from OS CSPRNG) and saved");

        println!();

        // Step 6: Run Migrations
        println!("{}", Self::colorize("[6/7] Database Migrations", "yellow"));
        let migrate_choice = self.choice(
            "Migration options:",
            &[(1, "Run pending migrations now"), (2, "Skip migrations")],
            1,
        );

        if migrate_choice == 1 {
            let mig_dir = self.project_root.join("database/migrations");
            if mig_dir.exists() {
                match Migrator::run(mig_dir.to_str().unwrap_or("database/migrations")).await {
                    Ok(applied) => {
                        if applied.is_empty() {
                            self.info("No migrations needed.");
                        } else {
                            for m in applied {
                                self.success(&format!("Migrated: {}", m));
                            }
                        }
                    }
                    Err(e) => {
                        self.warning(&format!("Migration warning: {}", e));
                    }
                }
            } else {
                self.info("Migration directory database/migrations does not exist yet. Skipped.");
            }
        }

        println!();

        // Step 7: CRUD Generation
        println!("{}", Self::colorize("[7/7] CRUD Generation", "yellow"));
        let crud_choice = self.choice(
            "Generate CRUD controllers and models?",
            &[
                (1, "Yes - Select specific table"),
                (2, "No - Skip generation"),
            ],
            2,
        );

        if crud_choice == 1 {
            let table = self.ask("Enter table name to scaffold", "users");
            let realtime_choice = self.choice(
                "Enable real-time SSE broadcasting for CRUD?",
                &[
                    (1, "No - Standard REST API"),
                    (2, "Yes - Enable SSE hooks (Realtime::publish)"),
                ],
                1,
            );
            let generator = Generator::new(&self.project_root).with_realtime(realtime_choice == 2);
            if let Err(e) = generator.generate_crud(&table, false).await {
                self.error(&format!("CRUD generation error: {}", e));
            } else {
                self.success(&format!("CRUD for table '{}' generated!", table));
            }
        }

        println!();

        // Final Summary
        let box_header = r#"
╔════════════════════════════════════════════════════════════════╗
║              Setup Completed Successfully! 🎉                 ║
╚════════════════════════════════════════════════════════════════╝
"#;
        println!("{}", Self::colorize(box_header, "green"));
        println!("{}", Self::colorize("Next Steps:", "blue"));
        println!("  1. Run the server:      {}", Self::colorize("cargo run", "yellow"));
        println!("  2. Visit API:           {}", Self::colorize("http://localhost:8085", "yellow"));
        println!("  3. Generate CRUD:       {}", Self::colorize("cargo run -- generate:crud <table_name>", "yellow"));
        println!("  4. Run migrations:      {}", Self::colorize("cargo run -- migrate", "yellow"));
        println!("\n{}", Self::colorize("Happy coding! 🚀", "green"));

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_colorize() {
        let red = SetupWizard::colorize("error", "red");
        assert!(red.starts_with("\x1b[31m"));
        assert!(red.ends_with("\x1b[0m"));

        let plain = SetupWizard::colorize("text", "unknown");
        assert_eq!(plain, "\x1b[0mtext\x1b[0m");
    }

    #[test]
    fn test_update_env_content_insert() {
        let original = "APP_NAME=Padi\n";
        let updated = SetupWizard::update_env_content(original, "APP_PORT", "8080");
        assert!(updated.contains("APP_NAME=Padi"));
        assert!(updated.contains("APP_PORT=8080"));
    }

    #[test]
    fn test_update_env_content_replace() {
        let original = "APP_NAME=Padi\nAPP_PORT=3000\n";
        let updated = SetupWizard::update_env_content(original, "APP_PORT", "8080");
        assert!(updated.contains("APP_PORT=8080"));
        assert!(!updated.contains("APP_PORT=3000"));
    }

    #[test]
    fn test_build_database_url() {
        let without_pass = SetupWizard::build_database_url("mysql", "root", "", "127.0.0.1", "3306", "testdb");
        assert_eq!(without_pass, "mysql://root@127.0.0.1:3306/testdb");

        let with_pass = SetupWizard::build_database_url("postgres", "postgres", "secret", "localhost", "5432", "mydb");
        assert_eq!(with_pass, "postgres://postgres:secret@localhost:5432/mydb");
    }

    #[test]
    fn test_generate_jwt_secret() {
        let s1 = SetupWizard::generate_jwt_secret();
        let s2 = SetupWizard::generate_jwt_secret();

        assert_eq!(s1.len(), 64);
        assert_eq!(s2.len(), 64);
        assert_ne!(s1, s2);
        assert!(s1.chars().all(|c| c.is_ascii_hexdigit()));
    }
}

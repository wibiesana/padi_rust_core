//! Console CLI (port of Console.php).
//!
//! Handles CLI commands, delegating code generation to `Generator`
//! and migrations to `Migrator`.
use std::{
    path::{Path, PathBuf},
};

use crate::{
    error::ApiError,
    generator::Generator,
    migrator::Migrator,
};

pub struct Console {
    pub base_dir: PathBuf,
}

impl Console {
    pub fn new<P: AsRef<Path>>(base_dir: P) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    pub async fn run(&self, args: &[String]) -> Result<(), ApiError> {
        let command = args.first().map(|s| s.as_str()).unwrap_or("help");
        let force = args.iter().any(|a| a == "--force");
        let generator = Generator::new(&self.base_dir);

        match command {
            "help" | "list" | "--help" | "-h" => self.show_help(),
            "make:controller" | "create:controller" => {
                let name = args.get(1).map(|s| s.as_str());
                if let Some(name) = name {
                    generator.generate_controller(name, force)?;
                } else {
                    eprintln!("\x1b[31mError: Controller name is required.\x1b[0m");
                    println!("Usage: cargo run -- make:controller <name>");
                }
            }
            "make:model" | "create:model" => {
                let table_name = args.get(1).map(|s| s.as_str());
                if let Some(table) = table_name {
                    generator.generate_model(table, force).await?;
                } else {
                    eprintln!("\x1b[31mError: Table name is required.\x1b[0m");
                    println!("Usage: cargo run -- make:model <table_name>");
                }
            }
            "make:migration" | "create:migration" => {
                let name = args.get(1).map(|s| s.as_str());
                if let Some(name) = name {
                    generator.generate_migration(name)?;
                } else {
                    eprintln!("\x1b[31mError: Migration name is required.\x1b[0m");
                    println!("Usage: cargo run -- make:migration <name>");
                }
            }
            "generate:crud" | "g" => {
                let table_name = args.get(1).map(|s| s.as_str());
                if let Some(table) = table_name {
                    generator.generate_crud(table, force).await?;
                } else {
                    eprintln!("\x1b[31mError: Table name is required.\x1b[0m");
                    println!("Usage: cargo run -- generate:crud <table_name>");
                }
            }
            "setup" | "init" | "install" => {
                let wizard = crate::setup_wizard::SetupWizard::new(&self.base_dir);
                wizard.run().await?;
            }
            "migrate" => {
                self.migrate().await?;
            }
            "migrate:rollback" => {
                self.migrate_rollback().await?;
            }
            "migrate:status" => {
                self.migrate_status().await?;
            }
            other => {
                eprintln!("\x1b[31mUnknown command: {}\x1b[0m\n", other);
                self.show_help();
            }
        }

        Ok(())
    }

    pub fn show_help(&self) {
        println!("\x1b[32mPadi REST API Framework (Rust)\x1b[0m version \x1b[33m0.1.0\x1b[0m\n");
        println!("\x1b[33mUsage:\x1b[0m");
        println!("  cargo run -- <command> [arguments]\n");
        println!("\x1b[33mAvailable commands:\x1b[0m");
        println!("  \x1b[32mhelp\x1b[0m                       Display this help message");
        println!("  \x1b[32msetup\x1b[0m                      Run interactive project setup wizard");
        println!("\n \x1b[33mmake\x1b[0m");
        println!("  \x1b[32mmake:controller\x1b[0m <name>      Create a new controller");
        println!("  \x1b[32mmake:model\x1b[0m <table_name>   Create a new model");
        println!("  \x1b[32mmake:migration\x1b[0m <name>       Create a new SQL migration file");
        println!("\n \x1b[33mgenerate\x1b[0m");
        println!("  \x1b[32mgenerate:crud\x1b[0m <table_name>  Generate full CRUD (Model, Controller, Resource, Routes)");
        println!("  \x1b[32mg\x1b[0m <table_name>              Alias for generate:crud");
        println!("\n \x1b[33mmigrate\x1b[0m");
        println!("  \x1b[32mmigrate\x1b[0m                     Run pending migrations");
        println!("  \x1b[32mmigrate:rollback\x1b[0m            Rollback last batch of migrations");
        println!("  \x1b[32mmigrate:status\x1b[0m              Show migration status\n");
    }

    pub async fn migrate(&self) -> Result<(), ApiError> {
        let dir = self.base_dir.join("database/migrations");
        let dir_str = dir.to_string_lossy();
        let applied = Migrator::run(&dir_str).await?;
        if applied.is_empty() {
            println!("Nothing to migrate.");
        } else {
            for m in applied {
                println!("\x1b[32m✓ Migrated:\x1b[0m {}", m);
            }
        }
        Ok(())
    }

    pub async fn migrate_rollback(&self) -> Result<(), ApiError> {
        let dir = self.base_dir.join("database/migrations");
        let dir_str = dir.to_string_lossy();
        let reverted = Migrator::rollback(&dir_str).await?;
        if reverted.is_empty() {
            println!("Nothing to rollback.");
        } else {
            for m in reverted {
                println!("\x1b[33m✓ Rolled back:\x1b[0m {}", m);
            }
        }
        Ok(())
    }

    pub async fn migrate_status(&self) -> Result<(), ApiError> {
        let dir = self.base_dir.join("database/migrations");
        let dir_str = dir.to_string_lossy();
        Migrator::print_status(&dir_str).await
    }
}

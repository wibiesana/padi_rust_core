//! Console CLI for Padi Rust Api Framework.
//!
//! Handles CLI commands, delegating code generation to `Generator`,
//! queue processing to `Queue`, migrations to `Migrator`, and setup to `SetupWizard`.
use std::path::{Path, PathBuf};

use crate::{
    error::ApiError,
    generator::Generator,
    migrator::Migrator,
    queue::Queue,
    setup_wizard::SetupWizard,
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

    fn ask_realtime_prompt(args: &[String]) -> bool {
        for arg in args {
            if arg == "--realtime" || arg == "-r" || arg == "--realtime=true" || arg == "--realtime=1" {
                return true;
            }
            if arg == "--no-realtime" || arg == "--realtime=false" || arg == "--realtime=0" {
                return false;
            }
        }

        use std::io::Write;
        print!("📡 Enable Real-time SSE broadcasting hooks for this CRUD? [y/N]: ");
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim().to_lowercase();
            return trimmed == "y" || trimmed == "yes";
        }
        false
    }

    pub async fn run(&self, args: &[String]) -> Result<(), ApiError> {
        let command = args.first().map(|s| s.as_str()).unwrap_or("help");
        let force = args.iter().any(|a| a == "--force");
        let generator = Generator::new(&self.base_dir);

        match command {
            "help" | "list" | "--help" | "-h" => self.show_help(),

            "make:controller" | "create:controller" => {
                let name = args.iter().skip(1).find(|s| !s.starts_with('-')).map(|s| s.as_str());
                let realtime = args.iter().any(|a| a == "--realtime" || a == "-r" || a == "--realtime=true" || a == "--realtime=1");
                if let Some(name) = name {
                    generator.with_realtime(realtime).generate_controller(name, force).await?;
                } else {
                    eprintln!("\x1b[31mError: Controller name is required.\x1b[0m");
                    println!("Usage: padi make:controller <name> [--realtime / -r] [--force]");
                }
            }

            "make:model" | "create:model" => {
                let table_name = args.get(1).map(|s| s.as_str()).filter(|s| !s.starts_with("--"));
                if let Some(table) = table_name {
                    generator.generate_model(table, force).await?;
                } else {
                    eprintln!("\x1b[31mError: Table name is required.\x1b[0m");
                    println!("Usage: padi make:model <table_name>");
                }
            }

            "make:resource" | "create:resource" => {
                let name = args.get(1).map(|s| s.as_str()).filter(|s| !s.starts_with("--"));
                if let Some(name) = name {
                    generator.generate_resource(name, force).await?;
                } else {
                    eprintln!("\x1b[31mError: Resource name is required.\x1b[0m");
                    println!("Usage: padi make:resource <name>");
                }
            }

            "make:migration" | "create:migration" => {
                let name = args.get(1).map(|s| s.as_str()).filter(|s| !s.starts_with("--"));
                if let Some(name) = name {
                    generator.generate_migration(name)?;
                } else {
                    eprintln!("\x1b[31mError: Migration name is required.\x1b[0m");
                    println!("Usage: padi make:migration <name>");
                }
            }

            "make:route" | "create:route" => {
                let name = args.get(1).map(|s| s.as_str()).filter(|s| !s.starts_with("--"));
                if let Some(name) = name {
                    generator.generate_route(name, force)?;
                } else {
                    eprintln!("\x1b[31mError: Route name is required.\x1b[0m");
                    println!("Usage: padi make:route <name>");
                }
            }

            "generate:crud" | "g" => {
                let table_name = args.iter().skip(1).find(|s| !s.starts_with('-')).map(|s| s.as_str());
                if let Some(table) = table_name {
                    let realtime = Self::ask_realtime_prompt(args);
                    let generator = generator.with_realtime(realtime);
                    generator.generate_crud(table, force).await?;
                } else {
                    eprintln!("\x1b[31mError: Table name is required.\x1b[0m");
                    println!("Usage: padi generate:crud <table_name> [--realtime / -r] [--force]");
                    println!("   or: padi g <table_name> [--realtime / -r] [--force]");
                }
            }

            "generate:crud-all" | "ga" => {
                let realtime = Self::ask_realtime_prompt(args);
                let generator = generator.with_realtime(realtime);
                generator.generate_crud_all(force).await?;
            }

            "make:collection"
            | "create:collection"
            | "make:api-collection"
            | "make:api_collection"
            | "collection"
            | "api_collection"
            | "make:postman"
            | "postman" => {
                let table_name = args.get(1).map(|s| s.as_str()).filter(|s| !s.starts_with("--"));
                if let Some(table) = table_name {
                    generator.generate_api_collection(table, force).await?;
                } else {
                    generator.generate_api_collection_all(force).await?;
                }
            }

            "setup" | "init" | "install" => {
                let wizard = SetupWizard::new(&self.base_dir);
                wizard.run().await?;
            }

            "migrate" => {
                let mut filter_tables = Vec::new();
                for arg in args.iter().skip(1) {
                    if let Some(tables_str) = arg.strip_prefix("--tables=") {
                        for t in tables_str.split(',') {
                            let t = t.trim();
                            if !t.is_empty() {
                                filter_tables.push(t);
                            }
                        }
                    } else if let Some(table_str) = arg.strip_prefix("--table=") {
                        let t = table_str.trim();
                        if !t.is_empty() {
                            filter_tables.push(t);
                        }
                    }
                }

                if filter_tables.is_empty() {
                    self.migrate().await?;
                } else {
                    self.migrate_filtered(&filter_tables).await?;
                }
            }

            "migrate:rollback" => {
                let mut steps = 1;
                for arg in args.iter().skip(1) {
                    if let Some(s) = arg.strip_prefix("--step=") {
                        if let Ok(n) = s.parse::<usize>() {
                            steps = n;
                        }
                    } else if let Some(s) = arg.strip_prefix("--steps=") {
                        if let Ok(n) = s.parse::<usize>() {
                            steps = n;
                        }
                    }
                }
                self.migrate_rollback_steps(steps).await?;
            }

            "migrate:status" => {
                self.migrate_status().await?;
            }

            "queue:work" | "queue" => {
                let mut queue_name = None;
                let mut once = false;
                let mut stop_when_empty = false;

                for arg in args.iter().skip(1) {
                    if arg == "--once" {
                        once = true;
                    } else if arg == "--stop-when-empty" {
                        stop_when_empty = true;
                    } else if !arg.starts_with("--") {
                        queue_name = Some(arg.as_str());
                    }
                }

                Queue::work(queue_name, once, stop_when_empty).await?;
            }

            "serve" => {
                println!("\x1b[32mTo run the Padi server, use:\x1b[0m");
                println!("  padi serve");
                println!("  or: cargo run\n");
            }

            other => {
                eprintln!("\x1b[31mUnknown command: {}\x1b[0m\n", other);
                self.show_help();
            }
        }

        Ok(())
    }

    pub fn show_help(&self) {
        println!("\x1b[32mPadi Rust Api Framework\x1b[0m version \x1b[33m0.1.0\x1b[0m\n");
        println!("\x1b[33mUsage:\x1b[0m");
        println!("  padi <command> [options] [arguments]");
        println!("  cargo run -- <command> [options] [arguments]\n");

        println!("\x1b[33mAvailable commands:\x1b[0m");
        println!("  \x1b[32mserve\x1b[0m                      Start Padi Rust Api Framework server (--port, --host)");
        println!("  \x1b[32minit\x1b[0m                       Initialize the application (Run Setup Wizard)");
        println!("  \x1b[32msetup\x1b[0m                      Alias for init");

        println!("\n \x1b[33mmake\x1b[0m");
        println!("  \x1b[32mmake:controller\x1b[0m <name>      Create a new controller");
        println!("  \x1b[32mmake:model\x1b[0m <table_name>   Create a new model from database table");
        println!("  \x1b[32mmake:resource\x1b[0m <name>        Create a new API resource");
        println!("  \x1b[32mmake:route\x1b[0m <name>           Create and mount a new API route");
        println!("  \x1b[32mmake:migration\x1b[0m <name>       Create a new SQL migration file");
        println!("  \x1b[32mmake:collection\x1b[0m [table]      Generate API Collection (api_collection/<table_name>_collection.json)");

        println!("\n \x1b[33mmigrate\x1b[0m");
        println!("  \x1b[32mmigrate\x1b[0m                     Run pending migrations (--tables=users,posts)");
        println!("  \x1b[32mmigrate:rollback\x1b[0m            Rollback last batch of migrations (--step=<n>)");
        println!("  \x1b[32mmigrate:status\x1b[0m              Show migration status");

        println!("\n \x1b[33mgenerate\x1b[0m");
        println!("  \x1b[32mgenerate:crud\x1b[0m <table_name>  Generate complete CRUD (Model, Controller, Resource, Routes) [--realtime, --force]");
        println!("  \x1b[32mg\x1b[0m <table_name>              Alias for generate:crud");
        println!("  \x1b[32mgenerate:crud-all\x1b[0m           Generate complete CRUD for ALL tables in database [--realtime, --force]");
        println!("  \x1b[32mga\x1b[0m                         Alias for generate:crud-all");

        println!("\n \x1b[33mqueue\x1b[0m");
        println!("  \x1b[32mqueue:work\x1b[0m [queue]        Start processing jobs on the queue (--once, --stop-when-empty)");
        println!("  \x1b[32mqueue\x1b[0m [queue]             Alias for queue:work\n");
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

    pub async fn migrate_filtered(&self, tables: &[&str]) -> Result<(), ApiError> {
        let dir = self.base_dir.join("database/migrations");
        let dir_str = dir.to_string_lossy();
        println!("Migrating specific tables: {}", tables.join(", "));
        let applied = Migrator::run_filtered(&dir_str, Some(tables)).await?;
        if applied.is_empty() {
            println!("Nothing to migrate for selected tables.");
        } else {
            for m in applied {
                println!("\x1b[32m✓ Migrated:\x1b[0m {}", m);
            }
        }
        Ok(())
    }

    pub async fn migrate_rollback(&self) -> Result<(), ApiError> {
        self.migrate_rollback_steps(1).await
    }

    pub async fn migrate_rollback_steps(&self, steps: usize) -> Result<(), ApiError> {
        let dir = self.base_dir.join("database/migrations");
        let dir_str = dir.to_string_lossy();
        let reverted = Migrator::rollback_steps(&dir_str, steps).await?;
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

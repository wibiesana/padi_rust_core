<div align="center">

# 🌾 Padi Core (Rust)

**The Industrial-Grade, Zero-Bloat Rust Engine for the Modern Web**

[![Rust](https://img.shields.io/badge/Rust-2021_Edition-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![GitHub](https://img.shields.io/badge/GitHub-wibiesana%2Fpadi__rust__core-blue?style=flat-square&logo=github)](https://github.com/wibiesana/padi_rust_core)

</div>

`padi_core` is the core engine for the **Padi REST API Framework** in Rust. It provides ActiveRecord ORM, Axum routing, JWT authentication, file uploads, queue workers, real-time SSE broadcasting, migration runner, code generator, and CLI console tools.

---

## 📦 Installation

Add `padi_core` to your `Cargo.toml`:

### From GitHub Repository
```toml
[dependencies]
padi_core = { git = "https://github.com/wibiesana/padi_rust_core.git", branch = "main" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

### To Update `padi_core`
Whenever updates are pushed to the repository, update your project using:
```bash
cargo update -p padi_core
```

---

## ✨ Features

- ⚡ **High-Performance Axum Router**: Fluent route builder and standard REST verbs.
- 🗄️ **Multi-Database ActiveRecord ORM**: Supports PostgreSQL, MySQL, and SQLite.
- 🔐 **JWT Authentication & RBAC**: Integrated role and permission extractors.
- 🛠️ **CLI & Auto CRUD Generator**: Scaffold controllers, models, resources, and routes instantly.
- 📦 **Database-Backed Queue**: Async background job processing with retry backoff.
- 📡 **Real-time SSE Broadcasting**: Built-in Mercure-compatible server-sent events.
- 📜 **SQL Migrator**: Automatic batch tracking, rollback, and status inspection.

---

## 📄 License

Licensed under the [MIT License](LICENSE).

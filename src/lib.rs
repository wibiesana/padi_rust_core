//! Padi Core - Core engine for Padi Rust Api Framework.
pub mod active_record;
pub mod application;
pub mod auth;
pub mod cache;
pub mod console;
pub mod database;
pub use database as db;
pub mod email;
pub mod env;
pub use env::Env;
pub mod error;
pub mod file;
pub use file::{File, FileHelper};
pub mod generator;
pub mod hash;
pub mod logger;
pub use logger::Logger;
pub mod migrator;
pub use migrator::Migrator;
pub mod model;
pub mod query;
pub mod queue;
pub use queue::Queue;
pub mod realtime;
pub use realtime::Realtime;
pub mod request;
pub mod resource;
pub mod response;
pub mod router;
pub use router::{Router as PadiRouter, RouterExt};
pub mod setup_wizard;
pub use setup_wizard::SetupWizard;
pub mod validator;

pub use axum;
pub use serde_json;
pub use request::Request;

/// Everything you normally need in a controller.
pub mod prelude {
    pub use crate::application::Application;
    pub use crate::auth::{Auth, AuthUser};
    pub use crate::cache::Cache;
    pub use crate::console::Console;
    pub use crate::database::{Database, Val};
    pub use crate::email::Email;
    pub use crate::env::Env;
    pub use crate::error::ApiError;
    pub use crate::file::{File, FileHelper};
    pub use crate::generator::Generator;
    pub use crate::logger::Logger;
    pub use crate::migrator::Migrator;
    pub use crate::model::{
        current_user_id, with_user, ActiveRecord, Model, ModelQuery, Pk, Relation, RelationType,
        SearchableRelation, TimestampFormat,
    };
    pub use crate::query::Query;
    pub use crate::queue::Queue;
    pub use crate::realtime::Realtime;
    pub use crate::request::{Body, Params, Request, UploadedFile};
    pub use crate::resource::{
        when_loaded, when_loaded_then, CollectionResource, ItemResource, Resource,
    };
    pub use crate::response::{
        created, no_content, ok, ApiResponse, ApiResult, Response, status_code_name, status_text,
    };
    pub use crate::router::{Router as PadiRouter, RouterExt};
    pub use crate::validator::Validator;
    pub use axum::{
        extract::Path,
        routing::{delete, get, patch, post, put},
        Router,
    };
    pub use serde_json::{json, Value};
}

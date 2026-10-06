//! Padi Core - Rust port of the Padi REST API Framework.
pub mod active_record;
pub mod application;
pub mod auth;
pub mod cache;
pub mod db;
pub mod env;
pub mod error;
pub mod hash;
pub mod logger;
pub mod migrator;
pub mod model;
pub mod query;
pub mod request;
pub mod resource;
pub mod response;
pub mod validator;

pub use axum;
pub use serde_json;

/// Everything you normally need in a controller.
pub mod prelude {
    pub use crate::application::Application;
    pub use crate::auth::{Auth, AuthUser};
    pub use crate::cache::Cache;
    pub use crate::db::{Database, Val};
    pub use crate::error::ApiError;
    pub use crate::model::{
        current_user_id, with_user, ActiveRecord, Model, ModelQuery, Pk, Relation, RelationType,
        SearchableRelation, TimestampFormat,
    };
    pub use crate::query::Query;
    pub use crate::request::{Body, Params};
    pub use crate::resource::{when_loaded, Resource};
    pub use crate::response::{created, no_content, ok, ApiResponse, ApiResult};
    pub use crate::validator::Validator;
    pub use axum::{
        extract::Path,
        routing::{delete, get, patch, post, put},
        Router,
    };
    pub use serde_json::{json, Value};
}

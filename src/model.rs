//! Model and ActiveRecord ORM abstractions for Padi Rust Api Framework.
//!
//! Provides the primary data model layer, including static [`Model`] declarations,
//! query building via [`ModelQuery`], relationship declarations, automated audit fields,
//! and scoped lifecycle hooks.
//!
//! # Overview
//!
//! Models are defined as `pub const` instances using a fluent builder pattern:
//!
//! ```rust,ignore
//! use padi_core::prelude::*;
//!
//! pub const USER: Model = Model::new("users")
//!     .fillable(&["name", "email", "password"])
//!     .hidden(&["password"])
//!     .default_order("created_at DESC");
//! ```
//!
//! All core ActiveRecord implementations are located in [`crate::active_record`]
//! and re-exported here for an idiomatic model API.

pub use crate::active_record::{
    current_user_id, with_user, ActiveRecord, AfterDelete, AfterLoad, AfterSave,
    BeforeDelete, BeforeSave, Model, ModelQuery, Pk, Relation, RelationType,
    SearchableRelation, TimestampFormat,
};

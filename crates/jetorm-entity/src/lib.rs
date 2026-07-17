//! Entity metadata contracts for JetORM frontends.
//!
//! This crate defines the static description of a database entity: its table
//! identity, its ordered column metadata, and the conversion contract between
//! Rust field values and the dialect-independent [`Value`] currency. It owns
//! no IO and no query semantics; query construction lives in `jetorm-query`
//! and IR lowering targets the `afterburner` compiler crate.
//!
//! # Design
//!
//! - [`Entity`] is implemented by a zero-sized marker type per table. All
//!   metadata is `const` so schema information is available without allocation
//!   and without a database connection.
//! - [`Model`] is implemented by the user's plain data struct and converts a
//!   row to and from positional [`Value`]s in [`Entity::COLUMNS`] order.
//! - [`Column`] is implemented by one zero-sized marker type per column. The
//!   marker carries the column's Rust type, so downstream expression builders
//!   are type-checked at compile time.
//!
//! `#[derive(JetModel)]` from `jetorm-derive` generates every implementation
//! in this module; the traits can also be implemented by hand.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod entity;
mod error;
mod json;
mod meta;
mod relation;
mod value;

pub use entity::{Column, Entity, KeyedEntity, Model, SingleKeyEntity};
pub use error::{DecodeError, ValueTypeMismatch};
pub use json::Json;
pub use meta::{ColumnMeta, ColumnType, TableMeta};
pub use relation::{ForeignKeyMeta, ForeignKeyRef, Inverse, ReferentialAction, Relation};
pub use value::{SqlValue, Value};

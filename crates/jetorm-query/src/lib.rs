//! Typed query and mutation construction with AfterBurner IR lowering for JetORM.
//!
//! This crate is the frontend layer between entity metadata and the
//! `afterburner` optimizer. Queries are assembled through a typed builder
//! whose expressions are checked against column Rust types at compile time and
//! stored as a small backend-independent AST. [`afterburner::IntoAfterBurnerIr`]
//! lowers that AST, while [`afterburner::afterburner!`] adds verification at
//! the frontend boundary. Inserts, updates, deletes, upserts, and `RETURNING`
//! use the same path without accepting raw SQL text.
//!
//! # Parameters, not literals
//!
//! Every user-supplied value becomes an IR parameter, never a literal. The
//! value itself is retained in the builder's positional bind table, exposed
//! by methods such as [`Select::binds`] and [`Insert::binds`]. Two builders
//! that differ only in bound values therefore lower to structurally identical
//! IR. Shape-based plan caches reuse one rendered statement, while stable IR
//! fingerprints give profile-guided optimization one identity after lowering.
//!
//! # Example
//!
//! ```
//! use afterburner::afterburner;
//! use jetorm_entity::{Column, ColumnMeta, ColumnType, Entity, TableMeta};
//! use jetorm_query::{ColumnExt, EntityQuery};
//! # use jetorm_entity::{DecodeError, Model, SqlValue, Value};
//!
//! #[derive(Clone, Copy, Debug)]
//! pub struct UserEntity;
//!
//! # #[derive(Clone, Debug)]
//! # pub struct User { id: i64 }
//! # impl Model for User {
//! #     type Entity = UserEntity;
//! #     fn into_values(self) -> Vec<Value> { vec![self.id.into_value()] }
//! #     fn from_values(values: Vec<Value>) -> Result<Self, DecodeError> {
//! #         let mut values = values.into_iter();
//! #         Ok(Self { id: i64::from_value(values.next().unwrap()).unwrap() })
//! #     }
//! #     fn value(&self, _: usize) -> Option<Value> { Some(self.id.into_value()) }
//! # }
//! impl Entity for UserEntity {
//!     type Model = User;
//!     const TABLE: TableMeta = TableMeta::new("users");
//!     const COLUMNS: &'static [ColumnMeta] =
//!         &[ColumnMeta::new("id", "id", ColumnType::Int64).primary_key()];
//!     const PRIMARY_KEY: &'static [usize] = &[0];
//! }
//!
//! #[derive(Clone, Copy, Debug)]
//! pub struct Id;
//!
//! impl Column for Id {
//!     type Entity = UserEntity;
//!     type Rust = i64;
//!     type Field = i64;
//!     const INDEX: usize = 0;
//!     const NULLABLE: bool = false;
//! }
//!
//! let query = UserEntity::find().filter(Id.gt(100)).limit(10);
//! // The predicate value plus the row limit both ride in the bind table.
//! assert_eq!(query.binds().len(), 2);
//! let module = afterburner!(query).expect("query lowers to verified IR");
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod aggregate;
mod behavior;
mod cursor;
mod expr;
mod join;
mod lowering;
mod mutation;
mod projection;
mod select;

pub use aggregate::{
    Aggregate, AggregateFunction, AggregateList, AggregateRef, AggregateSpec, Averageable,
    Comparable, GroupBy, GroupedSelect, HavingExpr, Summable, avg, count_rows, max, min, sum,
};
pub use behavior::Exists;
pub use cursor::{Cursor, CursorKey, CursorPage};
pub use expr::{ColumnExt, Expr, OrderKey, TextColumnExt};
pub use join::{Join2Select, JoinSelect};
pub use lowering::LoweringError;
pub use mutation::{Delete, EntityMutation, Insert, Returning, Update};
pub use projection::{ColumnList, Projected};
pub use select::{CacheableQuery, CountQuery, EntityQuery, QueryShape, Select};

/// Typing facts every [`jetorm_entity::SqlValue`] carries, spelled as a
/// separate trait so generic code can read them without naming the value.
#[doc(hidden)]
pub trait SqlValueTyping {
    /// Canonical column type of the Rust type.
    const COLUMN_TYPE_OF: jetorm_entity::ColumnType;
    /// Whether the Rust type itself models SQL `NULL`.
    const NULLABLE_OF: bool;
    /// Named database type backing the Rust type, when one exists.
    const TYPE_NAME_OF: Option<&'static str>;
}

impl<T> SqlValueTyping for T
where
    T: jetorm_entity::SqlValue,
{
    const COLUMN_TYPE_OF: jetorm_entity::ColumnType = T::COLUMN_TYPE;
    const NULLABLE_OF: bool = T::NULLABLE;
    const TYPE_NAME_OF: Option<&'static str> = T::TYPE_NAME;
}

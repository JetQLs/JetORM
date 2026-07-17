//! Typed query construction and AfterBurner IR lowering for JetORM.
//!
//! This crate is the frontend layer between entity metadata and the
//! `afterburner` optimizer. Queries are assembled through a typed builder
//! whose expressions are checked against column Rust types at compile time,
//! stored as a small backend-independent AST, and lowered into verified
//! AfterBurner IR through [`afterburner::IntoAfterBurnerIr`].
//!
//! # Parameters, not literals
//!
//! Every user-supplied value becomes an IR parameter, never a literal. The
//! value itself is retained in the query's positional bind table
//! ([`Select::binds`]). Two queries that differ only in bound values
//! therefore lower to structurally identical IR, which keeps plan caches and
//! profile-guided optimization keyed on one stable fingerprint per query
//! shape.
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

mod expr;
mod lowering;
mod projection;
mod select;

pub use expr::{ColumnExt, Expr, OrderKey, TextColumnExt};
pub use lowering::LoweringError;
pub use projection::{ColumnList, Projected};
pub use select::{CacheableQuery, CountQuery, EntityQuery, QueryShape, Select};

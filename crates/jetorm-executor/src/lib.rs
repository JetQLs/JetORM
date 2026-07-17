//! Connection management and statement execution for JetORM.
//!
//! This crate owns the runtime side of the pipeline: connection pooling,
//! transactions, plan caching, parameter binding, and decoding driver rows
//! into typed results. The wire protocol is deliberately not reimplemented —
//! [`sqlx`] provides the PostgreSQL driver, and JetORM's effort stays on the
//! optimizer pipeline above it.
//!
//! # Execution pipeline
//!
//! Running a typed read or mutation performs, in order:
//!
//! 1. **Validation** — value-dependent safety rules run on every execution,
//!    including cache hits.
//! 2. **Plan lookup** — a value-independent query shape probes the shared
//!    cache. A miss lowers the typed builder into verified AfterBurner IR and
//!    renders SQL; a hit skips those stages entirely.
//! 3. **Binding** — the statement's `bind_order` maps frontend bind-table
//!    positions onto `$n` placeholders; values bind through their exact
//!    PostgreSQL types.
//! 4. **Decoding** — row-producing statements decode positionally through
//!    [`jetorm_entity::Value`]s. Depending on the builder, those values become
//!    an entity model, a joined model pair, a projected column, or a scalar.
//!
//! Row-producing statements and affected-row commands use separate executor
//! paths. The rendered statement carries that result contract, and a mismatch
//! is rejected before the driver executes SQL.
//!
//! # Example
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use jetorm_executor::{Database, SelectExecute};
//! # use jetorm_entity::{Column, ColumnMeta, ColumnType, Entity, TableMeta};
//! # use jetorm_query::EntityQuery;
//! # #[derive(Clone, Copy, Debug)] pub struct UserEntity;
//! # #[derive(Clone, Debug)] pub struct User { id: i64 }
//! # impl jetorm_entity::Model for User {
//! #     type Entity = UserEntity;
//! #     fn into_values(self) -> Vec<jetorm_entity::Value> { vec![] }
//! #     fn from_values(_: Vec<jetorm_entity::Value>) -> Result<Self, jetorm_entity::DecodeError> { Ok(Self { id: 0 }) }
//! #     fn value(&self, _: usize) -> Option<jetorm_entity::Value> { Some(jetorm_entity::Value::Int64(self.id)) }
//! # }
//! # impl Entity for UserEntity {
//! #     type Model = User;
//! #     const TABLE: TableMeta = TableMeta::new("users");
//! #     const COLUMNS: &'static [ColumnMeta] =
//! #         &[ColumnMeta::new("id", "id", ColumnType::Int64).primary_key()];
//! #     const PRIMARY_KEY: &'static [usize] = &[0];
//! # }
//!
//! let db = Database::connect("postgres://localhost/app").await?;
//! let users: Vec<User> = UserEntity::find().all(&db).await?;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod database;
mod error;
mod join;
mod mutation;
mod paginate;
mod plan;
mod relations;
mod row;
mod scalar;
mod select;
mod value;

pub use database::{Database, DatabaseOptions, Executor, Transaction};
pub use error::{ErrorKind, ExecuteError};
pub use join::JoinExecute;
pub use mutation::{MutationExecute, ReturningExecute};
pub use paginate::{CursorExecute, PaginateExecute, Paginator};
pub use plan::PlanCache;
pub use relations::{load_many, load_one};
pub use row::JetRow;
pub use scalar::ExistsExecute;
pub use select::{GroupedExecute, ProjectedExecute, SelectExecute};

/// The `sqlx` version JetORM is built against.
///
/// [`Database::pool`] and [`Transaction::connection`] hand out driver types,
/// and `sqlx` types only interoperate within one semver-compatible version.
/// Reaching for the escape hatch through this re-export makes that agreement
/// automatic instead of a version constraint downstream has to mirror.
///
/// [`Database::pool`]: crate::Database::pool
/// [`Transaction::connection`]: crate::Transaction::connection
pub use sqlx;

//! Connection management and statement execution for JetORM.
//!
//! This crate owns the runtime side of the pipeline: connection pooling,
//! transactions, plan caching, parameter binding, and decoding driver rows
//! back into entity models. The wire protocol is deliberately not
//! reimplemented — [`sqlx`] provides the PostgreSQL driver, and JetORM's
//! effort stays on the optimizer pipeline above it.
//!
//! # Execution pipeline
//!
//! Running a [`jetorm_query::Select`] performs, in order:
//!
//! 1. **Lowering** — the typed builder lowers into AfterBurner IR and is
//!    verified (`afterburner!`).
//! 2. **Plan cache** — the module's structural fingerprint keys a cache of
//!    rendered statements. Queries differing only in bound values share one
//!    fingerprint, so SQL rendering (and, later, optimizer passes) run once
//!    per query shape.
//! 3. **Binding** — the statement's `bind_order` maps frontend bind-table
//!    positions onto `$n` placeholders; values bind through their exact
//!    PostgreSQL types.
//! 4. **Decoding** — each row decodes positionally into
//!    [`jetorm_entity::Value`]s using the entity's column metadata, then into
//!    the user's model through [`jetorm_entity::Model::from_values`].
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
mod plan;
mod select;
mod value;

pub use database::{Database, DatabaseOptions, Executor, Transaction};
pub use error::ExecuteError;
pub use plan::PlanCache;
pub use select::SelectExecute;

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

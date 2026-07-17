//! JetORM — a typed async ORM built on the AfterBurner query optimizer.
//!
//! This facade re-exports the JetORM frontend as one dependency. Application
//! code derives entity metadata with [`JetModel`], builds typed reads and
//! mutations, and lowers them into verified AfterBurner IR; SQL rendering and
//! execution layers consume that IR downstream.
//!
//! # Example
//!
//! ```
//! use jetorm::prelude::*;
//!
//! // `JetModel` generates module-level items (an entity marker and a column
//! // module), so it is used at module scope rather than inside a function.
//! #[derive(Clone, Debug, JetModel)]
//! #[jet(table = "users")]
//! pub struct User {
//!     #[jet(primary_key, auto_increment)]
//!     pub id: i64,
//!     pub name: String,
//!     pub email: Option<String>,
//! }
//!
//! fn main() {
//!     use jetorm::{Dialect, IntoAfterBurnerIr, Postgres};
//!
//!     let query = UserEntity::find()
//!         .filter(user::Email.like("%@example.com").and(user::Id.gt(100)))
//!         .order_by(user::Id.desc())
//!         .limit(20);
//!     // Two predicate values plus the row limit: every user value is a
//!     // bind, so query shapes stay identical across bound values.
//!     assert_eq!(query.binds().len(), 3);
//!
//!     // `render_query` verifies the module itself, so the lowering is left
//!     // unverified here — verifying both sides would walk the module twice.
//!     // (Executing through `SelectExecute` does all of this internally.)
//!     let module = query.into_afterburner_ir().expect("query lowers to IR");
//!     let statement = Postgres.render_query(&module).expect("IR renders to SQL");
//!     assert!(statement.sql().starts_with("SELECT"));
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use afterburner::{AfterBurnerError, AfterBurnerOptions, IntoAfterBurnerIr, afterburner, ir};
pub use jetorm_derive::{JetEnum, JetModel, JetPartial};
pub use jetorm_dialect::{Dialect, Postgres, RenderError, Statement, StatementResult};
pub use jetorm_entity::{
    Column, ColumnMeta, ColumnType, DecodeError, Entity, EnumMeta, ForeignKeyMeta, ForeignKeyRef,
    Inverse, Json, KeyedEntity, Model, ReferentialAction, Relation, ScalarValue, SingleKeyEntity,
    SqlValue, TableMeta, Value, ValueTypeMismatch,
};
#[cfg(feature = "executor")]
pub use jetorm_executor::{
    CursorExecute, Database, DatabaseOptions, ErrorKind, ExecuteError, Executor, ExistsExecute,
    GroupedExecute, JetRow, Join2Execute, JoinExecute, MutationExecute, PaginateExecute, Paginator,
    PlanCache, ProjectedExecute, ReturningExecute, SelectExecute, Transaction, load_many, load_one,
};
pub use jetorm_query::{
    Aggregate, AggregateFunction, AggregateList, AggregateRef, AggregateSpec, Averageable,
    CacheableQuery, ColumnExt, ColumnList, Comparable, CountQuery, Cursor, CursorPage, Delete,
    EntityMutation, EntityQuery, Exists, Expr, GroupBy, GroupedSelect, HavingExpr, Insert,
    JoinSelect, LoweringError, OrderKey, Projected, Returning, Select, Summable, TextColumnExt,
    Update, avg, count_rows, max, min, sum,
};

/// Single-import surface for application code.
pub mod prelude {
    pub use crate::{
        Column, ColumnExt, ColumnList, Entity, EntityMutation, EntityQuery, Expr, Inverse, JetEnum,
        JetModel, JetPartial, Json, Model, OrderKey, Projected, Relation, Select, SqlValue,
        TextColumnExt, Value,
    };
    #[cfg(feature = "executor")]
    pub use crate::{
        CursorExecute, Database, ExistsExecute, GroupedExecute, Join2Execute, JoinExecute,
        MutationExecute, PaginateExecute, ProjectedExecute, ReturningExecute, SelectExecute,
        Transaction, load_many, load_one,
    };
}

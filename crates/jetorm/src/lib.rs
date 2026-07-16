//! JetORM — a typed async ORM built on the AfterBurner query optimizer.
//!
//! This facade re-exports the JetORM frontend as one dependency. Application
//! code derives entity metadata with [`JetModel`], builds typed queries with
//! [`Select`], and lowers them into verified AfterBurner IR; SQL rendering
//! and execution layers consume that IR downstream.
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
//!     use jetorm::{Dialect, Postgres};
//!
//!     let query = UserEntity::find()
//!         .filter(user::Email.like("%@example.com").and(user::Id.gt(100)))
//!         .order_by(user::Id.desc())
//!         .limit(20);
//!     assert_eq!(query.binds().len(), 2);
//!
//!     let module = jetorm::afterburner!(query).expect("query lowers to verified IR");
//!     let statement = Postgres.render_query(&module).expect("IR renders to SQL");
//!     assert!(statement.sql().starts_with("SELECT"));
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub use afterburner::{AfterBurnerError, AfterBurnerOptions, IntoAfterBurnerIr, afterburner, ir};
pub use jetorm_derive::JetModel;
pub use jetorm_dialect::{Dialect, Postgres, RenderError, Statement};
pub use jetorm_entity::{
    Column, ColumnMeta, ColumnType, DecodeError, Entity, Model, SqlValue, TableMeta, Value,
    ValueTypeMismatch,
};
pub use jetorm_query::{
    ColumnExt, EntityQuery, Expr, LoweringError, OrderKey, Select, TextColumnExt,
};

/// Single-import surface for application code.
pub mod prelude {
    pub use crate::{
        Column, ColumnExt, Entity, EntityQuery, Expr, JetModel, Model, OrderKey, Select, SqlValue,
        TextColumnExt, Value,
    };
}

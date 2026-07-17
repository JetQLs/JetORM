//! Abstract schema model and diff engine for JetORM.
//!
//! This crate defines the database-independent description of a schema —
//! tables, columns, and keys — and computes the change set between two
//! schema states. It is pure logic with no IO: migration generation
//! (`jet migrate generate`), drift checking (`jet check`), and database
//! introspection (`jet db pull`) all build on it.
//!
//! # Code-first flow
//!
//! JetORM treats entity definitions as the source of truth:
//!
//! 1. The **target** schema comes from entity metadata
//!    ([`TableDef::from_entity`]).
//! 2. The **current** schema is reconstructed by replaying applied
//!    migrations ([`SchemaSet::apply`]).
//! 3. [`diff`] computes the ordered change set turning current into target.
//!    Destructive changes are flagged, and drop/add pairs that look like
//!    renames surface as [`RenameCandidate`]s for explicit confirmation —
//!    the differ never guesses a rename on its own.
//!
//! The core invariant, exercised heavily by tests: applying `diff(a, b)` to
//! `a` always reproduces `b` exactly.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod apply;
mod diff;
mod model;

pub use apply::ApplyError;
pub use diff::{RenameCandidate, SchemaChange, SchemaDiff, diff};
pub use model::{ColumnDef, ForeignKeyDef, SchemaSet, TableDef, TableName};

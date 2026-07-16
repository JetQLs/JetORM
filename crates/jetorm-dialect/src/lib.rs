//! SQL rendering from verified AfterBurner IR.
//!
//! This crate walks a verified [`afterburner::ir::Module`] and renders one
//! SQL statement plus a positional parameter layout for a concrete database
//! dialect. It is the only JetORM layer that knows SQL spelling: the query
//! frontend never produces SQL text and the executor never inspects it.
//!
//! # Correctness contract
//!
//! The renderer is deliberately conservative:
//!
//! - Consecutive relational operations fuse into one flat `SELECT` only when
//!   SQL clause evaluation order (`FROM` → `WHERE` → `DISTINCT` → `ORDER BY`
//!   → `LIMIT`) matches the operation order. Anything else is wrapped in a
//!   derived table, and shapes SQL cannot express faithfully — such as an
//!   interior sort without a row limit, whose ordering a subquery does not
//!   preserve — are rejected with [`RenderError::Unsupported`] instead of
//!   silently producing misleading SQL.
//! - Every IR parameter renders with an explicit type cast derived from its
//!   SSA type, so prepared-statement type inference can never fail or drift.
//! - Sub-expressions are always parenthesized; correctness never depends on
//!   an operator-precedence table.
//! - Operations the dialect cannot render yet (joins, aggregates, window
//!   functions, set operations, extension dialects) fail loudly with a
//!   precise diagnostic.
//!
//! Statements are rendered from IR after optimizer passes, so a
//! [`Statement`] is cacheable under the module's structural fingerprint.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod dialect;
mod error;
mod statement;

pub mod postgres;

pub use dialect::Dialect;
pub use error::RenderError;
pub use postgres::Postgres;
pub use statement::Statement;

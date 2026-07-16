//! Connection management and statement execution for JetORM.
//!
//! This crate owns the runtime side of the pipeline: connection pooling,
//! transactions, prepared-statement and plan caching, parameter binding, and
//! decoding driver rows into positional [`jetorm_entity::Value`]s for model
//! reconstruction.
//!
//! # Planned design
//!
//! - A `Driver` trait isolates the wire protocol; the first implementation
//!   wraps `sqlx` so JetORM's effort stays on the optimizer pipeline.
//!   The trait boundary allows native protocol implementations later.
//! - A plan cache keyed by AfterBurner structural fingerprints skips
//!   lowering, optimization, and SQL rendering for repeated query shapes.
//! - Query execution entry points accept any connection-like value, so
//!   application code runs the same builder against a pool, a single
//!   connection, or an open transaction.
//!
//! The implementation lands with the first end-to-end execution milestone;
//! this crate currently pins the workspace layout and dependency direction.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

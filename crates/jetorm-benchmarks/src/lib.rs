//! JetORM benchmark suite.
//!
//! This crate exists only for its `benches/` targets; every dependency is a
//! dev-dependency and nothing here is published. Run with `cargo xtask
//! bench` or `just bench`.
//!
//! Both groups measure in-process query construction only — builder through
//! to SQL text plus bind values, no database. Against a real round trip
//! (hundreds of microseconds at best) this whole path is noise; the numbers
//! exist to keep JetORM's overhead bounded and honest, not because they
//! dominate application latency.
//!
//! Interpreting results honestly:
//!
//! - **`cold_build`** measures the first execution of a query shape: the
//!   plan-cache miss path, exactly as the executor runs it (one lowering,
//!   one verification, one rendering, one cache insert). JetORM does
//!   strictly more work here than sea-query's string assembly — it builds
//!   and verifies a typed IR module — so JetORM is expected to lose this
//!   group. Each shape pays it once per process.
//! - **`warm_repeat`** measures the realistic server steady state: one
//!   query shape executed repeatedly with changing values. JetORM resolves
//!   the statement through the shape-keyed plan cache; SeaORM and Diesel
//!   have no equivalent and rebuild the SQL text every call.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

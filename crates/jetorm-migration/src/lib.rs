//! Migration runtime for JetORM.
//!
//! This crate applies, reverts, and replays migrations. It is a library
//! first: applications can embed their migration set and run it at startup,
//! and the `jet` CLI drives the same code paths interactively.
//!
//! # Planned design
//!
//! - Migration files pair a typed change set from `jetorm-schema` with the
//!   rendered SQL, so reviews see both intent and effect; a raw SQL escape
//!   hatch is a first-class migration step.
//! - Applied history is tracked in a metadata table; replaying history
//!   reconstructs the expected schema for diffing and drift checks.
//!
//! The implementation lands with the migration milestone; this crate
//! currently pins the workspace layout and dependency direction.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

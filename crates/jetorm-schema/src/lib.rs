//! Abstract schema model and diff engine for JetORM.
//!
//! This crate defines the database-independent description of a schema —
//! tables, columns, keys, and indexes — and computes the change set between
//! two schema states. It is shared by migration generation (`jet migrate
//! generate`), drift checking (`jet check`), and database introspection
//! (`jet db pull`), so it lives outside the CLI.
//!
//! # Planned design
//!
//! - JetORM is code-first: entity metadata from `jetorm-entity` is the source
//!   of truth, and the expected database state is reconstructed by replaying
//!   migration history.
//! - The differ emits typed change operations (`AddColumn`, `DropTable`,
//!   `AlterColumnType`, ...) with destructive changes flagged for explicit
//!   confirmation and rename detection surfaced as questions, not guesses.
//!
//! The implementation lands with the migration milestone; this crate
//! currently pins the workspace layout and dependency direction.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

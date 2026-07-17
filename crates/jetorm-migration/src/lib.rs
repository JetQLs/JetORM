//! Migration runtime for JetORM.
//!
//! This crate applies, reverts, and replays migrations. It is a library
//! first: an application can embed its migration set and run it at startup,
//! and the `jet` CLI drives the same code paths interactively.
//!
//! # Declarative migrations
//!
//! A migration is data, not code: an ordered list of [`MigrationStep`]s
//! serialized to TOML. SQL is rendered from those steps at apply time rather
//! than stored, so the file and the statements it runs cannot drift apart.
//!
//! That choice splits the tooling cleanly. Generating a migration needs the
//! entity metadata, which only exists in compiled user code; applying,
//! reverting, and inspecting one needs nothing but the files and a
//! connection.
//!
//! # Replay
//!
//! Applying a migration set to an empty [`jetorm_schema::SchemaSet`]
//! reconstructs the schema the database should have — the "current" side of
//! the code-first diff, and the baseline a drift check compares against.
//! Raw SQL steps declare their own effect on that model ([`MigrationStep::Sql`]),
//! so an escape hatch cannot silently desynchronize replay.
//!
//! # Transactions
//!
//! PostgreSQL applies DDL transactionally, so each migration and its history
//! row commit together: a failed migration leaves neither a half-changed
//! schema nor a history row claiming success.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod history;
mod migration;
mod migrator;

pub use error::MigrationError;
pub use history::{AppliedMigration, HISTORY_TABLE};
pub use migration::{Migration, MigrationSet, MigrationStep};
pub use migrator::{MigrationState, MigrationStatus, Migrator};

//! `jet` — the JetORM command-line interface.
//!
//! Planned commands:
//!
//! - `jet migrate generate <name>` — diff entity metadata against replayed
//!   migration history and generate a reviewed migration file.
//! - `jet migrate up | down | status` — apply, revert, and inspect migrations.
//! - `jet db pull` — introspect an existing database into entity code.
//! - `jet check` — CI drift check between entities, migrations, and the live
//!   database.
//!
//! Interactive sessions get a `ratatui` + `crossterm` TUI (diff review,
//! status dashboards); every command also runs non-interactively with plain
//! output and exit codes for CI. All logic lives in the library crates; this
//! binary only parses arguments and renders.

fn main() {
    eprintln!("jet: commands land with the migration milestone; see crate docs for the plan");
    std::process::exit(2);
}

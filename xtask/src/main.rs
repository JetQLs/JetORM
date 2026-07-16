//! Workspace automation tasks.
//!
//! Implemented in Rust so every task runs identically on Windows, macOS, and
//! Linux without a shell dependency. The `justfile` at the workspace root
//! provides short aliases for the same tasks.
//!
//! ```text
//! cargo xtask test        # unit and integration tests (no database)
//! cargo xtask lint        # rustfmt check + clippy with warnings denied
//! cargo xtask test-live   # live `--ignored` tests; each starts its own
//!                         # disposable database container (needs Docker)
//! cargo xtask db-up       # start the manual development database
//! cargo xtask db-down     # stop it and discard its data
//! cargo xtask ci          # lint + test
//! ```

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const COMPOSE_FILE: &str = "docker-compose.test.yml";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("test") => cargo(&["test", "--workspace"], &[]),
        Some("lint") => lint(),
        Some("db-up") => compose(&["up", "-d", "--wait"]),
        Some("db-down") => compose(&["down", "-v"]),
        Some("test-live") => test_live(),
        Some("ci") => lint().and_then(|()| cargo(&["test", "--workspace"], &[])),
        Some(other) => Err(format!("unknown task {other:?}\n\n{HELP}")),
        None => {
            println!("{HELP}");
            return ExitCode::SUCCESS;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("xtask: {message}");
            ExitCode::FAILURE
        }
    }
}

const HELP: &str = "\
Usage: cargo xtask <task>

Tasks:
  test        Run unit and integration tests (no database required)
  lint        rustfmt check + clippy with warnings denied
  test-live   Run live `--ignored` tests; each starts its own disposable
              database container through testcontainers (needs Docker)
  db-up       Start the manual development database (docker compose)
  db-down     Stop it and discard its data
  ci          lint + test";

/// Runs every live test. Container lifecycles are owned by the tests
/// themselves through `testcontainers`, so no orchestration happens here.
fn test_live() -> Result<(), String> {
    cargo(&["test", "--workspace", "--", "--ignored"], &[])
}

fn lint() -> Result<(), String> {
    cargo(&["fmt", "--all", "--check"], &[])?;
    cargo(
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        &[],
    )
}

fn cargo(args: &[&str], envs: &[(&str, &str)]) -> Result<(), String> {
    run(Command::new("cargo").args(args).envs(envs.iter().copied()))
}

fn compose(args: &[&str]) -> Result<(), String> {
    run(Command::new("docker")
        .args(["compose", "-f", COMPOSE_FILE])
        .args(args))
}

fn run(command: &mut Command) -> Result<(), String> {
    command.current_dir(workspace_root());
    let rendered = format!("{command:?}");
    let status = command
        .status()
        .map_err(|error| format!("failed to start {rendered}: {error}"))?;
    if !status.success() {
        return Err(format!("{rendered} exited with {status}"));
    }
    Ok(())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

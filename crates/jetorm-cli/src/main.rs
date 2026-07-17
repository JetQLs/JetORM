//! `jet` — JetORM's migration command line.
//!
//! Every command is non-interactive and scriptable: state-changing commands
//! demand their acknowledgement flags up front instead of prompting, and
//! `jet check` reports through its exit code so CI pipelines need no output
//! parsing. The target schema comes from a serialized `SchemaSet` the
//! application emits (`SchemaSet` implements `serde`), which keeps this
//! binary independent from user code.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use jetorm_executor::Database;
use jetorm_migration::{Migration, MigrationSet, MigrationState, MigrationStep, Migrator};
use jetorm_schema::{RenameCandidate, SchemaSet, diff};

#[derive(Parser)]
#[command(name = "jet", version, about = "JetORM migration tooling")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage schema migrations.
    #[command(subcommand)]
    Migrate(MigrateCommand),
    /// Verify the database, the migration files, and the target schema
    /// agree; exits 1 on any disagreement.
    Check(CheckArgs),
}

#[derive(Subcommand)]
enum MigrateCommand {
    /// Show every migration and whether it is applied.
    Status(ConnectionArgs),
    /// Apply pending migrations in order.
    Up(UpArgs),
    /// Revert the most recent applied migrations.
    Down(DownArgs),
    /// Diff the target schema against the migration files and write the
    /// change set as a new migration.
    Generate(GenerateArgs),
}

#[derive(Args)]
struct ConnectionArgs {
    /// PostgreSQL connection URL; falls back to $DATABASE_URL.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    /// Directory holding the migration files.
    #[arg(long, default_value = "migrations")]
    dir: PathBuf,
}

#[derive(Args)]
struct UpArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Apply at most this many pending migrations.
    #[arg(long)]
    count: Option<usize>,
    /// Acknowledge migrations whose steps can lose data or fail on
    /// populated tables.
    #[arg(long)]
    allow_destructive: bool,
}

#[derive(Args)]
struct DownArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Revert exactly this many applied migrations.
    #[arg(long)]
    count: usize,
    /// Acknowledge that reverting undoes schema changes.
    #[arg(long)]
    yes: bool,
}

#[derive(Args)]
struct GenerateArgs {
    /// Name of the migration, appended to the generated version.
    name: String,
    /// Serialized target `SchemaSet` (TOML) emitted by the application.
    #[arg(long)]
    schema: PathBuf,
    /// Directory holding the migration files.
    #[arg(long, default_value = "migrations")]
    dir: PathBuf,
    /// Acknowledge generated changes that can lose data or fail on
    /// populated tables.
    #[arg(long)]
    allow_destructive: bool,
}

#[derive(Args)]
struct CheckArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Serialized target `SchemaSet` (TOML); when given, drift between the
    /// migrations and the target is also checked.
    #[arg(long)]
    schema: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("jet: cannot start async runtime: {error}");
            return ExitCode::from(2);
        }
    };
    let result = runtime.block_on(async {
        match cli.command {
            Command::Migrate(MigrateCommand::Status(args)) => status(args).await,
            Command::Migrate(MigrateCommand::Up(args)) => up(args).await,
            Command::Migrate(MigrateCommand::Down(args)) => down(args).await,
            Command::Migrate(MigrateCommand::Generate(args)) => generate(&args),
            Command::Check(args) => check(args).await,
        }
    });
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("jet: {message}");
            ExitCode::from(2)
        }
    }
}

async fn connect(args: &ConnectionArgs) -> Result<(Database, MigrationSet), String> {
    let migrations = MigrationSet::from_directory(&args.dir).map_err(|error| {
        format!(
            "cannot load migrations from {}: {error}",
            args.dir.display()
        )
    })?;
    let database = Database::connect(&args.database_url)
        .await
        .map_err(|error| format!("cannot connect: {error}"))?;
    Ok((database, migrations))
}

async fn status(args: ConnectionArgs) -> Result<ExitCode, String> {
    let (database, migrations) = connect(&args).await?;
    let migrator = Migrator::new(&database, &migrations);
    migrator
        .install()
        .await
        .map_err(|error| error.to_string())?;
    let statuses = migrator.status().await.map_err(|error| error.to_string())?;
    if statuses.is_empty() {
        println!("no migrations in {}", args.dir.display());
        return Ok(ExitCode::SUCCESS);
    }
    for status in &statuses {
        let marker = match status.state() {
            MigrationState::Applied => "applied",
            MigrationState::Pending => "pending",
        };
        println!("{marker:>8}  {}", status.version());
    }
    Ok(ExitCode::SUCCESS)
}

async fn up(args: UpArgs) -> Result<ExitCode, String> {
    let (database, migrations) = connect(&args.connection).await?;
    let migrator = Migrator::new(&database, &migrations);
    migrator
        .install()
        .await
        .map_err(|error| error.to_string())?;

    let pending = migrator
        .pending()
        .await
        .map_err(|error| error.to_string())?;
    let planned: Vec<&Migration> = match args.count {
        Some(count) => pending.iter().copied().take(count).collect(),
        None => pending,
    };
    if planned.is_empty() {
        println!("nothing to apply");
        return Ok(ExitCode::SUCCESS);
    }
    let destructive: Vec<&str> = planned
        .iter()
        .filter(|migration| migration.is_destructive())
        .map(|migration| migration.version())
        .collect();
    if !destructive.is_empty() && !args.allow_destructive {
        eprintln!(
            "refusing to apply destructive migrations without --allow-destructive: {}",
            destructive.join(", ")
        );
        return Ok(ExitCode::FAILURE);
    }

    let applied = migrator
        .up(args.count)
        .await
        .map_err(|error| error.to_string())?;
    for version in &applied {
        println!(" applied  {version}");
    }
    Ok(ExitCode::SUCCESS)
}

async fn down(args: DownArgs) -> Result<ExitCode, String> {
    if !args.yes {
        eprintln!("refusing to revert migrations without --yes");
        return Ok(ExitCode::FAILURE);
    }
    let (database, migrations) = connect(&args.connection).await?;
    let migrator = Migrator::new(&database, &migrations);
    migrator
        .install()
        .await
        .map_err(|error| error.to_string())?;

    // A migration without down steps cannot be reverted: removing its
    // history row while its changes stay applied would corrupt replay.
    let applied = migrator
        .applied()
        .await
        .map_err(|error| error.to_string())?;
    let irreversible: Vec<&str> = applied
        .iter()
        .rev()
        .take(args.count)
        .filter_map(|entry| migrations.get(entry.version()))
        .filter(|migration| !migration.is_reversible())
        .map(|migration| migration.version())
        .collect();
    if !irreversible.is_empty() {
        eprintln!(
            "refusing to revert migrations without down steps: {}",
            irreversible.join(", ")
        );
        return Ok(ExitCode::FAILURE);
    }

    let reverted = migrator
        .down(args.count)
        .await
        .map_err(|error| error.to_string())?;
    if reverted.is_empty() {
        println!("nothing to revert");
        return Ok(ExitCode::SUCCESS);
    }
    for version in &reverted {
        println!("reverted  {version}");
    }
    Ok(ExitCode::SUCCESS)
}

fn generate(args: &GenerateArgs) -> Result<ExitCode, String> {
    let target = load_schema(&args.schema)?;
    let migrations = MigrationSet::from_directory(&args.dir).map_err(|error| {
        format!(
            "cannot load migrations from {}: {error}",
            args.dir.display()
        )
    })?;
    let current = migrations.replay().map_err(|error| error.to_string())?;

    let changes = diff(&current, &target);
    if changes.is_empty() {
        println!("schema is up to date; nothing to generate");
        return Ok(ExitCode::SUCCESS);
    }

    for candidate in changes.rename_candidates() {
        match candidate {
            RenameCandidate::Table { from, to } => eprintln!(
                "note: {from} -> {to} may be a rename; the generated migration \
                 drops and recreates it, edit the file to confirm the rename"
            ),
            RenameCandidate::Column { table, from, to } => eprintln!(
                "note: {table}.{from} -> {to} may be a rename; the generated \
                 migration drops and re-adds it, edit the file to confirm the rename"
            ),
        }
    }
    let destructive: Vec<String> = changes
        .changes()
        .iter()
        .filter(|change| change.is_destructive())
        .map(ToString::to_string)
        .collect();
    if !destructive.is_empty() && !args.allow_destructive {
        eprintln!("refusing to generate destructive changes without --allow-destructive:");
        for change in &destructive {
            eprintln!("  {change}");
        }
        return Ok(ExitCode::FAILURE);
    }

    let version = next_version(&migrations, &args.name);
    let up: Vec<MigrationStep> = changes
        .changes()
        .iter()
        .cloned()
        .map(MigrationStep::Change)
        .collect();
    // Reversal is not derivable in general (a dropped column's data is
    // gone), so the down steps start empty for the author to fill in.
    let migration = Migration::new(version.clone(), up, Vec::new());
    let contents = migration.to_toml().map_err(|error| error.to_string())?;
    let path = args.dir.join(format!("{version}.toml"));
    std::fs::write(&path, contents)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    println!(
        "wrote {} with {} change(s)",
        path.display(),
        changes.changes().len()
    );
    Ok(ExitCode::SUCCESS)
}

async fn check(args: CheckArgs) -> Result<ExitCode, String> {
    let (database, migrations) = connect(&args.connection).await?;
    let migrator = Migrator::new(&database, &migrations);
    migrator
        .install()
        .await
        .map_err(|error| error.to_string())?;

    let mut clean = true;
    let pending = migrator
        .pending()
        .await
        .map_err(|error| error.to_string())?;
    if !pending.is_empty() {
        clean = false;
        for migration in &pending {
            println!(" pending  {}", migration.version());
        }
    }

    if let Some(schema) = &args.schema {
        let target = load_schema(schema)?;
        let replayed = migrations.replay().map_err(|error| error.to_string())?;
        let drift = diff(&replayed, &target);
        if !drift.is_empty() {
            clean = false;
            for change in drift.changes() {
                println!("   drift  {change}");
            }
        }
    }

    if clean {
        println!("in sync");
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

fn load_schema(path: &Path) -> Result<SchemaSet, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    toml::from_str(&contents)
        .map_err(|error| format!("{} is not a schema: {error}", path.display()))
}

/// Builds the next version identifier: a zero-padded ordinal followed by
/// the given name, which sorts after every existing version.
fn next_version(migrations: &MigrationSet, name: &str) -> String {
    let next = migrations.len() + 1;
    let slug: String = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("{next:04}_{slug}")
}

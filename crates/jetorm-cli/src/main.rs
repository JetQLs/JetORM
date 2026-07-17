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

mod codegen;
mod ui;

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
    /// Inspect a live database.
    #[command(subcommand)]
    Db(DbCommand),
    /// Verify the database, the migration files, and the target schema
    /// agree; exits 1 on any disagreement.
    Check(CheckArgs),
    /// Open the interactive migration dashboard.
    Ui(UiArgs),
}

#[derive(Args)]
struct UiArgs {
    #[command(flatten)]
    connection: ConnectionArgs,
    /// Serialized target `SchemaSet` (TOML); enables drift review and
    /// rename confirmation.
    #[arg(long)]
    schema: Option<PathBuf>,
}

#[derive(Subcommand)]
enum DbCommand {
    /// Read the live schema and generate entity code from it.
    Pull(PullArgs),
}

#[derive(Args)]
struct PullArgs {
    /// PostgreSQL connection URL; falls back to $DATABASE_URL.
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    /// Database schema to read.
    #[arg(long, default_value = "public")]
    db_schema: String,
    /// File receiving the generated entities.
    #[arg(long, default_value = "entities.rs")]
    out: PathBuf,
    /// Also write the pulled schema as a serialized `SchemaSet` — the
    /// target-schema file `generate` and `check` consume.
    #[arg(long)]
    schema_out: Option<PathBuf>,
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
            Command::Db(DbCommand::Pull(args)) => pull(args).await,
            Command::Check(args) => check(args).await,
            Command::Ui(args) => tui(args).await,
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

    // Applying exactly the vetted plan closes the review-to-apply gap:
    // if another process changes the pending set in between, this fails
    // instead of applying something never vetted.
    let plan: Vec<String> = planned
        .iter()
        .map(|migration| migration.version().to_owned())
        .collect();
    let applied = migrator
        .up_versions(&plan)
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
    if path.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite a migration",
            path.display()
        ));
    }
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

async fn tui(args: UiArgs) -> Result<ExitCode, String> {
    let database = Database::connect(&args.connection.database_url)
        .await
        .map_err(|error| format!("cannot connect: {error}"))?;
    let schema = match &args.schema {
        Some(path) => Some(load_schema(path)?),
        None => None,
    };
    ui::run(&database, args.connection.dir, schema).await?;
    Ok(ExitCode::SUCCESS)
}

async fn pull(args: PullArgs) -> Result<ExitCode, String> {
    let database = Database::connect(&args.database_url)
        .await
        .map_err(|error| format!("cannot connect: {error}"))?;
    let introspection = jetorm_migration::introspect(&database, &args.db_schema)
        .await
        .map_err(|error| format!("introspection failed: {error}"))?;

    for skipped in &introspection.skipped {
        eprintln!(
            "warning: skipped {}.{} ({}); it will be absent from the entities",
            skipped.table, skipped.column, skipped.data_type
        );
    }
    if introspection.schema.is_empty() {
        println!("schema {} has no tables", args.db_schema);
        return Ok(ExitCode::SUCCESS);
    }

    let source = codegen::entities_source(&introspection.schema);
    std::fs::write(&args.out, source)
        .map_err(|error| format!("cannot write {}: {error}", args.out.display()))?;
    println!(
        "wrote {} with {} entities",
        args.out.display(),
        introspection.schema.len()
    );

    if let Some(schema_out) = &args.schema_out {
        let serialized = toml::to_string_pretty(&introspection.schema)
            .map_err(|error| format!("schema does not serialize: {error}"))?;
        std::fs::write(schema_out, serialized)
            .map_err(|error| format!("cannot write {}: {error}", schema_out.display()))?;
        println!("wrote {}", schema_out.display());
    }
    Ok(ExitCode::SUCCESS)
}

fn load_schema(path: &Path) -> Result<SchemaSet, String> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    toml::from_str(&contents)
        .map_err(|error| format!("{} is not a schema: {error}", path.display()))
}

/// Builds the next version identifier: a zero-padded ordinal followed by
/// the given name, which sorts after every existing version.
///
/// The ordinal is one past the highest existing ordinal — not the file
/// count — so deleted or squashed migrations leave gaps rather than
/// causing a later generate to reuse (and mis-sort against) an applied
/// version.
fn next_version(migrations: &MigrationSet, name: &str) -> String {
    let highest = migrations
        .migrations()
        .iter()
        .filter_map(|migration| {
            let digits: String = migration
                .version()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            digits.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0);
    let next = highest + 1;
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

#[cfg(test)]
mod tests {
    use jetorm_migration::{Migration, MigrationSet};

    use super::next_version;

    fn set_of(versions: &[&str]) -> MigrationSet {
        MigrationSet::new(
            versions
                .iter()
                .map(|version| Migration::new(*version, Vec::new(), Vec::new()))
                .collect(),
        )
        .expect("distinct versions")
    }

    #[test]
    fn next_version_advances_past_the_highest_ordinal() {
        assert_eq!(next_version(&set_of(&[]), "init"), "0001_init");
        assert_eq!(next_version(&set_of(&["0001_a", "0002_b"]), "c"), "0003_c");
        // A squash deleted 0002: the next version must not reuse or
        // mis-sort against the applied 0003.
        assert_eq!(
            next_version(&set_of(&["0001_a", "0003_c"]), "add_users"),
            "0004_add_users"
        );
    }

    #[test]
    fn names_slugify_into_version_identifiers() {
        assert_eq!(next_version(&set_of(&[]), "Add Users!"), "0001_add_users_");
    }
}

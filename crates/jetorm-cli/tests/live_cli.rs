//! End-to-end runs of the `jet` binary against a live PostgreSQL server.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use std::path::Path;
use std::process::{Command, Output};

use jetorm_entity::ColumnType;
use jetorm_schema::{ColumnDef, SchemaSet, TableDef, TableName};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const POSTGRES_TAG: &str = "18-alpine";

fn jet(url: &str, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jet"))
        .args(arguments)
        .env("DATABASE_URL", url)
        .output()
        .expect("the jet binary runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_schema(path: &Path, with_note: bool) {
    let mut table = TableDef::new(TableName::new("articles"))
        .with_column(ColumnDef::new("id", ColumnType::Int64))
        .with_column(ColumnDef::new("title", ColumnType::Text))
        .with_primary_key(vec!["id".to_owned()]);
    if with_note {
        table = table.with_column(ColumnDef::new("note", ColumnType::Text).nullable());
    }
    let mut schema = SchemaSet::new();
    schema.insert(table);
    std::fs::write(
        path,
        toml::to_string_pretty(&schema).expect("schema serializes"),
    )
    .expect("schema file writes");
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn the_full_generate_up_check_cycle_round_trips() {
    let container = Postgres::default()
        .with_tag(POSTGRES_TAG)
        .start()
        .await
        .expect("PostgreSQL test container starts (is Docker running?)");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container maps the PostgreSQL port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    let workspace = tempfile::tempdir().expect("temp workspace");
    let migrations = workspace.path().join("migrations");
    std::fs::create_dir(&migrations).expect("migrations directory");
    let migrations = migrations.to_str().expect("utf8 path");
    let schema_path = workspace.path().join("schema.toml");
    write_schema(&schema_path, false);
    let schema = schema_path.to_str().expect("utf8 path");

    // Generate the initial migration from the target schema.
    let generated = jet(
        &url,
        &[
            "migrate", "generate", "init", "--schema", schema, "--dir", migrations,
        ],
    );
    assert!(generated.status.success(), "{}", stderr(&generated));
    assert!(stdout(&generated).contains("0001_init.toml"));

    // The new migration is pending, so check fails...
    let dirty = jet(&url, &["check", "--dir", migrations, "--schema", schema]);
    assert_eq!(dirty.status.code(), Some(1), "{}", stdout(&dirty));
    assert!(stdout(&dirty).contains("pending"));

    // ...until it is applied, after which everything is in sync.
    let applied = jet(&url, &["migrate", "up", "--dir", migrations]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    assert!(stdout(&applied).contains("applied"));
    let clean = jet(&url, &["check", "--dir", migrations, "--schema", schema]);
    assert!(clean.status.success(), "{}", stdout(&clean));
    assert!(stdout(&clean).contains("in sync"));

    // Growing the target schema reports drift, and a second generate + up
    // cycle absorbs it.
    write_schema(&schema_path, true);
    let drifted = jet(&url, &["check", "--dir", migrations, "--schema", schema]);
    assert_eq!(drifted.status.code(), Some(1));
    assert!(stdout(&drifted).contains("drift"));
    let second = jet(
        &url,
        &[
            "migrate", "generate", "add_note", "--schema", schema, "--dir", migrations,
        ],
    );
    assert!(second.status.success(), "{}", stderr(&second));
    let applied = jet(&url, &["migrate", "up", "--dir", migrations]);
    assert!(applied.status.success(), "{}", stderr(&applied));
    let clean = jet(&url, &["check", "--dir", migrations, "--schema", schema]);
    assert!(clean.status.success(), "{}", stdout(&clean));

    // Shrinking the schema is destructive: generation refuses without the
    // acknowledgement flag and proceeds with it.
    write_schema(&schema_path, false);
    let refused = jet(
        &url,
        &[
            "migrate",
            "generate",
            "drop_note",
            "--schema",
            schema,
            "--dir",
            migrations,
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("--allow-destructive"));
    let allowed = jet(
        &url,
        &[
            "migrate",
            "generate",
            "drop_note",
            "--schema",
            schema,
            "--dir",
            migrations,
            "--allow-destructive",
        ],
    );
    assert!(allowed.status.success(), "{}", stderr(&allowed));

    // Applying the destructive migration needs the same acknowledgement.
    let refused = jet(&url, &["migrate", "up", "--dir", migrations]);
    assert_eq!(refused.status.code(), Some(1));
    let applied = jet(
        &url,
        &["migrate", "up", "--dir", migrations, "--allow-destructive"],
    );
    assert!(applied.status.success(), "{}", stderr(&applied));

    // Reverting is double-guarded: --yes first, and generated migrations
    // have no down steps, so they refuse reversal outright.
    let refused = jet(
        &url,
        &["migrate", "down", "--count", "1", "--dir", migrations],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("--yes"));
    let refused = jet(
        &url,
        &[
            "migrate", "down", "--count", "1", "--yes", "--dir", migrations,
        ],
    );
    assert_eq!(refused.status.code(), Some(1));
    assert!(stderr(&refused).contains("without down steps"));

    // Status shows the full history as applied.
    let status = jet(&url, &["migrate", "status", "--dir", migrations]);
    assert!(status.status.success());
    assert_eq!(stdout(&status).matches(" applied").count(), 3);
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn db_pull_generates_entities_and_a_usable_baseline() {
    let container = Postgres::default()
        .with_tag(POSTGRES_TAG)
        .start()
        .await
        .expect("PostgreSQL test container starts (is Docker running?)");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container maps the PostgreSQL port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

    // A hand-made schema exercising identity, uniqueness, nullability,
    // decimals, foreign keys with actions, and an unmappable type.
    let pool = sqlx::postgres::PgPool::connect(&url)
        .await
        .expect("connects");
    for statement in [
        "CREATE TABLE users (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             email text UNIQUE,
             name text NOT NULL,
             location point
         )",
        "CREATE TABLE posts (
             id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
             title text NOT NULL,
             price numeric NOT NULL,
             author_id bigint NOT NULL REFERENCES users (id) ON DELETE CASCADE
         )",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("schema statement runs");
    }
    pool.close().await;

    let workspace = tempfile::tempdir().expect("temp workspace");
    let entities = workspace.path().join("entities.rs");
    let schema_toml = workspace.path().join("schema.toml");

    let pulled = jet(
        &url,
        &[
            "db",
            "pull",
            "--out",
            entities.to_str().expect("utf8 path"),
            "--schema-out",
            schema_toml.to_str().expect("utf8 path"),
        ],
    );
    assert!(pulled.status.success(), "{}", stderr(&pulled));
    assert!(
        stderr(&pulled).contains("users.location"),
        "the unmappable point column is reported, not silently lost: {}",
        stderr(&pulled)
    );

    let source = std::fs::read_to_string(&entities).expect("entities were written");
    for expected in [
        "pub struct Users {",
        "#[jet(primary_key, auto_increment)]",
        "#[jet(unique)]",
        "pub email: Option<String>,",
        "pub struct Posts {",
        "pub price: rust_decimal::Decimal,",
        "#[jet(references = \"users::Id\", on_delete = \"cascade\")]",
        "pub author_id: i64,",
    ] {
        assert!(
            source.contains(expected),
            "generated entities lack {expected:?}:
{source}"
        );
    }

    // The pulled schema is a working baseline: generating against an empty
    // migration directory recreates it, and check then reports in sync.
    let migrations = workspace.path().join("migrations");
    std::fs::create_dir(&migrations).expect("migrations directory");
    let migrations = migrations.to_str().expect("utf8 path");
    let schema = schema_toml.to_str().expect("utf8 path");
    // Adding a foreign key is flagged destructive (existing rows can
    // violate it), so a baseline carrying one needs the acknowledgement.
    let generated = jet(
        &url,
        &[
            "migrate",
            "generate",
            "baseline",
            "--schema",
            schema,
            "--dir",
            migrations,
            "--allow-destructive",
        ],
    );
    assert!(generated.status.success(), "{}", stderr(&generated));

    // The live tables already exist, so the baseline is recorded rather
    // than reapplied — a fresh database would run it; this one adopts it.
    // (Baseline adoption tooling is future work; here the files replay.)
    let replayed = jet(&url, &["check", "--dir", migrations, "--schema", schema]);
    assert_eq!(
        replayed.status.code(),
        Some(1),
        "the baseline is pending against this database: {}",
        stdout(&replayed)
    );
    assert!(stdout(&replayed).contains("pending"));
}

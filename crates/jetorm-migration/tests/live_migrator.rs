//! Drives the migrator against live PostgreSQL servers.
//!
//! Each test starts its own disposable container, so migration history never
//! leaks between tests. Run via `cargo xtask test-live` (needs Docker).

use jetorm_entity::ColumnType;
use jetorm_executor::Database;
use jetorm_executor::sqlx::{self, Row};
use jetorm_migration::{
    HISTORY_TABLE, Migration, MigrationError, MigrationSet, MigrationState, MigrationStep, Migrator,
};
use jetorm_schema::{ColumnDef, SchemaChange, TableDef, TableName};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

const POSTGRES_TAG: &str = "18-alpine";

async fn fresh_database() -> (ContainerAsync<Postgres>, Database) {
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
    let database = Database::connect(&url)
        .await
        .expect("connects to the test container");
    (container, database)
}

fn users_table() -> TableDef {
    TableDef::new(TableName::new("users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64).auto_increment())
        .with_column(ColumnDef::new("name", ColumnType::Text))
        .with_primary_key(vec!["id".to_owned()])
}

fn create_users() -> Migration {
    Migration::new(
        "20260717_100000_create_users",
        vec![MigrationStep::Change(SchemaChange::CreateTable(
            users_table(),
        ))],
        vec![MigrationStep::Change(SchemaChange::DropTable(
            TableName::new("users"),
        ))],
    )
}

fn add_email() -> Migration {
    Migration::new(
        "20260717_110000_add_email",
        vec![MigrationStep::Change(SchemaChange::AddColumn {
            table: TableName::new("users"),
            column: ColumnDef::new("email", ColumnType::Text).nullable(),
        })],
        vec![MigrationStep::Change(SchemaChange::DropColumn {
            table: TableName::new("users"),
            column: "email".to_owned(),
        })],
    )
}

async fn column_exists(db: &Database, table: &str, column: &str) -> bool {
    sqlx::query(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.columns
             WHERE table_name = $1 AND column_name = $2
         )",
    )
    .bind(table)
    .bind(column)
    .fetch_one(db.pool())
    .await
    .expect("catalog query runs")
    .get(0)
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn migrations_apply_record_and_revert() {
    let (_container, db) = fresh_database().await;
    let set = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");
    let migrator = Migrator::new(&db, &set);

    // Everything is pending against an empty database.
    let status = migrator.status().await.expect("status reads");
    assert_eq!(status.len(), 2);
    assert!(
        status
            .iter()
            .all(|entry| entry.state() == MigrationState::Pending)
    );

    let applied = migrator.up(None).await.expect("migrations apply");
    assert_eq!(
        applied,
        ["20260717_100000_create_users", "20260717_110000_add_email"],
        "migrations must apply oldest first"
    );
    assert!(column_exists(&db, "users", "email").await);

    // History records both, and status agrees with the database.
    let status = migrator.status().await.expect("status reads");
    assert!(
        status
            .iter()
            .all(|entry| entry.state() == MigrationState::Applied)
    );
    assert!(status[0].applied_at().is_some());

    // Re-running is a no-op rather than an error.
    assert!(
        migrator.up(None).await.expect("re-run succeeds").is_empty(),
        "an up with nothing pending must apply nothing"
    );

    // Reverting walks newest first.
    let reverted = migrator.down(1).await.expect("revert succeeds");
    assert_eq!(reverted, ["20260717_110000_add_email"]);
    assert!(!column_exists(&db, "users", "email").await);

    let status = migrator.status().await.expect("status reads");
    assert_eq!(status[0].state(), MigrationState::Applied);
    assert_eq!(status[1].state(), MigrationState::Pending);

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn a_step_limit_applies_only_that_many() {
    let (_container, db) = fresh_database().await;
    let set = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");
    let migrator = Migrator::new(&db, &set);

    let applied = migrator.up(Some(1)).await.expect("one migration applies");
    assert_eq!(applied, ["20260717_100000_create_users"]);
    assert!(!column_exists(&db, "users", "email").await);

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn a_failed_migration_leaves_no_trace() {
    // PostgreSQL applies DDL transactionally, so a migration that fails
    // partway must leave neither a half-changed schema nor a history row.
    let (_container, db) = fresh_database().await;
    let broken = Migration::new(
        "20260717_100000_broken",
        vec![
            // Succeeds.
            MigrationStep::Change(SchemaChange::CreateTable(users_table())),
            // Fails: the column type is fine but the table does not exist.
            MigrationStep::Change(SchemaChange::AddColumn {
                table: TableName::new("does_not_exist"),
                column: ColumnDef::new("x", ColumnType::Int64).nullable(),
            }),
        ],
        Vec::new(),
    );
    let set = MigrationSet::new(vec![broken]).expect("set is structurally valid");
    let migrator = Migrator::new(&db, &set);

    migrator
        .up(None)
        .await
        .expect_err("the failing migration must be reported");

    let table_exists: bool = sqlx::query(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.tables WHERE table_name = 'users'
         )",
    )
    .fetch_one(db.pool())
    .await
    .expect("catalog query runs")
    .get(0);
    assert!(
        !table_exists,
        "the successful step must roll back with the failing one"
    );

    let applied = migrator.applied().await.expect("history reads");
    assert!(
        applied.is_empty(),
        "a failed migration must not be recorded as applied"
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn installing_history_is_idempotent_and_uses_jetorm_ddl() {
    let (_container, db) = fresh_database().await;
    let set = MigrationSet::new(Vec::new()).expect("empty set is valid");
    let migrator = Migrator::new(&db, &set);

    migrator.install().await.expect("first install succeeds");
    migrator
        .install()
        .await
        .expect("installing an existing history table is a no-op");

    let exists: bool = sqlx::query(
        "SELECT EXISTS (
             SELECT 1 FROM information_schema.tables WHERE table_name = $1
         )",
    )
    .bind(HISTORY_TABLE)
    .fetch_one(db.pool())
    .await
    .expect("catalog query runs")
    .get(0);
    assert!(exists, "the history table must be created");

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn an_orphaned_history_row_is_reported_rather_than_ignored() {
    // Deleting an applied migration's file leaves history naming something
    // that no longer exists; the schema can no longer be reconstructed, so
    // the migrator must refuse rather than guess.
    let (_container, db) = fresh_database().await;
    let full = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");
    Migrator::new(&db, &full)
        .up(None)
        .await
        .expect("migrations apply");

    let truncated = MigrationSet::new(vec![create_users()]).expect("set is valid");
    let error = Migrator::new(&db, &truncated)
        .status()
        .await
        .expect_err("an orphaned history row must be reported");
    assert!(
        matches!(&error, MigrationError::UnknownAppliedVersion { version }
            if version == "20260717_110000_add_email"),
        "unexpected error: {error:?}"
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn replaying_applied_history_matches_the_live_database() {
    let (_container, db) = fresh_database().await;
    let set = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");
    let migrator = Migrator::new(&db, &set);
    migrator.up(Some(1)).await.expect("one migration applies");

    // Replay reflects what is applied, not what exists on disk.
    let replayed = migrator.replay_applied().await.expect("history replays");
    let users = replayed
        .table(&TableName::new("users"))
        .expect("users exists in the replayed schema");
    assert!(users.column("name").is_some());
    assert!(
        users.column("email").is_none(),
        "replay must exclude the pending migration"
    );
    assert_eq!(
        column_exists(&db, "users", "email").await,
        users.column("email").is_some(),
        "the replayed model must agree with the live database"
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn irreversible_migrations_refuse_to_revert() {
    let (_container, db) = fresh_database().await;
    let one_way = Migration::new(
        "20260717_100000_one_way",
        vec![MigrationStep::Change(SchemaChange::CreateTable(
            users_table(),
        ))],
        Vec::new(),
    );
    let set = MigrationSet::new(vec![one_way]).expect("set is valid");
    let migrator = Migrator::new(&db, &set);
    migrator.up(None).await.expect("migration applies");

    let error = migrator
        .down(1)
        .await
        .expect_err("an irreversible migration must refuse");
    assert!(matches!(error, MigrationError::InvalidVersion { .. }));

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn raw_sql_steps_execute_verbatim() {
    let (_container, db) = fresh_database().await;
    let set = MigrationSet::new(vec![
        create_users(),
        Migration::new(
            "20260717_120000_raw",
            vec![MigrationStep::Sql {
                sql: "CREATE INDEX users_name_idx ON users (name)".to_owned(),
                state: Vec::new(),
            }],
            vec![MigrationStep::Sql {
                sql: "DROP INDEX users_name_idx".to_owned(),
                state: Vec::new(),
            }],
        ),
    ])
    .expect("set is valid");
    let migrator = Migrator::new(&db, &set);
    migrator.up(None).await.expect("migrations apply");

    let index_exists: bool =
        sqlx::query("SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'users_name_idx')")
            .fetch_one(db.pool())
            .await
            .expect("catalog query runs")
            .get(0);
    assert!(index_exists, "the raw escape hatch must reach the database");

    migrator.down(1).await.expect("raw step reverts");
    db.close().await;
}

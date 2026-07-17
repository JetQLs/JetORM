//! Executes rendered DDL against live PostgreSQL servers.
//!
//! Golden-string DDL tests prove spelling; these tests prove the statements
//! are actually accepted and behave — identity columns generate values,
//! `USING` casts convert, constraint names resolve. Run via
//! `cargo xtask test-live` (needs a Docker daemon).

use jetorm_dialect::postgres::ddl::render_change;
use jetorm_entity::ColumnType;
use jetorm_executor::Database;
use jetorm_schema::SchemaSet;
use jetorm_schema::{ColumnDef, SchemaChange, TableDef, TableName, diff};
use sqlx::Row;
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

async fn execute_change(db: &Database, change: &SchemaChange) {
    for statement in render_change(change).expect("change renders as DDL") {
        sqlx::query(&statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL rejected {statement:?}: {error}"));
    }
}

fn users_table() -> TableDef {
    TableDef::new(TableName::new("ddl_users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64).auto_increment())
        .with_column(ColumnDef::new("name", ColumnType::Text))
        .with_column(
            ColumnDef::new("email", ColumnType::Text)
                .nullable()
                .unique(),
        )
        .with_primary_key(vec!["id".to_owned()])
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn rendered_ddl_round_trips_against_live_postgres() {
    let (_container, db) = fresh_database().await;

    // Create the table and prove the identity column generates keys.
    execute_change(&db, &SchemaChange::CreateTable(users_table())).await;
    let generated: i64 = sqlx::query(
        "INSERT INTO ddl_users (name, email) VALUES ('alice', 'alice@example.com') RETURNING id",
    )
    .fetch_one(db.pool())
    .await
    .expect("identity column generates a key")
    .get(0);
    assert_eq!(generated, 1);

    // Walk a realistic evolution, every step through rendered DDL.
    let table = TableName::new("ddl_users");
    let steps = [
        SchemaChange::AddColumn {
            table: table.clone(),
            column: ColumnDef::new("bio", ColumnType::Text).nullable(),
        },
        SchemaChange::RenameColumn {
            table: table.clone(),
            from: "name".to_owned(),
            to: "full_name".to_owned(),
        },
        // The column only holds NULLs, so the USING cast must succeed.
        SchemaChange::AlterColumnType {
            table: table.clone(),
            column: "bio".to_owned(),
            from: ColumnType::Text,
            to: ColumnType::Json,
        },
        SchemaChange::SetUnique {
            table: table.clone(),
            column: "full_name".to_owned(),
            unique: true,
        },
        SchemaChange::SetUnique {
            table: table.clone(),
            column: "full_name".to_owned(),
            unique: false,
        },
        SchemaChange::SetPrimaryKey {
            table: table.clone(),
            from: vec!["id".to_owned()],
            to: vec!["id".to_owned(), "full_name".to_owned()],
        },
        SchemaChange::SetNullable {
            table: table.clone(),
            column: "email".to_owned(),
            nullable: false,
        },
        SchemaChange::DropColumn {
            table: table.clone(),
            column: "bio".to_owned(),
        },
        SchemaChange::RenameTable {
            from: table,
            to: TableName::new("ddl_people"),
        },
    ];
    for change in &steps {
        execute_change(&db, change).await;
    }

    // The evolved table still answers queries with the data intact.
    let row = sqlx::query("SELECT full_name, email FROM ddl_people WHERE id = 1")
        .fetch_one(db.pool())
        .await
        .expect("evolved table is queryable");
    let full_name: String = row.get(0);
    let email: String = row.get(1);
    assert_eq!(full_name, "alice");
    assert_eq!(email, "alice@example.com");

    execute_change(&db, &SchemaChange::DropTable(TableName::new("ddl_people"))).await;
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn entity_diff_bootstraps_a_queryable_database() {
    // The full code-first path: empty database + entity-shaped target
    // schema -> diff -> rendered DDL -> live PostgreSQL.
    let (_container, db) = fresh_database().await;

    let mut target = SchemaSet::new();
    target.insert(users_table());
    let changes = diff(&SchemaSet::new(), &target);
    assert!(!changes.has_destructive_changes());
    for change in changes.changes() {
        execute_change(&db, change).await;
    }

    sqlx::query("INSERT INTO ddl_users (name) VALUES ('bob')")
        .execute(db.pool())
        .await
        .expect("bootstrapped table accepts rows");
    let count: i64 = sqlx::query("SELECT count(*) FROM ddl_users")
        .fetch_one(db.pool())
        .await
        .expect("bootstrapped table is queryable")
        .get(0);
    assert_eq!(count, 1);

    db.close().await;
}

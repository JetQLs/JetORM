use std::path::Path;

use jetorm_entity::ColumnType;
use jetorm_migration::{Migration, MigrationError, MigrationSet, MigrationStep};
use jetorm_schema::{ColumnDef, SchemaChange, SchemaSet, TableDef, TableName, diff};

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

#[test]
fn replaying_a_set_reconstructs_the_schema_it_describes() {
    let set = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");
    let schema = set.replay().expect("set replays");

    let table = schema
        .table(&TableName::new("users"))
        .expect("users table exists after replay");
    assert!(table.column("id").is_some());
    assert!(table.column("name").is_some());
    assert!(
        table
            .column("email")
            .expect("email was added")
            .is_nullable(),
        "the second migration's column must survive replay"
    );
}

#[test]
fn replay_is_the_baseline_the_differ_compares_against() {
    // The code-first loop: replay history, diff entity-shaped target against
    // it, and get exactly the changes the next migration should contain.
    let set = MigrationSet::new(vec![create_users()]).expect("set is valid");
    let current = set.replay().expect("set replays");

    let mut target = SchemaSet::new();
    target.insert(users_table().with_column(ColumnDef::new("email", ColumnType::Text).nullable()));

    let changes = diff(&current, &target);
    assert_eq!(changes.changes(), add_email().up()[0].state_changes());
    assert!(!changes.has_destructive_changes());
}

#[test]
fn replay_stops_at_a_prefix() {
    let set = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");

    let after_first = set.replay_through(1).expect("prefix replays");
    assert!(
        after_first
            .table(&TableName::new("users"))
            .expect("table exists")
            .column("email")
            .is_none(),
        "a prefix replay must not include later migrations"
    );

    assert!(
        set.replay_through(0)
            .expect("empty prefix replays")
            .is_empty(),
        "replaying nothing yields an empty schema"
    );
}

#[test]
fn replaying_applied_versions_skips_pending_ones() {
    let set = MigrationSet::new(vec![create_users(), add_email()]).expect("set is valid");
    let schema = set
        .replay_applied(&["20260717_100000_create_users".to_owned()])
        .expect("applied subset replays");
    assert!(
        schema
            .table(&TableName::new("users"))
            .expect("table exists")
            .column("email")
            .is_none()
    );
}

#[test]
fn history_naming_a_missing_migration_is_an_error() {
    // Deleting an applied migration file orphans its history row, and the
    // schema the database is in can no longer be reconstructed.
    let set = MigrationSet::new(vec![create_users()]).expect("set is valid");
    let error = set
        .replay_applied(&["20260717_999999_deleted".to_owned()])
        .expect_err("an orphaned history row must be reported");
    assert!(matches!(
        error,
        MigrationError::UnknownAppliedVersion { .. }
    ));
}

#[test]
fn an_internally_inconsistent_set_is_reported_with_its_version() {
    // The second migration adds a column to a table that does not exist.
    let set = MigrationSet::new(vec![Migration::new(
        "20260717_100000_broken",
        vec![MigrationStep::Change(SchemaChange::AddColumn {
            table: TableName::new("missing"),
            column: ColumnDef::new("x", ColumnType::Int64),
        })],
        Vec::new(),
    )])
    .expect("set is structurally valid");

    let error = set.replay().expect_err("replay must reject the set");
    assert!(
        matches!(&error, MigrationError::Replay { version, .. }
            if version == "20260717_100000_broken"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn sets_order_by_version_and_reject_duplicates() {
    let set = MigrationSet::new(vec![add_email(), create_users()]).expect("set is valid");
    let versions: Vec<&str> = set
        .migrations()
        .iter()
        .map(jetorm_migration::Migration::version)
        .collect();
    assert_eq!(
        versions,
        ["20260717_100000_create_users", "20260717_110000_add_email"],
        "a set must not depend on the order it was given"
    );

    let error = MigrationSet::new(vec![create_users(), create_users()])
        .expect_err("duplicate versions must be rejected");
    assert!(matches!(error, MigrationError::DuplicateVersion { .. }));
}

#[test]
fn raw_sql_steps_declare_their_effect_on_the_schema_model() {
    // A raw step that changes the schema must say so, or replay would drift
    // from the real database and every later diff would be wrong.
    let set = MigrationSet::new(vec![
        create_users(),
        Migration::new(
            "20260717_120000_raw",
            vec![MigrationStep::Sql {
                sql: "ALTER TABLE users ADD COLUMN nickname text".to_owned(),
                state: vec![SchemaChange::AddColumn {
                    table: TableName::new("users"),
                    column: ColumnDef::new("nickname", ColumnType::Text).nullable(),
                }],
            }],
            Vec::new(),
        ),
    ])
    .expect("set is valid");

    let schema = set.replay().expect("set replays");
    assert!(
        schema
            .table(&TableName::new("users"))
            .expect("table exists")
            .column("nickname")
            .is_some(),
        "a declared raw-SQL effect must reach the replayed model"
    );
}

#[test]
fn data_only_raw_sql_leaves_the_model_untouched() {
    let set = MigrationSet::new(vec![
        create_users(),
        Migration::new(
            "20260717_120000_backfill",
            vec![MigrationStep::Sql {
                sql: "UPDATE users SET name = 'unknown' WHERE name = ''".to_owned(),
                state: Vec::new(),
            }],
            Vec::new(),
        ),
    ])
    .expect("set is valid");

    let with_backfill = set.replay().expect("set replays");
    let without = MigrationSet::new(vec![create_users()])
        .expect("set is valid")
        .replay()
        .expect("set replays");
    assert_eq!(
        with_backfill, without,
        "a data-only step must not change the schema model"
    );
}

#[test]
fn raw_sql_is_always_treated_as_destructive() {
    // Its contents are opaque, so the cautious assumption is the safe one.
    let step = MigrationStep::Sql {
        sql: "SELECT 1".to_owned(),
        state: Vec::new(),
    };
    assert!(step.is_destructive());

    assert!(
        !MigrationStep::Change(SchemaChange::CreateTable(users_table())).is_destructive(),
        "a create is not destructive"
    );
}

#[test]
fn migrations_round_trip_through_their_file_format() {
    let migration = add_email();
    let rendered = migration.to_toml().expect("migration serializes");
    let parsed = Migration::from_toml(Path::new("add_email.toml"), &rendered)
        .expect("migration parses back");
    assert_eq!(parsed, migration);

    // The format is meant to be read and reviewed, not just round-tripped.
    assert!(
        rendered.contains("20260717_110000_add_email"),
        "the version must be legible in the file: {rendered}"
    );
}

#[test]
fn a_malformed_file_reports_its_path() {
    let error = Migration::from_toml(Path::new("broken.toml"), "not a migration")
        .expect_err("malformed input must be rejected");
    assert!(
        matches!(&error, MigrationError::File { path, .. } if path == "broken.toml"),
        "unexpected error: {error:?}"
    );
}

#[test]
fn reversibility_is_explicit() {
    assert!(add_email().is_reversible());
    assert!(
        !Migration::new("20260717_100000_oneway", Vec::new(), Vec::new()).is_reversible(),
        "a migration without down steps is irreversible"
    );
}

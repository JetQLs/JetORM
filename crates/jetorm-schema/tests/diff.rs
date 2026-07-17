use jetorm_entity::{ColumnMeta, ColumnType, DecodeError, Entity, Model, TableMeta, Value};
use jetorm_schema::{
    ColumnDef, RenameCandidate, SchemaChange, SchemaSet, TableDef, TableName, diff,
};

#[derive(Clone, Copy, Debug)]
struct UserEntity;

#[derive(Clone, Debug)]
struct User;

impl Entity for UserEntity {
    type Model = User;
    const TABLE: TableMeta = TableMeta::new("users").with_schema("public");
    const COLUMNS: &'static [ColumnMeta] = &[
        ColumnMeta::new("id", "id", ColumnType::Int64)
            .primary_key()
            .auto_increment(),
        ColumnMeta::new("name", "name", ColumnType::Text),
        ColumnMeta::new("email", "email", ColumnType::Text)
            .nullable()
            .unique(),
    ];
    const PRIMARY_KEY: &'static [usize] = &[0];
}

impl Model for User {
    type Entity = UserEntity;

    fn into_values(self) -> Vec<Value> {
        Vec::new()
    }

    fn from_values(_values: Vec<Value>) -> Result<Self, DecodeError> {
        Ok(Self)
    }
}

fn users_table() -> TableDef {
    TableDef::new(TableName::qualified("public", "users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64).auto_increment())
        .with_column(ColumnDef::new("name", ColumnType::Text))
        .with_column(
            ColumnDef::new("email", ColumnType::Text)
                .nullable()
                .unique(),
        )
        .with_primary_key(vec!["id".to_owned()])
}

fn schema_of(tables: impl IntoIterator<Item = TableDef>) -> SchemaSet {
    let mut schema = SchemaSet::new();
    for table in tables {
        schema.insert(table);
    }
    schema
}

/// The core differ invariant: applying `diff(a, b)` to `a` reproduces `b`.
fn assert_round_trip(current: &SchemaSet, target: &SchemaSet) {
    let changes = diff(current, target);
    let mut replayed = current.clone();
    replayed
        .apply_all(changes.changes())
        .expect("diff output applies to its own source state");
    assert_eq!(
        &replayed, target,
        "diff({current:?} -> {target:?}) must reproduce the target"
    );
}

#[test]
fn from_entity_captures_all_column_facts() {
    let table = TableDef::from_entity::<UserEntity>();
    assert_eq!(table, users_table());
    assert_eq!(table.primary_key(), ["id"]);
    let email = table.column("email").expect("email column exists");
    assert!(email.is_nullable());
    assert!(email.is_unique());
    assert!(!email.is_auto_increment());
}

#[test]
fn empty_to_entities_creates_tables() {
    let mut target = SchemaSet::new();
    target.insert_entity::<UserEntity>();
    let changes = diff(&SchemaSet::new(), &target);
    assert_eq!(
        changes.changes(),
        [SchemaChange::CreateTable(users_table())]
    );
    assert!(!changes.has_destructive_changes());
    assert_round_trip(&SchemaSet::new(), &target);
}

#[test]
fn identical_states_produce_an_empty_diff() {
    let schema = schema_of([users_table()]);
    let changes = diff(&schema, &schema);
    assert!(changes.is_empty());
    assert!(changes.rename_candidates().is_empty());
}

#[test]
fn column_additions_drops_and_alterations_are_detected() {
    let current = schema_of([users_table()]);
    let target = schema_of([TableDef::new(TableName::qualified("public", "users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64).auto_increment())
        // `name` dropped; `email` becomes non-nullable and loses uniqueness.
        .with_column(ColumnDef::new("email", ColumnType::Text))
        // New nullable column.
        .with_column(ColumnDef::new("bio", ColumnType::Text).nullable())
        .with_primary_key(vec!["id".to_owned()])]);

    let changes = diff(&current, &target);
    let table = TableName::qualified("public", "users");
    assert_eq!(
        changes.changes(),
        [
            SchemaChange::AddColumn {
                table: table.clone(),
                column: ColumnDef::new("bio", ColumnType::Text).nullable(),
            },
            SchemaChange::SetNullable {
                table: table.clone(),
                column: "email".to_owned(),
                nullable: false,
            },
            SchemaChange::SetUnique {
                table: table.clone(),
                column: "email".to_owned(),
                unique: false,
            },
            SchemaChange::DropColumn {
                table,
                column: "name".to_owned(),
            },
        ]
    );
    assert!(changes.has_destructive_changes());
    assert_round_trip(&current, &target);
}

#[test]
fn type_and_primary_key_changes_round_trip() {
    let current = schema_of([users_table()]);
    let target = schema_of([TableDef::new(TableName::qualified("public", "users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64))
        .with_column(ColumnDef::new("name", ColumnType::Text))
        // Type change Text -> Json.
        .with_column(
            ColumnDef::new("email", ColumnType::Json)
                .nullable()
                .unique(),
        )
        // Composite primary key.
        .with_primary_key(vec!["id".to_owned(), "name".to_owned()])]);

    let changes = diff(&current, &target);
    assert!(changes.changes().contains(&SchemaChange::AlterColumnType {
        table: TableName::qualified("public", "users"),
        column: "email".to_owned(),
        from: ColumnType::Text,
        to: ColumnType::Json,
    }));
    assert!(changes.changes().iter().any(|change| matches!(
        change,
        SchemaChange::SetPrimaryKey { .. } | SchemaChange::SetAutoIncrement { .. }
    )));
    assert_round_trip(&current, &target);
}

#[test]
fn destructive_classification_matches_data_safety() {
    let table = TableName::new("t");
    let destructive = [
        SchemaChange::DropTable(table.clone()),
        SchemaChange::DropColumn {
            table: table.clone(),
            column: "c".to_owned(),
        },
        SchemaChange::AlterColumnType {
            table: table.clone(),
            column: "c".to_owned(),
            from: ColumnType::Int32,
            to: ColumnType::Int16,
        },
        SchemaChange::SetNullable {
            table: table.clone(),
            column: "c".to_owned(),
            nullable: false,
        },
        SchemaChange::SetUnique {
            table: table.clone(),
            column: "c".to_owned(),
            unique: true,
        },
        // A NOT NULL column addition fails on populated tables.
        SchemaChange::AddColumn {
            table: table.clone(),
            column: ColumnDef::new("c", ColumnType::Int32),
        },
    ];
    for change in &destructive {
        assert!(change.is_destructive(), "{change} must be destructive");
    }

    let safe = [
        SchemaChange::CreateTable(TableDef::new(table.clone())),
        SchemaChange::AddColumn {
            table: table.clone(),
            column: ColumnDef::new("c", ColumnType::Int32).nullable(),
        },
        SchemaChange::RenameColumn {
            table: table.clone(),
            from: "a".to_owned(),
            to: "b".to_owned(),
        },
        SchemaChange::SetNullable {
            table,
            column: "c".to_owned(),
            nullable: true,
        },
    ];
    for change in &safe {
        assert!(!change.is_destructive(), "{change} must be safe");
    }
}

#[test]
fn same_shape_drop_add_pairs_surface_as_rename_candidates() {
    let current = schema_of([users_table()]);
    let target = schema_of([TableDef::new(TableName::qualified("public", "users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64).auto_increment())
        // `name` renamed to `full_name`, same shape.
        .with_column(ColumnDef::new("full_name", ColumnType::Text))
        .with_column(
            ColumnDef::new("email", ColumnType::Text)
                .nullable()
                .unique(),
        )
        .with_primary_key(vec!["id".to_owned()])]);

    let mut changes = diff(&current, &target);
    let candidate = RenameCandidate::Column {
        table: TableName::qualified("public", "users"),
        from: "name".to_owned(),
        to: "full_name".to_owned(),
    };
    assert_eq!(
        changes.rename_candidates(),
        std::slice::from_ref(&candidate)
    );

    // Unconfirmed: the diff stays a destructive drop + add and round-trips.
    assert_round_trip(&current, &target);

    // Confirmed: the pair collapses into one non-destructive rename and the
    // round-trip guarantee still holds.
    assert!(changes.confirm_rename(&candidate));
    assert_eq!(
        changes.changes(),
        [SchemaChange::RenameColumn {
            table: TableName::qualified("public", "users"),
            from: "name".to_owned(),
            to: "full_name".to_owned(),
        }]
    );
    assert!(!changes.has_destructive_changes());
    assert!(changes.rename_candidates().is_empty());
    let mut replayed = current.clone();
    replayed
        .apply_all(changes.changes())
        .expect("confirmed rename applies");
    assert_eq!(replayed, target);

    // Confirming the same candidate again reports failure.
    assert!(!changes.confirm_rename(&candidate));
}

#[test]
fn identical_dropped_and_created_tables_surface_as_table_renames() {
    let current = schema_of([users_table()]);
    // The same definition as `users_table`, under a new name.
    let renamed = TableDef::new(TableName::qualified("public", "accounts"))
        .with_column(ColumnDef::new("id", ColumnType::Int64).auto_increment())
        .with_column(ColumnDef::new("name", ColumnType::Text))
        .with_column(
            ColumnDef::new("email", ColumnType::Text)
                .nullable()
                .unique(),
        )
        .with_primary_key(vec!["id".to_owned()]);
    let target = schema_of([renamed]);

    let mut changes = diff(&current, &target);
    let candidate = RenameCandidate::Table {
        from: TableName::qualified("public", "users"),
        to: TableName::qualified("public", "accounts"),
    };
    assert_eq!(
        changes.rename_candidates(),
        std::slice::from_ref(&candidate)
    );
    assert_round_trip(&current, &target);

    assert!(changes.confirm_rename(&candidate));
    assert_eq!(
        changes.changes(),
        [SchemaChange::RenameTable {
            from: TableName::qualified("public", "users"),
            to: TableName::qualified("public", "accounts"),
        }]
    );
    let mut replayed = current.clone();
    replayed
        .apply_all(changes.changes())
        .expect("confirmed table rename applies");
    assert_eq!(replayed, target);
}

#[test]
fn differently_shaped_pairs_are_not_rename_candidates() {
    let current = schema_of([
        TableDef::new(TableName::new("t")).with_column(ColumnDef::new("a", ColumnType::Int64))
    ]);
    let target = schema_of([TableDef::new(TableName::new("t"))
        // Different type: not a rename candidate.
        .with_column(ColumnDef::new("b", ColumnType::Text))]);

    let changes = diff(&current, &target);
    assert!(changes.rename_candidates().is_empty());
    assert_round_trip(&current, &target);
}

#[test]
fn apply_rejects_replay_drift() {
    let mut schema = schema_of([users_table()]);
    let error = schema
        .apply(&SchemaChange::AlterColumnType {
            table: TableName::qualified("public", "users"),
            column: "email".to_owned(),
            // Recorded prior type disagrees with the actual state.
            from: ColumnType::Uuid,
            to: ColumnType::Json,
        })
        .expect_err("drifted prior state must be rejected");
    assert!(matches!(
        error,
        jetorm_schema::ApplyError::StateMismatch { .. }
    ));
}

#[test]
fn column_rename_updates_primary_key_references() {
    let mut schema = schema_of([users_table()]);
    schema
        .apply(&SchemaChange::RenameColumn {
            table: TableName::qualified("public", "users"),
            from: "id".to_owned(),
            to: "user_id".to_owned(),
        })
        .expect("primary-key column rename applies");
    let table = schema
        .table(&TableName::qualified("public", "users"))
        .expect("table exists");
    assert_eq!(table.primary_key(), ["user_id"]);
    assert!(table.column("user_id").is_some());
    assert!(table.column("id").is_none());
}

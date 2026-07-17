use jetorm_entity::{
    ColumnMeta, ColumnType, DecodeError, Entity, ForeignKeyMeta, ForeignKeyRef, Model,
    ReferentialAction, TableMeta, Value,
};
use jetorm_schema::{
    ColumnDef, ForeignKeyDef, RenameCandidate, SchemaChange, SchemaSet, TableDef, TableName, diff,
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

    fn value(&self, _column: usize) -> Option<Value> {
        None
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

// ---- Foreign keys ----------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct PostEntity;

#[derive(Clone, Debug)]
struct Post;

impl Entity for PostEntity {
    type Model = Post;
    const TABLE: TableMeta = TableMeta::new("posts").with_schema("public");
    const COLUMNS: &'static [ColumnMeta] = &[
        ColumnMeta::new("id", "id", ColumnType::Int64).primary_key(),
        ColumnMeta::new("author_id", "author_id", ColumnType::Int64),
    ];
    const PRIMARY_KEY: &'static [usize] = &[0];
    const FOREIGN_KEYS: &'static [ForeignKeyRef] = &[ForeignKeyRef::new(
        1,
        UserEntity::TABLE,
        "id",
        ForeignKeyMeta::new(ReferentialAction::Cascade, ReferentialAction::NoAction),
    )];
}

impl Model for Post {
    type Entity = PostEntity;

    fn into_values(self) -> Vec<Value> {
        Vec::new()
    }

    fn from_values(_values: Vec<Value>) -> Result<Self, DecodeError> {
        Ok(Self)
    }

    fn value(&self, _column: usize) -> Option<Value> {
        None
    }
}

fn posts_table() -> TableDef {
    TableDef::new(TableName::qualified("public", "posts"))
        .with_column(ColumnDef::new("id", ColumnType::Int64))
        .with_column(ColumnDef::new("author_id", ColumnType::Int64))
        .with_primary_key(vec!["id".to_owned()])
        .with_foreign_key(
            ForeignKeyDef::new(
                "posts_author_id_fkey",
                "author_id",
                TableName::qualified("public", "users"),
                "id",
            )
            .on_delete(ReferentialAction::Cascade),
        )
}

#[test]
fn from_entity_captures_foreign_keys() {
    let table = TableDef::from_entity::<PostEntity>();
    let keys: Vec<_> = table.foreign_keys().collect();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name(), "posts_author_id_fkey");
    assert_eq!(keys[0].column(), "author_id");
    assert_eq!(
        keys[0].target_table(),
        &TableName::qualified("public", "users")
    );
    assert_eq!(keys[0].target_column(), "id");
    assert_eq!(keys[0].delete_action(), ReferentialAction::Cascade);
    assert_eq!(keys[0].update_action(), ReferentialAction::NoAction);
}

#[test]
fn foreign_keys_bracket_every_other_change() {
    // Created together: the constraint must land after BOTH tables exist,
    // even though "posts" sorts before "users".
    let current = SchemaSet::new();
    let target = schema_of([users_table(), posts_table()]);
    let changes = diff(&current, &target);
    assert!(matches!(
        changes.changes().last(),
        Some(SchemaChange::AddForeignKey { foreign_key, .. })
            if foreign_key.name() == "posts_author_id_fkey"
    ));
    // The bare creates carry no constraints of their own.
    for change in changes.changes() {
        if let SchemaChange::CreateTable(table) = change {
            assert_eq!(table.foreign_keys().count(), 0);
        }
    }
    assert_round_trip(&current, &target);

    // Dropped together: every constraint goes before any table does.
    let reverse = diff(&target, &current);
    assert!(matches!(
        reverse.changes().first(),
        Some(SchemaChange::DropForeignKey { name, .. }) if name == "posts_author_id_fkey"
    ));
    assert_round_trip(&target, &current);
}

#[test]
fn changed_foreign_keys_drop_and_re_add() {
    let current = schema_of([users_table(), posts_table()]);
    // Same constraint name, weaker delete action.
    let retargeted = posts_table().with_foreign_key(
        ForeignKeyDef::new(
            "posts_author_id_fkey",
            "author_id",
            TableName::qualified("public", "users"),
            "id",
        )
        .on_delete(ReferentialAction::Restrict),
    );
    let target = schema_of([users_table(), retargeted]);
    let changes = diff(&current, &target);
    assert_eq!(changes.changes().len(), 2);
    assert!(matches!(
        changes.changes()[0],
        SchemaChange::DropForeignKey { .. }
    ));
    assert!(matches!(
        changes.changes()[1],
        SchemaChange::AddForeignKey { .. }
    ));
    assert_round_trip(&current, &target);
}

#[test]
fn apply_rejects_dropping_a_referenced_table_or_column() {
    let schema = schema_of([users_table(), posts_table()]);
    let users = TableName::qualified("public", "users");

    let mut attempt = schema.clone();
    let error = attempt
        .apply(&SchemaChange::DropTable(users.clone()))
        .expect_err("a referenced table cannot be dropped");
    assert!(matches!(
        error,
        jetorm_schema::ApplyError::StillReferenced { .. }
    ));

    let mut attempt = schema.clone();
    let error = attempt
        .apply(&SchemaChange::DropColumn {
            table: users.clone(),
            column: "id".to_owned(),
        })
        .expect_err("a referenced column cannot be dropped");
    assert!(matches!(
        error,
        jetorm_schema::ApplyError::StillReferenced { .. }
    ));

    // Dropping the constraint first unblocks both.
    let mut attempt = schema.clone();
    attempt
        .apply(&SchemaChange::DropForeignKey {
            table: TableName::qualified("public", "posts"),
            name: "posts_author_id_fkey".to_owned(),
        })
        .expect("constraint drops");
    attempt
        .apply(&SchemaChange::DropTable(users))
        .expect("unreferenced table drops");
}

#[test]
fn apply_rejects_a_foreign_key_onto_a_non_unique_column() {
    let mut schema = schema_of([users_table(), posts_table()]);
    let error = schema
        .apply(&SchemaChange::AddForeignKey {
            table: TableName::qualified("public", "posts"),
            foreign_key: ForeignKeyDef::new(
                "posts_author_name_fkey",
                "author_id",
                TableName::qualified("public", "users"),
                // "name" is neither unique nor the primary key.
                "name",
            ),
        })
        .expect_err("the referenced column must be unique");
    assert!(matches!(
        error,
        jetorm_schema::ApplyError::StateMismatch { .. }
    ));
}

#[test]
fn renames_propagate_into_referencing_constraints() {
    let mut schema = schema_of([users_table(), posts_table()]);
    let users = TableName::qualified("public", "users");
    let posts = TableName::qualified("public", "posts");

    // Renaming the referenced column updates the constraint's target side.
    schema
        .apply(&SchemaChange::RenameColumn {
            table: users.clone(),
            from: "id".to_owned(),
            to: "user_id".to_owned(),
        })
        .expect("referenced column renames");
    // Renaming the referencing column updates the owning side.
    schema
        .apply(&SchemaChange::RenameColumn {
            table: posts.clone(),
            from: "author_id".to_owned(),
            to: "writer_id".to_owned(),
        })
        .expect("referencing column renames");
    // Renaming the referenced table follows, exactly as the database does.
    schema
        .apply(&SchemaChange::RenameTable {
            from: users,
            to: TableName::qualified("public", "accounts"),
        })
        .expect("referenced table renames");

    let key = schema
        .table(&posts)
        .expect("posts table exists")
        .foreign_key("posts_author_id_fkey")
        .expect("constraint kept its name, as the database keeps it");
    assert_eq!(key.column(), "writer_id");
    assert_eq!(
        key.target_table(),
        &TableName::qualified("public", "accounts")
    );
    assert_eq!(key.target_column(), "user_id");
}

#[test]
fn self_referencing_tables_diff_and_drop_cleanly() {
    let employees = TableDef::new(TableName::new("employees"))
        .with_column(ColumnDef::new("id", ColumnType::Int64))
        .with_column(ColumnDef::new("manager_id", ColumnType::Int64).nullable())
        .with_primary_key(vec!["id".to_owned()])
        .with_foreign_key(ForeignKeyDef::new(
            "employees_manager_id_fkey",
            "manager_id",
            TableName::new("employees"),
            "id",
        ));
    let current = SchemaSet::new();
    let target = schema_of([employees]);
    assert_round_trip(&current, &target);
    assert_round_trip(&target, &current);
}

// ---- Review-workflow regression tests ---------------------------------------

#[test]
fn confirming_a_primary_key_column_rename_still_round_trips() {
    // users(id pk) -> users(uid pk): the diff is drop+add+SetPrimaryKey and
    // a rename candidate. Confirming the rename must absorb the primary-key
    // change the rename itself performs, or apply rejects its own output.
    let current = schema_of([TableDef::new(TableName::new("users"))
        .with_column(ColumnDef::new("id", ColumnType::Int64))
        .with_primary_key(vec!["id".to_owned()])]);
    let target = schema_of([TableDef::new(TableName::new("users"))
        .with_column(ColumnDef::new("uid", ColumnType::Int64))
        .with_primary_key(vec!["uid".to_owned()])]);

    let mut changes = diff(&current, &target);
    let candidate = changes.rename_candidates()[0].clone();
    assert!(changes.confirm_rename(&candidate));

    let mut replayed = current.clone();
    replayed
        .apply_all(changes.changes())
        .expect("a confirmed rename must apply to its own source state");
    assert_eq!(replayed, target);
    assert!(
        !changes
            .changes()
            .iter()
            .any(|change| matches!(change, SchemaChange::SetPrimaryKey { .. })),
        "the rename subsumed the primary-key change entirely"
    );
}

#[test]
fn self_referencing_tables_surface_as_rename_candidates() {
    let table = |name: &str| {
        TableDef::new(TableName::new(name))
            .with_column(ColumnDef::new("id", ColumnType::Int64))
            .with_column(ColumnDef::new("manager_id", ColumnType::Int64).nullable())
            .with_primary_key(vec!["id".to_owned()])
            .with_foreign_key(ForeignKeyDef::new(
                format!("{name}_manager_id_fkey"),
                "manager_id",
                TableName::new(name),
                "id",
            ))
    };
    let current = schema_of([table("employees")]);
    let target = schema_of([table("staff")]);

    let mut changes = diff(&current, &target);
    let candidate = changes.rename_candidates().first().cloned().expect(
        "a self-reference embeds its own table name, which must not \
         disqualify an otherwise exact rename",
    );
    assert!(changes.confirm_rename(&candidate));

    let mut replayed = current.clone();
    replayed
        .apply_all(changes.changes())
        .expect("the confirmed self-referencing rename applies");
    assert_eq!(replayed, target);
}

#[test]
fn dropping_a_column_takes_the_tables_own_self_reference_along() {
    // PostgreSQL drops the altered table's constraints involving the column
    // — including a self-reference targeting it — so the model must too.
    let mut schema = schema_of([TableDef::new(TableName::new("employees"))
        .with_column(ColumnDef::new("id", ColumnType::Int64))
        .with_column(ColumnDef::new("manager_id", ColumnType::Int64).nullable())
        .with_primary_key(vec!["id".to_owned()])
        .with_foreign_key(ForeignKeyDef::new(
            "employees_manager_id_fkey",
            "manager_id",
            TableName::new("employees"),
            "id",
        ))]);
    schema
        .apply(&SchemaChange::SetPrimaryKey {
            table: TableName::new("employees"),
            from: vec!["id".to_owned()],
            to: vec![],
        })
        .expect("clearing the key first keeps the drop about the reference");
    schema
        .apply(&SchemaChange::DropColumn {
            table: TableName::new("employees"),
            column: "id".to_owned(),
        })
        .expect("the referenced column drops, exactly as it does live");
    let table = schema
        .table(&TableName::new("employees"))
        .expect("table remains");
    assert_eq!(
        table.foreign_keys().count(),
        0,
        "the self-reference went with its referenced column"
    );
}

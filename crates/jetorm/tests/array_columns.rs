//! Array columns: metadata, spelling, DDL, query SQL, and the live round trip.

use jetorm::prelude::*;
use jetorm_schema::{SchemaSet, diff};

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "boards")]
pub struct Board {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub tags: Vec<String>,
    pub scores: Vec<i32>,
    pub markers: Option<Vec<uuid::Uuid>>,
}

#[test]
fn array_fields_carry_their_element_type_through_the_metadata() {
    use jetorm_entity::{ColumnType, ElementType};

    assert_eq!(
        BoardEntity::COLUMNS[1].column_type(),
        ColumnType::ArrayOf(ElementType::Text)
    );
    assert_eq!(
        BoardEntity::COLUMNS[2].column_type(),
        ColumnType::ArrayOf(ElementType::Int32)
    );
    assert_eq!(
        BoardEntity::COLUMNS[3].column_type(),
        ColumnType::ArrayOf(ElementType::Uuid)
    );
    assert!(
        BoardEntity::COLUMNS[3].is_nullable(),
        "the whole array is nullable, not its elements"
    );
}

#[test]
fn array_types_spell_themselves_with_brackets() {
    use jetorm_entity::{ColumnType, ElementType};

    let spelled = serde_json::to_value(ColumnType::ArrayOf(ElementType::Int32))
        .expect("array types serialize");
    assert_eq!(spelled, serde_json::json!("Int32[]"));
    let parsed: ColumnType = serde_json::from_value(spelled).expect("the spelling parses back");
    assert_eq!(parsed, ColumnType::ArrayOf(ElementType::Int32));

    // Scalars keep their unbracketed spelling — existing migration files
    // stay readable.
    assert_eq!(
        serde_json::to_value(ColumnType::Text).expect("scalars serialize"),
        serde_json::json!("Text")
    );
    assert!(
        serde_json::from_value::<ColumnType>(serde_json::json!("Json[]")).is_err(),
        "only scalar elements form arrays"
    );
}

#[test]
fn array_ddl_appends_brackets_to_the_element_type() {
    use jetorm_dialect::postgres::ddl::render_changes;

    let mut target = SchemaSet::new();
    target.insert_entity::<BoardEntity>();
    let changes = diff(&SchemaSet::new(), &target);
    let statements = render_changes(changes.changes()).expect("changes render");
    let create = &statements[0];
    assert!(
        create.contains("\"tags\" text[] NOT NULL"),
        "text array column: {create}"
    );
    assert!(
        create.contains("\"scores\" integer[] NOT NULL"),
        "integer array column: {create}"
    );
    assert!(
        create.contains("\"markers\" uuid[]") && !create.contains("\"markers\" uuid[] NOT NULL"),
        "nullable uuid array column: {create}"
    );
}

#[test]
fn array_binds_cast_to_the_element_array_type() {
    use jetorm::{Dialect, IntoAfterBurnerIr, Postgres};

    let query = BoardEntity::find().filter(board::Tags.eq(vec!["a".to_owned()]));
    let module = query.into_afterburner_ir().expect("array filter lowers");
    let statement = Postgres
        .render_query(&module)
        .expect("array filter renders");
    let sql = statement.sql();
    assert!(
        sql.contains("$1::text[]"),
        "the bound array casts to its element array type: {sql}"
    );

    let insert = BoardEntity::insert(Board {
        id: 0,
        tags: vec!["a".to_owned()],
        scores: vec![1, 2],
        markers: None,
    })
    .returning();
    let module = insert.into_afterburner_ir().expect("array insert lowers");
    let statement = Postgres
        .render_query(&module)
        .expect("array insert renders");
    let sql = statement.sql();
    assert!(
        sql.contains("::text[]") && sql.contains("::integer[]") && sql.contains("::uuid[]"),
        "every inserted array casts to its own type: {sql}"
    );
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn array_columns_round_trip_on_live_postgres() {
    use jetorm::Database;
    use jetorm_dialect::postgres::ddl::render_changes;
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    let container = Postgres::default()
        .with_tag("18-alpine")
        .start()
        .await
        .expect("PostgreSQL test container starts (is Docker running?)");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container maps the PostgreSQL port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = Database::connect(&url).await.expect("connects");

    let mut target = SchemaSet::new();
    target.insert_entity::<BoardEntity>();
    let changes = diff(&SchemaSet::new(), &target);
    for statement in render_changes(changes.changes()).expect("changes render") {
        sqlx::query(&statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL rejected {statement:?}: {error}"));
    }

    // Typed insert with returning: populated, empty, and NULL arrays all
    // come back as they went in.
    let marker = uuid::Uuid::new_v4();
    let created = BoardEntity::insert(Board {
        id: 0,
        tags: vec!["rust".to_owned(), "orm".to_owned()],
        scores: vec![10, 20, 30],
        markers: Some(vec![marker]),
    })
    .returning()
    .all(&db)
    .await
    .expect("typed insert returns");
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].tags, ["rust", "orm"]);
    assert_eq!(created[0].scores, [10, 20, 30]);
    assert_eq!(created[0].markers.as_deref(), Some(&[marker][..]));

    let empty = BoardEntity::insert(Board {
        id: 0,
        tags: Vec::new(),
        scores: Vec::new(),
        markers: None,
    })
    .returning()
    .all(&db)
    .await
    .expect("empty arrays insert");
    assert!(empty[0].tags.is_empty(), "an empty array is not NULL");
    assert!(empty[0].scores.is_empty());
    assert_eq!(empty[0].markers, None, "a NULL array is not empty");

    // Whole-array equality filters on the bound array value.
    let matched = BoardEntity::find()
        .filter(board::Scores.eq(vec![10, 20, 30]))
        .all(&db)
        .await
        .expect("array equality filter runs");
    assert_eq!(matched.len(), 1);
    assert_eq!(matched[0].tags, ["rust", "orm"]);

    // An external writer can store a NULL element — legal in PostgreSQL,
    // unrepresentable in Vec<T>. The read fails as a named condition
    // pointing at the stored state, not a raw driver error.
    sqlx::query("UPDATE boards SET tags = ARRAY['a', NULL] WHERE scores = ARRAY[]::integer[]")
        .execute(db.pool())
        .await
        .expect("the external write runs");
    let poisoned = BoardEntity::find().all(&db).await;
    match poisoned {
        Err(jetorm::ExecuteError::ArrayDecode { detail, .. }) => {
            assert!(detail.contains("NULL element"), "{detail}");
        }
        other => panic!("expected ArrayDecode for the NULL element, got {other:?}"),
    }
    sqlx::query("UPDATE boards SET tags = ARRAY[]::text[] WHERE scores = ARRAY[]::integer[]")
        .execute(db.pool())
        .await
        .expect("cleanup runs");

    // Updates replace the whole array.
    BoardEntity::update()
        .set(board::Tags, vec!["updated".to_owned()])
        .filter(board::Id.eq(matched[0].id))
        .execute(&db)
        .await
        .expect("array update runs");
    let updated = BoardEntity::find()
        .filter(board::Id.eq(matched[0].id))
        .one(&db)
        .await
        .expect("updated row reads")
        .expect("the row still exists");
    assert_eq!(updated.tags, ["updated"]);

    db.close().await;
}

use afterburner::AfterBurnerError;
use jetorm::prelude::*;
use jetorm::{
    ColumnMeta, ColumnType, DecodeError, Dialect, LoweringError, Postgres, StatementResult,
    TableMeta,
};

#[derive(Clone, Debug, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct BrokenEntity;

#[derive(Clone, Debug)]
pub struct BrokenModel;

impl Entity for BrokenEntity {
    type Model = BrokenModel;
    const TABLE: TableMeta = TableMeta::new("broken");
    const COLUMNS: &'static [ColumnMeta] =
        &[ColumnMeta::new("id", "id", ColumnType::Int64).primary_key()];
    const PRIMARY_KEY: &'static [usize] = &[0];
}

impl Model for BrokenModel {
    type Entity = BrokenEntity;

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

fn render(query: impl jetorm::IntoAfterBurnerIr<Error = LoweringError>) -> jetorm::Statement {
    let module = jetorm::afterburner!(query).expect("behavior lowers to verified IR");
    Postgres
        .render_statement(&module)
        .expect("behavior renders to PostgreSQL")
}

#[test]
fn count_exists_and_projection_render_from_typed_builders() {
    let count = render(
        UserEntity::find()
            .filter(user::Id.gt(10))
            .order_by(user::Id.desc())
            .into_count(),
    );
    assert_eq!(count.result(), StatementResult::Rows);
    assert!(count.sql().contains("\"count\"(*)"));
    assert!(!count.sql().contains("ORDER BY"));

    let exists = render(UserEntity::find().filter(user::Email.is_null()).exists());
    assert!(exists.sql().contains("SELECT EXISTS (SELECT"));

    let projection = render(UserEntity::find().select((user::Email,)).into_select());
    assert!(projection.sql().contains("AS \"email\""));
}

#[test]
fn insert_upsert_and_returning_have_explicit_result_contracts() {
    let insert = UserEntity::insert(User {
        id: 0,
        name: "first".to_owned(),
        email: None,
    })
    .values(User {
        id: 0,
        name: "second".to_owned(),
        email: Some("second@example.com".to_owned()),
    });
    assert_eq!(insert.binds().len(), 4, "generated id values are omitted");
    let statement = render(insert);
    assert_eq!(statement.result(), StatementResult::AffectedRows);
    assert!(
        statement
            .sql()
            .starts_with("INSERT INTO \"users\" (\"name\", \"email\") VALUES")
    );

    let returning = render(
        UserEntity::insert(User {
            id: 0,
            name: "returning".to_owned(),
            email: None,
        })
        .returning(),
    );
    assert_eq!(returning.result(), StatementResult::Rows);
    assert!(
        returning
            .sql()
            .ends_with("RETURNING \"id\", \"name\", \"email\"")
    );

    let upsert = render(
        UserEntity::insert(User {
            id: 0,
            name: "upsert".to_owned(),
            email: None,
        })
        .on_conflict_update(),
    );
    assert!(upsert.sql().contains(
        "ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\", \"email\" = EXCLUDED.\"email\""
    ));
}

#[test]
fn update_and_delete_require_an_explicit_row_scope() {
    let error = jetorm::afterburner!(UserEntity::update().set(user::Name, "unsafe"))
        .expect_err("unbounded update must be rejected");
    assert!(matches!(
        error,
        AfterBurnerError::Lowering(LoweringError::UnboundedMutation)
    ));

    let error =
        jetorm::afterburner!(UserEntity::delete()).expect_err("unbounded delete must be rejected");
    assert!(matches!(
        error,
        AfterBurnerError::Lowering(LoweringError::UnboundedMutation)
    ));

    let error = jetorm::afterburner!(UserEntity::update().filter(user::Id.eq(7)))
        .expect_err("an update without assignments must be rejected");
    assert!(matches!(
        error,
        AfterBurnerError::Lowering(LoweringError::EmptyUpdate)
    ));

    let error = jetorm::afterburner!(
        UserEntity::update()
            .set(user::Name, "first")
            .set(user::Name, "second")
            .filter(user::Id.eq(7))
    )
    .expect_err("duplicate assignments must be rejected");
    assert!(matches!(
        error,
        AfterBurnerError::Lowering(LoweringError::DuplicateAssignment { ref column })
            if column == "name"
    ));

    let error = jetorm::afterburner!(
        UserEntity::update()
            .set_null(user::Name)
            .filter(user::Id.eq(7))
    )
    .expect_err("null writes to required columns must be rejected");
    assert!(matches!(
        error,
        AfterBurnerError::Lowering(LoweringError::NullForRequiredColumn { ref column })
            if column == "name"
    ));

    let update = render(
        UserEntity::update()
            .set(user::Name, "safe")
            .filter(user::Id.eq(7)),
    );
    assert_eq!(update.result(), StatementResult::AffectedRows);
    assert!(
        update
            .sql()
            .contains("SET \"name\" = $1::text WHERE (\"t0\".\"id\" = $2::bigint)")
    );
    assert_eq!(update.bind_order(), &[0, 1]);

    let delete = render(UserEntity::delete().all_rows());
    assert_eq!(delete.result(), StatementResult::AffectedRows);
    assert!(delete.sql().ends_with("WHERE TRUE"));
}

#[test]
fn malformed_manual_models_fail_lowering_without_panicking() {
    let insert = BrokenEntity::insert(BrokenModel);
    assert!(insert.binds().is_empty());
    let error = jetorm::afterburner!(insert).expect_err("invalid model width must be rejected");
    assert!(matches!(
        error,
        AfterBurnerError::Lowering(LoweringError::ModelWidthMismatch {
            row: 0,
            expected: 1,
            actual: 0,
        })
    ));
}

#[test]
fn exists_preserves_distinct_because_offsets_make_it_semantic() {
    use jetorm::{Dialect, IntoAfterBurnerIr, Postgres};
    // With rows [x, x], distinct().offset(1) leaves zero rows — one
    // distinct row, skipped — so EXISTS must see the deduplication.
    let query = UserEntity::find().distinct().offset(1).exists();
    let module = query.clone().into_afterburner_ir().expect("exists lowers");
    let statement = Postgres.render_query(&module).expect("exists renders");
    assert!(
        statement.sql().contains("DISTINCT"),
        "distinct survives into the existence subquery: {}",
        statement.sql()
    );

    use jetorm::CacheableQuery;
    let plain = UserEntity::find().offset(1).exists();
    assert_ne!(
        CacheableQuery::shape(&query),
        CacheableQuery::shape(&plain),
        "the distinct and non-distinct existence tests are different statements"
    );
}

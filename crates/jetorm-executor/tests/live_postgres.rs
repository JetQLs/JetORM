//! Round-trip tests against live PostgreSQL servers.
//!
//! Every test starts its own disposable PostgreSQL container through
//! `testcontainers`, so tests are fully isolated from each other and from
//! any local database, and run safely in parallel. They are ignored by
//! default because they need a running Docker daemon:
//!
//! ```text
//! cargo xtask test-live    # or: just test-live
//! ```

use afterburner::ir::{
    BinaryOperator, EffectSet, Field, FunctionRef, JoinKind, Literal, LogicalOp, Module, NullOrder,
    OperationSpec, ScalarOp, Schema, SchemaId, SetOperator, SortDirection, SortKey, SqlType,
    TerminatorOp, Type, ValueId, Volatility, WindowFrame, WindowFrameBound, WindowFrameUnit,
    WindowSpec, verify_module,
};
use jetorm_dialect::{Dialect, Postgres as PostgresDialect};
use jetorm_entity::{
    Column, ColumnMeta, ColumnType, DecodeError, Entity, Model, SqlValue, TableMeta, Value,
};
use jetorm_executor::{Database, SelectExecute};
use jetorm_query::{ColumnExt, EntityQuery, TextColumnExt};
use sqlx::Row;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// PostgreSQL version live tests run against; kept current with the newest
/// stable major release.
const POSTGRES_TAG: &str = "18-alpine";

const TABLE: &str = "live_users";

#[derive(Clone, Copy, Debug)]
struct UserEntity;

#[derive(Clone, Debug, PartialEq)]
struct User {
    id: i64,
    name: String,
    email: Option<String>,
}

impl Entity for UserEntity {
    type Model = User;
    const TABLE: TableMeta = TableMeta::new(TABLE);
    const COLUMNS: &'static [ColumnMeta] = &[
        ColumnMeta::new("id", "id", ColumnType::Int64).primary_key(),
        ColumnMeta::new("name", "name", ColumnType::Text),
        ColumnMeta::new("email", "email", ColumnType::Text).nullable(),
    ];
    const PRIMARY_KEY: &'static [usize] = &[0];
}

impl Model for User {
    type Entity = UserEntity;

    fn into_values(self) -> Vec<Value> {
        vec![
            self.id.into_value(),
            self.name.into_value(),
            self.email.into_value(),
        ]
    }

    fn from_values(values: Vec<Value>) -> Result<Self, DecodeError> {
        if values.len() != 3 {
            return Err(DecodeError::ColumnCount {
                expected: 3,
                actual: values.len(),
            });
        }
        let mut values = values.into_iter();
        let id = i64::from_value(values.next().expect("length checked")).map_err(|mismatch| {
            DecodeError::Column {
                name: "id",
                mismatch,
            }
        })?;
        let name =
            String::from_value(values.next().expect("length checked")).map_err(|mismatch| {
                DecodeError::Column {
                    name: "name",
                    mismatch,
                }
            })?;
        let email = Option::<String>::from_value(values.next().expect("length checked")).map_err(
            |mismatch| DecodeError::Column {
                name: "email",
                mismatch,
            },
        )?;
        Ok(Self { id, name, email })
    }
}

#[derive(Clone, Copy, Debug)]
struct Id;

impl Column for Id {
    type Entity = UserEntity;
    type Rust = i64;
    const INDEX: usize = 0;
    const NULLABLE: bool = false;
}

#[derive(Clone, Copy, Debug)]
struct Name;

impl Column for Name {
    type Entity = UserEntity;
    type Rust = String;
    const INDEX: usize = 1;
    const NULLABLE: bool = false;
}

#[derive(Clone, Copy, Debug)]
struct Email;

impl Column for Email {
    type Entity = UserEntity;
    type Rust = String;
    const INDEX: usize = 2;
    const NULLABLE: bool = true;
}

/// Starts one disposable PostgreSQL server and connects a database to it.
///
/// The container handle must stay alive for the duration of the test;
/// dropping it stops and removes the container.
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

async fn create_fixture(db: &Database) {
    sqlx::query(&format!(
        "CREATE TABLE {TABLE} (id bigint PRIMARY KEY, name text NOT NULL, email text)"
    ))
    .execute(db.pool())
    .await
    .expect("create table");
    sqlx::query(&format!(
        "INSERT INTO {TABLE} (id, name, email) VALUES \
         (1, 'alice', 'alice@example.com'), \
         (2, 'bob', NULL), \
         (3, 'carol', 'carol@example.com')"
    ))
    .execute(db.pool())
    .await
    .expect("insert fixture rows");
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn filtered_select_round_trips() {
    let (_container, db) = fresh_database().await;
    create_fixture(&db).await;

    let users = UserEntity::find()
        .filter(Email.like("%@example.com").and(Id.gt(0)))
        .order_by(Id.desc())
        .all(&db)
        .await
        .expect("filtered select executes");
    assert_eq!(
        users,
        [
            User {
                id: 3,
                name: "carol".to_owned(),
                email: Some("carol@example.com".to_owned()),
            },
            User {
                id: 1,
                name: "alice".to_owned(),
                email: Some("alice@example.com".to_owned()),
            },
        ]
    );

    let nobody = UserEntity::find()
        .filter(Email.is_null())
        .one(&db)
        .await
        .expect("null-test select executes");
    assert_eq!(
        nobody,
        Some(User {
            id: 2,
            name: "bob".to_owned(),
            email: None,
        })
    );

    let first_by_name = UserEntity::find()
        .order_by(Name.asc())
        .one(&db)
        .await
        .expect("ordered select executes");
    assert_eq!(
        first_by_name.map(|user| user.name),
        Some("alice".to_owned())
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn repeated_shapes_reuse_the_plan_cache() {
    let (_container, db) = fresh_database().await;
    create_fixture(&db).await;

    // Same shape with different bound values: the second call must reuse the
    // cached statement and still bind fresh values.
    let first = UserEntity::find()
        .filter(Id.eq(1))
        .all(&db)
        .await
        .expect("first execution");
    let second = UserEntity::find()
        .filter(Id.eq(2))
        .all(&db)
        .await
        .expect("second execution");
    assert_eq!(first[0].name, "alice");
    assert_eq!(second[0].name, "bob");

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn dropped_transactions_roll_back() {
    let (_container, db) = fresh_database().await;
    create_fixture(&db).await;

    {
        let mut transaction = db.begin().await.expect("transaction begins");
        sqlx::query(&format!(
            "INSERT INTO {TABLE} (id, name, email) VALUES (99, 'temp', NULL)"
        ))
        .execute(transaction.connection())
        .await
        .expect("insert inside transaction");

        let inside = UserEntity::find()
            .filter(Id.eq(99))
            .one(&mut transaction)
            .await
            .expect("select inside transaction");
        assert!(inside.is_some(), "open transaction sees its own insert");
        // Dropped without commit: the insert must roll back.
    }

    let outside = UserEntity::find()
        .filter(Id.eq(99))
        .one(&db)
        .await
        .expect("select after rollback");
    assert_eq!(outside, None, "dropped transaction must leave no rows");

    db.close().await;
}

fn bigint_type() -> Type {
    Type::scalar(
        SqlType::Integer {
            bits: 64,
            signed: true,
        },
        false,
    )
}

fn text_type() -> Type {
    Type::scalar(SqlType::Utf8, true)
}

fn append_values(module: &mut Module, schema: SchemaId, rows: Vec<Vec<Literal>>) -> ValueId {
    let root = module.root_block();
    let mut editor = module.editor();
    let operation = editor
        .append_operation(
            root,
            OperationSpec::new(LogicalOp::Values { rows }).with_result(Type::relation(schema)),
        )
        .expect("values append");
    editor.result(operation, 0).expect("values result")
}

/// Builds one query exercising every scope-establishing PostgreSQL renderer.
fn advanced_codegen_module() -> Module {
    let mut module = Module::new();
    let input_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("id", bigint_type()),
        Field::new("name", text_type()),
    ]));
    let join_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("left_id", bigint_type()),
        Field::new("left_name", text_type()),
        Field::new("right_id", bigint_type()),
        Field::new("right_name", text_type()),
    ]));
    let result_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("category", text_type()),
        Field::new("ordinal", bigint_type()),
    ]));

    let left = append_values(
        &mut module,
        input_schema,
        vec![
            vec![Literal::Integer(1), Literal::String("a".into())],
            vec![Literal::Integer(2), Literal::String("b".into())],
            vec![Literal::Integer(3), Literal::String("c".into())],
        ],
    );
    let right = append_values(
        &mut module,
        input_schema,
        vec![
            vec![Literal::Integer(1), Literal::String("x".into())],
            vec![Literal::Integer(1), Literal::String("y".into())],
            vec![Literal::Integer(2), Literal::String("z".into())],
        ],
    );
    let root = module.root_block();
    let windowed = {
        let mut editor = module.editor();
        let join = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Join {
                    kind: JoinKind::Inner,
                    has_condition: true,
                })
                .with_operands(vec![left, right])
                .with_result(Type::relation(join_schema)),
            )
            .expect("join appends");
        let join_region = editor.add_region(join).expect("join owns a region");
        let join_block = editor
            .append_block(
                join_region,
                vec![bigint_type(), text_type(), bigint_type(), text_type()],
            )
            .expect("join block appends");
        let left_id = editor
            .block_argument(join_block, 0)
            .expect("left id argument");
        let right_id = editor
            .block_argument(join_block, 2)
            .expect("right id argument");
        let equal = editor
            .append_operation(
                join_block,
                OperationSpec::new(ScalarOp::Binary(BinaryOperator::Equal))
                    .with_operands(vec![left_id, right_id])
                    .with_result(Type::boolean(false)),
            )
            .expect("join equality appends");
        let predicate = editor.result(equal, 0).expect("join predicate");
        editor
            .append_operation(
                join_block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate]),
            )
            .expect("join yield appends");
        let joined = editor.result(join, 0).expect("join result");

        let aggregate_schema = editor.intern_schema(Schema::new(vec![
            Field::new("category", text_type()),
            Field::new("matches", bigint_type()),
        ]));
        let aggregate = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Aggregate { group_keys: 1 })
                    .with_operands(vec![joined])
                    .with_result(Type::relation(aggregate_schema)),
            )
            .expect("aggregate appends");
        let aggregate_region = editor
            .add_region(aggregate)
            .expect("aggregate owns a region");
        let aggregate_block = editor
            .append_block(
                aggregate_region,
                vec![bigint_type(), text_type(), bigint_type(), text_type()],
            )
            .expect("aggregate block appends");
        let category = editor
            .block_argument(aggregate_block, 1)
            .expect("aggregate category argument");
        let counted_id = editor
            .block_argument(aggregate_block, 2)
            .expect("aggregate count argument");
        let count = editor
            .append_operation(
                aggregate_block,
                OperationSpec::new(ScalarOp::AggregateCall {
                    function: FunctionRef::new("count"),
                    distinct: false,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_operands(vec![counted_id])
                .with_result(bigint_type()),
            )
            .expect("aggregate call appends");
        let matches = editor.result(count, 0).expect("aggregate call result");
        editor
            .append_operation(
                aggregate_block,
                OperationSpec::new(TerminatorOp::Yield)
                    .with_operands(vec![category, category, matches]),
            )
            .expect("aggregate yield appends");
        let aggregated = editor.result(aggregate, 0).expect("aggregate result");

        let window = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Window)
                    .with_operands(vec![aggregated])
                    .with_result(Type::relation(result_schema)),
            )
            .expect("window appends");
        let window_region = editor.add_region(window).expect("window owns a region");
        let window_block = editor
            .append_block(window_region, vec![text_type(), bigint_type()])
            .expect("window block appends");
        let category = editor
            .block_argument(window_block, 0)
            .expect("window category argument");
        let partition = editor
            .append_operation(
                window_block,
                OperationSpec::new(ScalarOp::Literal(Literal::String("all".into())))
                    .with_result(text_type()),
            )
            .expect("window partition literal appends");
        let partition = editor
            .result(partition, 0)
            .expect("window partition literal result");
        let offset = editor
            .append_operation(
                window_block,
                OperationSpec::new(ScalarOp::Literal(Literal::Integer(2)))
                    .with_result(bigint_type()),
            )
            .expect("window frame offset appends");
        let offset = editor
            .result(offset, 0)
            .expect("window frame offset result");
        let specification = WindowSpec::new(
            1,
            vec![SortKey::new(SortDirection::Ascending, NullOrder::Last)],
        )
        .with_frame(WindowFrame::between(
            WindowFrameUnit::Rows,
            WindowFrameBound::Preceding,
            WindowFrameBound::CurrentRow,
        ));
        let row_number = editor
            .append_operation(
                window_block,
                OperationSpec::new(ScalarOp::WindowCall {
                    function: FunctionRef::new("row_number"),
                    argument_count: 0,
                    window: specification,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                // Constant partition, category ordering, start-bound offset.
                .with_operands(vec![partition, category, offset])
                .with_result(bigint_type()),
            )
            .expect("window call appends");
        let ordinal = editor.result(row_number, 0).expect("window call result");
        editor
            .append_operation(
                window_block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![category, ordinal]),
            )
            .expect("window yield appends");
        editor.result(window, 0).expect("window result")
    };

    let fallback = append_values(
        &mut module,
        result_schema,
        vec![vec![
            Literal::String("fallback".into()),
            Literal::Integer(99),
        ]],
    );
    {
        let mut editor = module.editor();
        let set = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Set {
                    operator: SetOperator::Union,
                    all: true,
                })
                .with_operands(vec![windowed, fallback])
                .with_result(Type::relation(result_schema)),
            )
            .expect("set operation appends");
        let result = editor.result(set, 0).expect("set result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![result]),
            )
            .expect("query return appends");
    }
    module
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn advanced_codegen_executes_on_postgres() {
    let (_container, db) = fresh_database().await;
    let module = advanced_codegen_module();
    verify_module(&module).expect("advanced codegen fixture verifies");
    let statement = PostgresDialect
        .render_query(&module)
        .expect("advanced IR renders");

    let rows = sqlx::query(statement.sql())
        .fetch_all(db.pool())
        .await
        .expect("generated SQL executes");
    let mut values: Vec<(String, i64)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get("category").expect("category decodes"),
                row.try_get("ordinal").expect("ordinal decodes"),
            )
        })
        .collect();
    values.sort_by(|left, right| left.0.cmp(&right.0));

    assert_eq!(
        values
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "fallback"]
    );
    let mut ordinals = values[..2]
        .iter()
        .map(|(_, ordinal)| *ordinal)
        .collect::<Vec<_>>();
    ordinals.sort_unstable();
    assert_eq!(ordinals, [1, 2]);
    assert_eq!(values[2].1, 99);

    db.close().await;
}

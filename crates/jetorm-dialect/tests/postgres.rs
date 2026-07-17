use afterburner::ir::{
    BinaryOperator, EffectSet, Field, FunctionRef, JoinKind, Literal, LogicalOp, Module, NullOrder,
    OperationSpec, ScalarOp, Schema, SchemaId, SetOperator, SortDirection, SortKey, SqlType,
    TableRef, TerminatorOp, Type, UnaryOperator, Volatility, WindowFrame, WindowFrameBound,
    WindowFrameExclusion, WindowFrameUnit, WindowSpec, verify_module,
};
use jetorm_dialect::{Dialect, Postgres, RenderError};
use jetorm_entity::{Column, ColumnMeta, ColumnType, DecodeError, Entity, Model, TableMeta, Value};
use jetorm_query::{ColumnExt, EntityQuery, TextColumnExt};

#[derive(Clone, Copy, Debug)]
struct UserEntity;

#[derive(Clone, Debug)]
struct User {
    id: i64,
    name: String,
    email: Option<String>,
}

impl Entity for UserEntity {
    type Model = User;
    const TABLE: TableMeta = TableMeta::new("users").with_schema("public");
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
            Value::Int64(self.id),
            Value::Text(self.name),
            self.email
                .map_or(Value::Null(ColumnType::Text), Value::Text),
        ]
    }

    fn from_values(_values: Vec<Value>) -> Result<Self, DecodeError> {
        unimplemented!("rendering tests never decode rows")
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

fn render(
    query: impl afterburner::IntoAfterBurnerIr<Error = jetorm_query::LoweringError>,
) -> jetorm_dialect::Statement {
    let module = afterburner::afterburner!(query).expect("query lowers to verified IR");
    Postgres
        .render_query(&module)
        .expect("verified IR renders to SQL")
}

#[test]
fn bare_select_renders_explicit_projection() {
    let statement = render(UserEntity::find());
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" \
         FROM \"public\".\"users\" AS \"t0\""
    );
    assert_eq!(statement.parameter_count(), 0);
}

#[test]
fn full_pipeline_renders_one_flat_select() {
    let query = UserEntity::find()
        .filter(Email.like("%@example.com").and(Id.gt(100)))
        .order_by(Id.desc())
        .limit(20);
    let statement = render(query);
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" \
         FROM \"public\".\"users\" AS \"t0\" \
         WHERE ((\"t0\".\"email\" LIKE $1::text) AND (\"t0\".\"id\" > $2::bigint)) \
         ORDER BY \"t0\".\"id\" DESC \
         LIMIT 20"
    );
    assert_eq!(statement.bind_order(), [0, 1]);
}

#[test]
fn nullability_unification_casts_leave_no_sql_trace() {
    // The frontend inserts a nullability-widening IR cast when predicates of
    // mixed nullability combine; it must not surface as SQL `CAST`.
    let query = UserEntity::find().filter(Email.like("%x%").and(Id.gt(0)));
    let statement = render(query);
    assert!(
        !statement.sql().contains("CAST"),
        "nullability-only casts must render as their operand: {}",
        statement.sql()
    );
}

#[test]
fn distinct_and_offset_render_without_nesting() {
    let statement = render(UserEntity::find().distinct().offset(5));
    assert_eq!(
        statement.sql(),
        "SELECT DISTINCT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" \
         FROM \"public\".\"users\" AS \"t0\" \
         OFFSET 5"
    );
}

#[test]
fn null_tests_negation_and_ilike_render() {
    let query = UserEntity::find().filter(Email.is_null().or(!Name.ilike("a%")));
    let statement = render(query);
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" \
         FROM \"public\".\"users\" AS \"t0\" \
         WHERE ((\"t0\".\"email\" IS NULL) OR (NOT (\"t0\".\"name\" ILIKE $1::text)))"
    );
    assert_eq!(statement.bind_order(), [0]);
}

#[test]
fn sort_keys_render_direction_and_null_placement() {
    let query = UserEntity::find()
        .order_by(Email.asc().nulls_first())
        .order_by(Id.desc().nulls_last());
    let statement = render(query);
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" \
         FROM \"public\".\"users\" AS \"t0\" \
         ORDER BY \"t0\".\"email\" ASC NULLS FIRST, \"t0\".\"id\" DESC NULLS LAST"
    );
}

/// Interns the hand-built test schema: `id` bigint, `name` nullable text.
fn test_schema(module: &mut Module) -> SchemaId {
    module.editor().intern_schema(Schema::new(vec![
        Field::new("id", bigint_type(false)),
        Field::new("name", Type::scalar(SqlType::Utf8, true)),
    ]))
}

fn bigint_type(nullable: bool) -> Type {
    Type::scalar(
        SqlType::Integer {
            bits: 64,
            signed: true,
        },
        nullable,
    )
}

fn scan_relation(module: &mut Module, schema: SchemaId) -> afterburner::ir::ValueId {
    named_scan_relation(module, schema, "items")
}

fn named_scan_relation(
    module: &mut Module,
    schema: SchemaId,
    table: &str,
) -> afterburner::ir::ValueId {
    let root = module.root_block();
    let mut editor = module.editor();
    let scan = editor
        .append_operation(
            root,
            OperationSpec::new(LogicalOp::Scan {
                table: TableRef::new(table),
                columns: vec!["id".to_owned(), "name".to_owned()],
            })
            .with_result(Type::relation(schema)),
        )
        .expect("scan appends");
    editor.result(scan, 0).expect("scan has one result")
}

fn query_return(module: &mut Module, relation: afterburner::ir::ValueId) {
    let root = module.root_block();
    module
        .editor()
        .append_operation(
            root,
            OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
        )
        .expect("query return appends");
}

#[test]
fn values_rows_render_with_exact_column_types() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let values = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Values {
                    rows: vec![
                        vec![Literal::Integer(1), Literal::String("O'Brien".to_owned())],
                        vec![Literal::Integer(2), Literal::Null],
                    ],
                })
                .with_result(Type::relation(schema)),
            )
            .expect("values appends");
        let relation = editor.result(values, 0).expect("values has one result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built module verifies");

    let statement = Postgres.render_query(&module).expect("values render");
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\" \
         FROM (VALUES (CAST(1 AS bigint), CAST('O''Brien' AS text)), \
         (CAST(2 AS bigint), CAST(NULL AS text))) AS \"t0\" (\"id\", \"name\")"
    );
}

#[test]
fn reused_parameter_positions_share_one_placeholder() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let relation = scan_relation(&mut module, schema);
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let relation_type = Type::relation(schema);
        let bigint = Type::scalar(
            SqlType::Integer {
                bits: 64,
                signed: true,
            },
            false,
        );
        let boolean = Type::boolean(false);

        let filter = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Filter)
                    .with_operands(vec![relation])
                    .with_result(relation_type),
            )
            .expect("filter appends");
        let region = editor.add_region(filter).expect("filter owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint.clone(), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("filter block appends");
        let id_argument = editor.block_argument(block, 0).expect("id argument");

        let parameter = |editor: &mut afterburner::ir::IrEditor<'_>| {
            let operation = editor
                .append_operation(
                    block,
                    OperationSpec::new(ScalarOp::Parameter {
                        position: 0,
                        name: None,
                    })
                    .with_result(bigint.clone()),
                )
                .expect("parameter appends");
            editor.result(operation, 0).expect("parameter result")
        };
        let first = parameter(&mut editor);
        let second = parameter(&mut editor);

        let compare = |editor: &mut afterburner::ir::IrEditor<'_>,
                       operator: BinaryOperator,
                       right: afterburner::ir::ValueId| {
            let operation = editor
                .append_operation(
                    block,
                    OperationSpec::new(ScalarOp::Binary(operator))
                        .with_operands(vec![id_argument, right])
                        .with_result(boolean.clone()),
                )
                .expect("comparison appends");
            editor.result(operation, 0).expect("comparison result")
        };
        let greater = compare(&mut editor, BinaryOperator::GreaterThan, first);
        let less = compare(&mut editor, BinaryOperator::LessThan, second);

        let both = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::Binary(BinaryOperator::And))
                    .with_operands(vec![greater, less])
                    .with_result(boolean.clone()),
            )
            .expect("conjunction appends");
        let predicate = editor.result(both, 0).expect("conjunction result");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate]),
            )
            .expect("yield appends");

        let filtered = editor.result(filter, 0).expect("filter result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![filtered]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built module verifies");

    let statement = Postgres.render_query(&module).expect("filter renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\" \
         WHERE ((\"t0\".\"id\" > $1::bigint) AND (\"t0\".\"id\" < $1::bigint))"
    );
    assert_eq!(statement.bind_order(), [0]);
}

#[test]
fn top_n_before_filter_nests_one_derived_table() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let relation = scan_relation(&mut module, schema);
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let relation_type = Type::relation(schema);
        let field_types = vec![
            Type::scalar(
                SqlType::Integer {
                    bits: 64,
                    signed: true,
                },
                false,
            ),
            Type::scalar(SqlType::Utf8, true),
        ];

        let sort = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Sort {
                    keys: vec![SortKey::new(
                        SortDirection::Descending,
                        NullOrder::DialectDefault,
                    )],
                })
                .with_operands(vec![relation])
                .with_result(relation_type.clone()),
            )
            .expect("sort appends");
        let sort_region = editor.add_region(sort).expect("sort owns a region");
        let sort_block = editor
            .append_block(sort_region, field_types.clone())
            .expect("sort block appends");
        let sort_key = editor.block_argument(sort_block, 0).expect("sort key");
        editor
            .append_operation(
                sort_block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![sort_key]),
            )
            .expect("sort yield appends");
        let sorted = editor.result(sort, 0).expect("sort result");

        let limit = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Limit {
                    offset: None,
                    fetch: Some(3),
                })
                .with_operands(vec![sorted])
                .with_result(relation_type.clone()),
            )
            .expect("limit appends");
        let limited = editor.result(limit, 0).expect("limit result");

        let filter = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Filter)
                    .with_operands(vec![limited])
                    .with_result(relation_type),
            )
            .expect("filter appends");
        let filter_region = editor.add_region(filter).expect("filter owns a region");
        let filter_block = editor
            .append_block(filter_region, field_types)
            .expect("filter block appends");
        let name_argument = editor.block_argument(filter_block, 1).expect("name arg");
        let not_null = editor
            .append_operation(
                filter_block,
                OperationSpec::new(ScalarOp::Unary(UnaryOperator::IsNotNull))
                    .with_operands(vec![name_argument])
                    .with_result(Type::boolean(false)),
            )
            .expect("null test appends");
        let predicate = editor.result(not_null, 0).expect("null test result");
        editor
            .append_operation(
                filter_block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate]),
            )
            .expect("filter yield appends");
        let filtered = editor.result(filter, 0).expect("filter result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![filtered]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built module verifies");

    let statement = Postgres
        .render_query(&module)
        .expect("nested shape renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t1\".\"id\", \"t1\".\"name\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\" \
         ORDER BY \"t0\".\"id\" DESC LIMIT 3) AS \"t1\" \
         WHERE (\"t1\".\"name\" IS NOT NULL)"
    );
}

#[test]
fn interior_sort_without_limit_is_rejected() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let relation = scan_relation(&mut module, schema);
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let relation_type = Type::relation(schema);
        let sort = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Sort {
                    keys: vec![SortKey::new(
                        SortDirection::Ascending,
                        NullOrder::DialectDefault,
                    )],
                })
                .with_operands(vec![relation])
                .with_result(relation_type.clone()),
            )
            .expect("sort appends");
        let region = editor.add_region(sort).expect("sort owns a region");
        let block = editor
            .append_block(
                region,
                vec![
                    Type::scalar(
                        SqlType::Integer {
                            bits: 64,
                            signed: true,
                        },
                        false,
                    ),
                    Type::scalar(SqlType::Utf8, true),
                ],
            )
            .expect("sort block appends");
        let key = editor.block_argument(block, 0).expect("sort key");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![key]),
            )
            .expect("sort yield appends");
        let sorted = editor.result(sort, 0).expect("sort result");

        let distinct = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Distinct)
                    .with_operands(vec![sorted])
                    .with_result(relation_type),
            )
            .expect("distinct appends");
        let deduplicated = editor.result(distinct, 0).expect("distinct result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![deduplicated]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built module verifies");

    let error = Postgres
        .render_query(&module)
        .expect_err("unpreservable ordering must be rejected");
    assert!(
        matches!(&error, RenderError::Unsupported { detail } if detail.contains("ORDER BY")),
        "unexpected error: {error:?}"
    );
}

fn join_output_schema(module: &mut Module, kind: JoinKind) -> SchemaId {
    if matches!(kind, JoinKind::Semi | JoinKind::Anti) {
        return test_schema(module);
    }

    let left_nullable = matches!(kind, JoinKind::Right | JoinKind::Full);
    let right_nullable = matches!(kind, JoinKind::Left | JoinKind::Full);
    module.editor().intern_schema(Schema::new(vec![
        Field::new("left_id", bigint_type(left_nullable)),
        Field::new("left_name", Type::scalar(SqlType::Utf8, true)),
        Field::new("right_id", bigint_type(right_nullable)),
        Field::new("right_name", Type::scalar(SqlType::Utf8, true)),
    ]))
}

fn join_module(kind: JoinKind) -> Module {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let joined_schema = join_output_schema(&mut module, kind);
    let left = named_scan_relation(&mut module, schema, "left_items");
    let right = named_scan_relation(&mut module, schema, "right_items");
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let has_condition = kind != JoinKind::Cross;
        let join = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Join {
                    kind,
                    has_condition,
                })
                .with_operands(vec![left, right])
                .with_result(Type::relation(joined_schema)),
            )
            .expect("join appends");
        if has_condition {
            let region = editor.add_region(join).expect("join owns a region");
            let block = editor
                .append_block(
                    region,
                    vec![
                        bigint_type(false),
                        Type::scalar(SqlType::Utf8, true),
                        bigint_type(false),
                        Type::scalar(SqlType::Utf8, true),
                    ],
                )
                .expect("join block appends");
            let left_id = editor.block_argument(block, 0).expect("left id argument");
            let right_id = editor.block_argument(block, 2).expect("right id argument");
            let equality = editor
                .append_operation(
                    block,
                    OperationSpec::new(ScalarOp::Binary(BinaryOperator::Equal))
                        .with_operands(vec![left_id, right_id])
                        .with_result(Type::boolean(false)),
                )
                .expect("join equality appends");
            let predicate = editor.result(equality, 0).expect("join predicate result");
            editor
                .append_operation(
                    block,
                    OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate]),
                )
                .expect("join yield appends");
        }
        let joined = editor.result(join, 0).expect("join result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![joined]),
            )
            .expect("query return appends");
    }
    module
}

#[test]
fn all_join_kinds_render_with_their_sql_semantics() {
    let cases = [
        (JoinKind::Inner, " INNER JOIN "),
        (JoinKind::Left, " LEFT JOIN "),
        (JoinKind::Right, " RIGHT JOIN "),
        (JoinKind::Full, " FULL JOIN "),
        (JoinKind::Semi, "EXISTS (SELECT 1 FROM"),
        (JoinKind::Anti, "NOT EXISTS (SELECT 1 FROM"),
        (JoinKind::Cross, " CROSS JOIN "),
    ];

    for (kind, fragment) in cases {
        let module = join_module(kind);
        verify_module(&module).expect("hand-built join module verifies");
        let statement = Postgres.render_query(&module).expect("join renders");
        assert!(
            statement.sql().contains(fragment),
            "{kind:?} join did not contain {fragment:?}: {}",
            statement.sql()
        );
    }
}

#[test]
fn inner_join_uses_both_row_scopes_and_explicit_output_names() {
    let module = join_module(JoinKind::Inner);
    verify_module(&module).expect("hand-built join module verifies");
    let statement = Postgres.render_query(&module).expect("join renders");

    assert_eq!(
        statement.sql(),
        "SELECT \"t4\".\"left_id\", \"t4\".\"left_name\", \"t4\".\"right_id\", \"t4\".\"right_name\" FROM \
         (SELECT \"t1\".\"id\" AS \"left_id\", \"t1\".\"name\" AS \"left_name\", \
         \"t3\".\"id\" AS \"right_id\", \"t3\".\"name\" AS \"right_name\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"left_items\" AS \"t0\") AS \"t1\" \
         INNER JOIN (SELECT \"t2\".\"id\", \"t2\".\"name\" FROM \"right_items\" AS \"t2\") AS \"t3\" \
         ON (\"t1\".\"id\" = \"t3\".\"id\")) AS \"t4\""
    );
}

#[test]
fn set_operators_render_as_derived_relations() {
    let cases = [
        (SetOperator::Union, true, " UNION ALL "),
        (SetOperator::Intersect, false, " INTERSECT "),
        (SetOperator::Except, false, " EXCEPT "),
    ];

    for (operator, all, fragment) in cases {
        let mut module = Module::new();
        let schema = test_schema(&mut module);
        let first = named_scan_relation(&mut module, schema, "first_items");
        let second = named_scan_relation(&mut module, schema, "second_items");
        let root = module.root_block();
        let set = module
            .editor()
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Set { operator, all })
                    .with_operands(vec![first, second])
                    .with_result(Type::relation(schema)),
            )
            .expect("set operation appends");
        let relation = module.editor().result(set, 0).expect("set result");
        query_return(&mut module, relation);
        verify_module(&module).expect("hand-built set module verifies");

        let statement = Postgres
            .render_query(&module)
            .expect("set operation renders");
        assert!(
            statement.sql().contains(fragment),
            "{operator:?} did not contain {fragment:?}: {}",
            statement.sql()
        );
    }
}

#[test]
fn grouped_aggregate_renders_group_keys_and_aggregate_calls() {
    let mut module = Module::new();
    let input_schema = test_schema(&mut module);
    let input = scan_relation(&mut module, input_schema);
    let output_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("category", Type::scalar(SqlType::Utf8, true)),
        Field::new("total", bigint_type(false)),
    ]));
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let aggregate = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Aggregate { group_keys: 1 })
                    .with_operands(vec![input])
                    .with_result(Type::relation(output_schema)),
            )
            .expect("aggregate appends");
        let region = editor
            .add_region(aggregate)
            .expect("aggregate owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint_type(false), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("aggregate block appends");
        let id = editor.block_argument(block, 0).expect("id argument");
        let category = editor.block_argument(block, 1).expect("category argument");
        let sum = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::AggregateCall {
                    function: FunctionRef::new("sum"),
                    distinct: false,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_operands(vec![id])
                .with_result(bigint_type(false)),
            )
            .expect("aggregate call appends");
        let total = editor.result(sum, 0).expect("aggregate call result");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield)
                    .with_operands(vec![category, category, total]),
            )
            .expect("aggregate yield appends");
        let relation = editor.result(aggregate, 0).expect("aggregate result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built aggregate module verifies");

    let statement = Postgres.render_query(&module).expect("aggregate renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t1\".\"name\" AS \"category\", \"sum\"(\"t1\".\"id\") AS \"total\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\") AS \"t1\" \
         GROUP BY \"t1\".\"name\""
    );
}

#[test]
fn aggregate_can_group_by_a_value_omitted_from_its_result() {
    let mut module = Module::new();
    let input_schema = test_schema(&mut module);
    let input = scan_relation(&mut module, input_schema);
    let output_schema = module
        .editor()
        .intern_schema(Schema::new(vec![Field::new("total", bigint_type(false))]));
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let aggregate = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Aggregate { group_keys: 1 })
                    .with_operands(vec![input])
                    .with_result(Type::relation(output_schema)),
            )
            .expect("aggregate appends");
        let region = editor
            .add_region(aggregate)
            .expect("aggregate owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint_type(false), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("aggregate block appends");
        let id = editor.block_argument(block, 0).expect("id argument");
        let category = editor.block_argument(block, 1).expect("category argument");
        let count = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::AggregateCall {
                    function: FunctionRef::new("count"),
                    distinct: false,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_operands(vec![id])
                .with_result(bigint_type(false)),
            )
            .expect("aggregate call appends");
        let total = editor.result(count, 0).expect("aggregate call result");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![category, total]),
            )
            .expect("aggregate yield appends");
        let relation = editor.result(aggregate, 0).expect("aggregate result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("aggregate with a hidden group key verifies");

    let statement = Postgres.render_query(&module).expect("aggregate renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"count\"(\"t1\".\"id\") AS \"total\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\") AS \"t1\" \
         GROUP BY \"t1\".\"name\""
    );
}

fn global_sum_module(mix_ungrouped_row: bool) -> Module {
    let mut module = Module::new();
    let input_schema = test_schema(&mut module);
    let input = scan_relation(&mut module, input_schema);
    let output_schema = module
        .editor()
        .intern_schema(Schema::new(vec![Field::new("total", bigint_type(false))]));
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let aggregate = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Aggregate { group_keys: 0 })
                    .with_operands(vec![input])
                    .with_result(Type::relation(output_schema)),
            )
            .expect("aggregate appends");
        let region = editor
            .add_region(aggregate)
            .expect("aggregate owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint_type(false), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("aggregate block appends");
        let id = editor.block_argument(block, 0).expect("id argument");
        let sum = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::AggregateCall {
                    function: FunctionRef::new("sum"),
                    distinct: true,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_operands(vec![id])
                .with_result(bigint_type(false)),
            )
            .expect("aggregate call appends");
        let total = editor.result(sum, 0).expect("aggregate call result");
        let yielded = if mix_ungrouped_row {
            let add = editor
                .append_operation(
                    block,
                    OperationSpec::new(ScalarOp::Binary(BinaryOperator::Add))
                        .with_operands(vec![total, id])
                        .with_result(bigint_type(false)),
                )
                .expect("mixed aggregate expression appends");
            editor.result(add, 0).expect("mixed expression result")
        } else {
            total
        };
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![yielded]),
            )
            .expect("aggregate yield appends");
        let relation = editor.result(aggregate, 0).expect("aggregate result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    module
}

#[test]
fn global_aggregate_uses_the_empty_grouping_set() {
    let module = global_sum_module(false);
    verify_module(&module).expect("hand-built aggregate module verifies");

    let statement = Postgres
        .render_query(&module)
        .expect("global aggregate renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"sum\"(DISTINCT \"t1\".\"id\") AS \"total\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\") AS \"t1\" \
         GROUP BY ()"
    );
}

#[test]
fn verifier_rejects_an_ungrouped_row_reference_in_an_aggregate_expression() {
    let module = global_sum_module(true);
    let errors = verify_module(&module).expect_err("ungrouped row reference must be rejected");
    assert!(errors.iter().any(|error| {
        error
            .message()
            .contains("only through explicit group keys or aggregate calls")
    }));
}

#[test]
fn global_window_calls_render_with_an_empty_window_specification() {
    let mut module = Module::new();
    let input_schema = test_schema(&mut module);
    let input = scan_relation(&mut module, input_schema);
    let output_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("id", bigint_type(false)),
        Field::new("ordinal", bigint_type(false)),
    ]));
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let window = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Window)
                    .with_operands(vec![input])
                    .with_result(Type::relation(output_schema)),
            )
            .expect("window appends");
        let region = editor.add_region(window).expect("window owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint_type(false), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("window block appends");
        let id = editor.block_argument(block, 0).expect("id argument");
        let row_number = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::WindowCall {
                    function: FunctionRef::new("row_number"),
                    argument_count: 0,
                    window: WindowSpec::global(),
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_result(bigint_type(false)),
            )
            .expect("window call appends");
        let ordinal = editor.result(row_number, 0).expect("window call result");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![id, ordinal]),
            )
            .expect("window yield appends");
        let relation = editor.result(window, 0).expect("window result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built window module verifies");

    let statement = Postgres.render_query(&module).expect("window renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t1\".\"id\" AS \"id\", \"row_number\"() OVER () AS \"ordinal\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\") AS \"t1\""
    );
}

#[test]
fn window_calls_render_partition_order_and_frame_operands() {
    let mut module = Module::new();
    let input_schema = test_schema(&mut module);
    let input = scan_relation(&mut module, input_schema);
    let output_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("id", bigint_type(false)),
        Field::new("running_count", bigint_type(false)),
    ]));
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let window = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Window)
                    .with_operands(vec![input])
                    .with_result(Type::relation(output_schema)),
            )
            .expect("window appends");
        let region = editor.add_region(window).expect("window owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint_type(false), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("window block appends");
        let id = editor.block_argument(block, 0).expect("id argument");
        let category = editor.block_argument(block, 1).expect("category argument");
        let offset = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::Literal(Literal::Integer(2)))
                    .with_result(bigint_type(false)),
            )
            .expect("frame offset appends");
        let offset = editor.result(offset, 0).expect("frame offset result");
        let specification = WindowSpec::new(
            1,
            vec![SortKey::new(SortDirection::Descending, NullOrder::Last)],
        )
        .with_frame(
            WindowFrame::between(
                WindowFrameUnit::Rows,
                WindowFrameBound::Preceding,
                WindowFrameBound::CurrentRow,
            )
            .with_exclusion(WindowFrameExclusion::CurrentRow),
        );
        let count = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::WindowCall {
                    function: FunctionRef::new("count"),
                    argument_count: 1,
                    window: specification,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                // Function argument, partition key, order key, frame offset.
                .with_operands(vec![id, category, id, offset])
                .with_result(bigint_type(false)),
            )
            .expect("window call appends");
        let running_count = editor.result(count, 0).expect("window call result");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![id, running_count]),
            )
            .expect("window yield appends");
        let relation = editor.result(window, 0).expect("window result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("explicit window specification verifies");

    let statement = Postgres.render_query(&module).expect("window renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t1\".\"id\" AS \"id\", \
         \"count\"(\"t1\".\"id\") OVER (PARTITION BY \"t1\".\"name\" \
         ORDER BY \"t1\".\"id\" DESC NULLS LAST \
         ROWS BETWEEN 2 PRECEDING AND CURRENT ROW EXCLUDE CURRENT ROW) AS \"running_count\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\") AS \"t1\""
    );
}

fn render_window_frame(frame: WindowFrame, offsets: &[i128]) -> String {
    let mut module = Module::new();
    let input_schema = test_schema(&mut module);
    let input = scan_relation(&mut module, input_schema);
    let output_schema = module.editor().intern_schema(Schema::new(vec![
        Field::new("id", bigint_type(false)),
        Field::new("ordinal", bigint_type(false)),
    ]));
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let window = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Window)
                    .with_operands(vec![input])
                    .with_result(Type::relation(output_schema)),
            )
            .expect("window appends");
        let region = editor.add_region(window).expect("window owns a region");
        let block = editor
            .append_block(
                region,
                vec![bigint_type(false), Type::scalar(SqlType::Utf8, true)],
            )
            .expect("window block appends");
        let id = editor.block_argument(block, 0).expect("id argument");
        let mut operands = vec![id];
        for offset in offsets {
            let literal = editor
                .append_operation(
                    block,
                    OperationSpec::new(ScalarOp::Literal(Literal::Integer(*offset)))
                        .with_result(bigint_type(false)),
                )
                .expect("frame offset appends");
            operands.push(editor.result(literal, 0).expect("frame offset result"));
        }
        let call = editor
            .append_operation(
                block,
                OperationSpec::new(ScalarOp::WindowCall {
                    function: FunctionRef::new("row_number"),
                    argument_count: 0,
                    window: WindowSpec::new(
                        0,
                        vec![SortKey::new(
                            SortDirection::Ascending,
                            NullOrder::DialectDefault,
                        )],
                    )
                    .with_frame(frame),
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                // Order expression followed by start/end frame offsets.
                .with_operands(operands)
                .with_result(bigint_type(false)),
            )
            .expect("window call appends");
        let ordinal = editor.result(call, 0).expect("window call result");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![id, ordinal]),
            )
            .expect("window yield appends");
        let relation = editor.result(window, 0).expect("window result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("window frame fixture verifies");
    Postgres
        .render_query(&module)
        .expect("window frame renders")
        .sql()
        .to_owned()
}

#[test]
fn window_frame_units_bounds_and_exclusions_render_positionally() {
    let cases = [
        (
            WindowFrame::between(
                WindowFrameUnit::Range,
                WindowFrameBound::UnboundedPreceding,
                WindowFrameBound::Following,
            )
            .with_exclusion(WindowFrameExclusion::Group),
            vec![3],
            "RANGE BETWEEN UNBOUNDED PRECEDING AND 3 FOLLOWING EXCLUDE GROUP",
        ),
        (
            WindowFrame::new(WindowFrameUnit::Groups, WindowFrameBound::Preceding)
                .with_exclusion(WindowFrameExclusion::Ties),
            vec![4],
            "GROUPS 4 PRECEDING EXCLUDE TIES",
        ),
        (
            WindowFrame::between(
                WindowFrameUnit::Rows,
                WindowFrameBound::CurrentRow,
                WindowFrameBound::UnboundedFollowing,
            ),
            Vec::new(),
            "ROWS BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING",
        ),
    ];

    for (frame, offsets, expected_frame) in cases {
        let sql = render_window_frame(frame, &offsets);
        let expected = format!(
            "SELECT \"t1\".\"id\" AS \"id\", \"row_number\"() OVER (ORDER BY \
             \"t1\".\"id\" ASC {expected_frame}) AS \"ordinal\" FROM \
             (SELECT \"t0\".\"id\", \"t0\".\"name\" FROM \"items\" AS \"t0\") AS \"t1\""
        );
        assert_eq!(sql, expected);
    }
}

#[test]
fn unverified_modules_are_rejected() {
    // A fresh module has an unterminated root block and must not render.
    let module = Module::new();
    let error = Postgres
        .render_query(&module)
        .expect_err("invalid module must not render");
    assert!(matches!(error, RenderError::InvalidModule(_)));
}

/// Appends a projection computing `(id + 1) AS "next_id"` over the relation.
fn project_next_id(
    module: &mut Module,
    relation: afterburner::ir::ValueId,
) -> afterburner::ir::ValueId {
    let root = module.root_block();
    let mut editor = module.editor();
    let bigint = Type::scalar(
        SqlType::Integer {
            bits: 64,
            signed: true,
        },
        false,
    );
    let output_schema =
        editor.intern_schema(Schema::new(vec![Field::new("next_id", bigint.clone())]));
    let project = editor
        .append_operation(
            root,
            OperationSpec::new(LogicalOp::Project)
                .with_operands(vec![relation])
                .with_result(Type::relation(output_schema)),
        )
        .expect("project appends");
    let region = editor.add_region(project).expect("project owns a region");
    let block = editor
        .append_block(
            region,
            vec![bigint.clone(), Type::scalar(SqlType::Utf8, true)],
        )
        .expect("project block appends");
    let id_argument = editor.block_argument(block, 0).expect("id argument");
    let one = editor
        .append_operation(
            block,
            OperationSpec::new(ScalarOp::Literal(Literal::Integer(1))).with_result(bigint.clone()),
        )
        .expect("literal appends");
    let one_value = editor.result(one, 0).expect("literal result");
    let sum = editor
        .append_operation(
            block,
            OperationSpec::new(ScalarOp::Binary(BinaryOperator::Add))
                .with_operands(vec![id_argument, one_value])
                .with_result(bigint),
        )
        .expect("addition appends");
    let sum_value = editor.result(sum, 0).expect("addition result");
    editor
        .append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(vec![sum_value]),
        )
        .expect("project yield appends");
    editor.result(project, 0).expect("project result")
}

#[test]
fn projection_renders_computed_columns() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let relation = scan_relation(&mut module, schema);
    let projected = project_next_id(&mut module, relation);
    query_return(&mut module, projected);
    verify_module(&module).expect("hand-built module verifies");

    let statement = Postgres.render_query(&module).expect("projection renders");
    assert_eq!(
        statement.sql(),
        "SELECT (\"t0\".\"id\" + 1) AS \"next_id\" FROM \"items\" AS \"t0\""
    );
}

#[test]
fn sort_after_projection_wraps_a_derived_table() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let relation = scan_relation(&mut module, schema);
    let projected = project_next_id(&mut module, relation);
    let root = module.root_block();
    let sorted = {
        let mut editor = module.editor();
        let bigint = Type::scalar(
            SqlType::Integer {
                bits: 64,
                signed: true,
            },
            false,
        );
        let output_schema =
            editor.intern_schema(Schema::new(vec![Field::new("next_id", bigint.clone())]));
        let sort = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Sort {
                    keys: vec![SortKey::new(
                        SortDirection::Ascending,
                        NullOrder::DialectDefault,
                    )],
                })
                .with_operands(vec![projected])
                .with_result(Type::relation(output_schema)),
            )
            .expect("sort appends");
        let region = editor.add_region(sort).expect("sort owns a region");
        let block = editor
            .append_block(region, vec![bigint])
            .expect("sort block appends");
        let key = editor.block_argument(block, 0).expect("sort key");
        editor
            .append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![key]),
            )
            .expect("sort yield appends");
        editor.result(sort, 0).expect("sort result")
    };
    query_return(&mut module, sorted);
    verify_module(&module).expect("hand-built module verifies");

    let statement = Postgres
        .render_query(&module)
        .expect("sorted projection renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t1\".\"next_id\" FROM \
         (SELECT (\"t0\".\"id\" + 1) AS \"next_id\" FROM \"items\" AS \"t0\") AS \"t1\" \
         ORDER BY \"t1\".\"next_id\" ASC"
    );
}

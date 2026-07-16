use afterburner::ir::{
    BinaryOperator, Field, JoinKind, Literal, LogicalOp, Module, NullOrder, OperationSpec,
    ScalarOp, Schema, SchemaId, SortDirection, SortKey, SqlType, TableRef, TerminatorOp, Type,
    UnaryOperator, verify_module,
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
        Field::new(
            "id",
            Type::scalar(
                SqlType::Integer {
                    bits: 64,
                    signed: true,
                },
                false,
            ),
        ),
        Field::new("name", Type::scalar(SqlType::Utf8, true)),
    ]))
}

fn scan_relation(module: &mut Module, schema: SchemaId) -> afterburner::ir::ValueId {
    let root = module.root_block();
    let mut editor = module.editor();
    let scan = editor
        .append_operation(
            root,
            OperationSpec::new(LogicalOp::Scan {
                table: TableRef::new("items"),
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

#[test]
fn joins_are_rejected_with_a_precise_diagnostic() {
    let mut module = Module::new();
    let schema = test_schema(&mut module);
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let joined_schema = editor.intern_schema(Schema::new(vec![
            Field::new(
                "left_id",
                Type::scalar(
                    SqlType::Integer {
                        bits: 64,
                        signed: true,
                    },
                    false,
                ),
            ),
            Field::new(
                "right_id",
                Type::scalar(
                    SqlType::Integer {
                        bits: 64,
                        signed: true,
                    },
                    false,
                ),
            ),
        ]));
        let scan = |editor: &mut afterburner::ir::IrEditor<'_>, table: &str| {
            let operation = editor
                .append_operation(
                    root,
                    OperationSpec::new(LogicalOp::Scan {
                        table: TableRef::new(table),
                        columns: vec!["id".to_owned(), "name".to_owned()],
                    })
                    .with_result(Type::relation(schema)),
                )
                .expect("scan appends");
            editor.result(operation, 0).expect("scan result")
        };
        let left = scan(&mut editor, "left_items");
        let right = scan(&mut editor, "right_items");
        let join = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Join {
                    kind: JoinKind::Cross,
                    has_condition: false,
                })
                .with_operands(vec![left, right])
                .with_result(Type::relation(joined_schema)),
            )
            .expect("join appends");
        let joined = editor.result(join, 0).expect("join result");
        editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![joined]),
            )
            .expect("query return appends");
    }
    verify_module(&module).expect("hand-built module verifies");

    let error = Postgres
        .render_query(&module)
        .expect_err("joins unsupported");
    assert!(
        matches!(&error, RenderError::Unsupported { detail } if detail.contains("join")),
        "unexpected error: {error:?}"
    );
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

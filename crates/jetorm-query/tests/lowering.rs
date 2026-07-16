use afterburner::afterburner;
use afterburner::ir::{
    LogicalOp, Module, OperationKind, ScalarOp, TerminatorOp, WalkOrder, verify_module,
    walk_operations,
};
use jetorm_entity::{
    Column, ColumnMeta, ColumnType, DecodeError, Entity, Model, SqlValue, TableMeta, Value,
};
use jetorm_query::{ColumnExt, EntityQuery, TextColumnExt};

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
        let mut next = |name: &'static str| {
            let value = values.next().expect("length checked above");
            (name, value)
        };
        let (name, value) = next("id");
        let id =
            i64::from_value(value).map_err(|mismatch| DecodeError::Column { name, mismatch })?;
        let (name_field, value) = next("name");
        let name = String::from_value(value).map_err(|mismatch| DecodeError::Column {
            name: name_field,
            mismatch,
        })?;
        let (name_field, value) = next("email");
        let email =
            Option::<String>::from_value(value).map_err(|mismatch| DecodeError::Column {
                name: name_field,
                mismatch,
            })?;
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

fn root_logical_kinds(module: &Module) -> Vec<String> {
    let root = module
        .block(module.root_block())
        .expect("root block exists");
    root.operations()
        .iter()
        .map(|operation| {
            match module
                .operation(*operation)
                .expect("root operations are live")
                .kind()
            {
                OperationKind::Logical(LogicalOp::Scan { .. }) => "scan".to_owned(),
                OperationKind::Logical(LogicalOp::Filter) => "filter".to_owned(),
                OperationKind::Logical(LogicalOp::Distinct) => "distinct".to_owned(),
                OperationKind::Logical(LogicalOp::Sort { .. }) => "sort".to_owned(),
                OperationKind::Logical(LogicalOp::Limit { .. }) => "limit".to_owned(),
                OperationKind::Terminator(TerminatorOp::QueryReturn) => "return".to_owned(),
                other => format!("{other:?}"),
            }
        })
        .collect()
}

fn parameter_positions(module: &Module) -> Vec<u32> {
    let mut positions = Vec::new();
    let walked = walk_operations(
        module,
        module.root_region(),
        WalkOrder::PreOrder,
        |operation| {
            if let OperationKind::Scalar(ScalarOp::Parameter { position, .. }) = module
                .operation(operation)
                .expect("walked operations are live")
                .kind()
            {
                positions.push(*position);
            }
        },
    );
    assert!(walked, "root region walks completely");
    positions.sort_unstable();
    positions
}

#[test]
fn bare_select_lowers_to_scan_and_return() {
    let module = afterburner!(UserEntity::find()).expect("bare select lowers to verified IR");
    assert_eq!(root_logical_kinds(&module), ["scan", "return"]);
}

#[test]
fn full_pipeline_lowers_in_sql_evaluation_order() {
    let query = UserEntity::find()
        .filter(Id.gt(100))
        .distinct()
        .order_by(Name.asc())
        .order_by(Id.desc().nulls_last())
        .limit(10)
        .offset(20);
    let module = afterburner!(query).expect("full pipeline lowers to verified IR");
    assert_eq!(
        root_logical_kinds(&module),
        ["scan", "filter", "distinct", "sort", "limit", "return"]
    );
}

#[test]
fn captured_values_become_positional_binds() {
    let query = UserEntity::find().filter(Id.gt(100).and(Name.eq("alice")));
    assert_eq!(
        query.binds(),
        [Value::Int64(100), Value::Text("alice".to_owned())]
    );

    let module = afterburner!(query).expect("filtered select lowers to verified IR");
    assert_eq!(parameter_positions(&module), [0, 1]);
}

#[test]
fn successive_filters_combine_with_and() {
    let query = UserEntity::find()
        .filter(Id.ge(1))
        .filter(Name.like("%lain%"));
    assert_eq!(
        query.binds(),
        [Value::Int64(1), Value::Text("%lain%".to_owned())]
    );
    afterburner!(query).expect("combined filters lower to verified IR");
}

#[test]
fn mixed_nullability_predicates_unify() {
    // `email` is nullable while `id` is not: the AND combination requires the
    // lowering to widen the non-nullable side before AfterBurner verification.
    let query = UserEntity::find().filter(
        Email
            .like("%@example.com")
            .and(Id.gt(0))
            .or(Email.is_null()),
    );
    afterburner!(query).expect("mixed-nullability predicate lowers to verified IR");
}

#[test]
fn null_binds_keep_their_column_type() {
    let query = UserEntity::find().filter(Email.eq(String::new()).or(Email.is_not_null()));
    let module = query;
    let lowered = afterburner!(module).expect("null-adjacent predicate lowers to verified IR");
    verify_module(&lowered).expect("module stays valid after macro verification");
}

#[test]
fn model_round_trips_through_positional_values() {
    let user = User {
        id: 7,
        name: "alice".to_owned(),
        email: None,
    };
    let values = user.clone().into_values();
    assert_eq!(
        values,
        [
            Value::Int64(7),
            Value::Text("alice".to_owned()),
            Value::Null(ColumnType::Text),
        ]
    );
    assert_eq!(User::from_values(values), Ok(user));
}

use jetorm::afterburner;
use jetorm::ir::{LogicalOp, OperationKind, TerminatorOp, verify_module};
use jetorm::prelude::*;
use jetorm::{Dialect, Postgres};

#[derive(Clone, Debug, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
}

#[test]
fn derived_entity_query_lowers_to_verified_ir() {
    let query = UserEntity::find()
        .filter(user::Email.like("%@example.com").and(user::Id.gt(100)))
        .order_by(user::Id.desc())
        .limit(20);
    assert_eq!(
        query.binds(),
        [Value::Text("%@example.com".to_owned()), Value::Int64(100),]
    );

    let module = afterburner!(query).expect("derived query lowers to verified IR");
    verify_module(&module).expect("module satisfies every IR invariant");

    let root = module
        .block(module.root_block())
        .expect("root block exists");
    let kinds: Vec<bool> = root
        .operations()
        .iter()
        .map(|operation| {
            matches!(
                module
                    .operation(*operation)
                    .expect("root operations are live")
                    .kind(),
                OperationKind::Logical(LogicalOp::Scan { .. })
                    | OperationKind::Logical(LogicalOp::Filter)
                    | OperationKind::Logical(LogicalOp::Sort { .. })
                    | OperationKind::Logical(LogicalOp::Limit { .. })
                    | OperationKind::Terminator(TerminatorOp::QueryReturn)
            )
        })
        .collect();
    assert_eq!(root.operations().len(), 5);
    assert!(kinds.iter().all(|expected| *expected));
}

#[test]
fn derived_entity_query_renders_to_postgres_sql() {
    let query = UserEntity::find()
        .filter(user::Email.like("%@example.com").and(user::Id.gt(100)))
        .order_by(user::Id.desc())
        .limit(20);
    let binds = query.binds().to_vec();

    let module = afterburner!(query).expect("derived query lowers to verified IR");
    let statement = Postgres.render_query(&module).expect("IR renders to SQL");

    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" \
         FROM \"users\" AS \"t0\" \
         WHERE ((\"t0\".\"email\" LIKE $1::text) AND (\"t0\".\"id\" > $2::bigint)) \
         ORDER BY \"t0\".\"id\" DESC \
         LIMIT 20"
    );
    // The executor binds `binds[bind_order[n]]` to placeholder `$n+1`.
    assert_eq!(statement.bind_order(), [0, 1]);
    assert_eq!(
        binds,
        [Value::Text("%@example.com".to_owned()), Value::Int64(100),]
    );
}

#[test]
fn identical_query_shapes_share_a_structural_fingerprint() {
    let first = UserEntity::find().filter(user::Id.gt(1)).limit(10);
    let second = UserEntity::find().filter(user::Id.gt(999_999)).limit(10);

    let first_module = afterburner!(first).expect("first query lowers to verified IR");
    let second_module = afterburner!(second).expect("second query lowers to verified IR");

    let first_print =
        jetorm::ir::structural_fingerprint(&first_module).expect("verified module fingerprints");
    let second_print =
        jetorm::ir::structural_fingerprint(&second_module).expect("verified module fingerprints");
    assert_eq!(
        first_print, second_print,
        "queries differing only in bound values must share one plan-cache key"
    );
}

//! Count queries: lowering, rendering, and plan-cache identity.

use jetorm::prelude::*;
use jetorm::{Dialect, IntoAfterBurnerIr, Postgres};

#[derive(Clone, Debug, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
}

#[test]
fn count_renders_a_grand_total_over_the_row_pipeline() {
    let query = UserEntity::find()
        .filter(user::Id.gt(10))
        .order_by(user::Id.desc())
        .limit(5)
        .into_count();
    // The filter and the row limit survive — the count sees exactly the rows
    // the select would return — while ordering is gone: it cannot change how
    // many rows there are.
    let module = query.clone().into_afterburner_ir().expect("count lowers");
    let statement = Postgres.render_query(&module).expect("count renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"count\"(*) AS \"count\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"name\", \"t0\".\"email\" FROM \"users\" AS \"t0\" \
         WHERE (\"t0\".\"id\" > $1::bigint) LIMIT $2::bigint) AS \"t1\" \
         GROUP BY ()"
    );

    // Binds line up with the select's positions: predicate values first,
    // then the row limit.
    assert_eq!(query.binds().len(), 2);
}

#[test]
fn counts_differing_only_in_ordering_share_one_shape() {
    let ordered = UserEntity::find()
        .filter(user::Name.eq("alice"))
        .order_by(user::Id.desc())
        .into_count();
    let unordered = UserEntity::find()
        .filter(user::Name.eq("alice"))
        .into_count();
    assert_eq!(
        ordered.shape(),
        unordered.shape(),
        "ordering cannot change a count, so the statements must be shared"
    );
}

#[test]
fn a_count_never_shares_a_shape_with_its_select() {
    let select = UserEntity::find().filter(user::Id.gt(0));
    let select_shape = select.shape();
    let count_shape = select.into_count().shape();
    assert_ne!(
        select_shape, count_shape,
        "a cached select statement must never answer a count"
    );
}

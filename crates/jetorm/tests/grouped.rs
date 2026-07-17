//! Grouped aggregates: typed promotion, lowering, and cache identity.

use jetorm::prelude::*;
use jetorm::{AggregateList, Dialect, IntoAfterBurnerIr, Postgres, avg, count_rows, max, sum};
use rust_decimal::Decimal;

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "orders")]
pub struct Order {
    #[jet(primary_key)]
    pub id: i64,
    pub customer: String,
    pub quantity: i32,
    pub price: Decimal,
    pub rating: Option<i32>,
}

#[test]
fn grouped_queries_render_where_before_group_by() {
    let query = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((count_rows(), sum(order::Quantity), avg(order::Rating)))
        .filter(order::Quantity.gt(0))
        .order_by_keys();
    assert_eq!(query.binds().len(), 1);
    let module = query.into_afterburner_ir().expect("grouped lowers");
    let statement = Postgres.render_query(&module).expect("grouped renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t2\".\"customer\", \"t2\".\"__agg_0_count\", \"t2\".\"__agg_1_sum\", \"t2\".\"__agg_2_avg\" FROM (SELECT \"t1\".\"customer\" AS \"customer\", \"count\"(*) AS \"__agg_0_count\", \"sum\"(\"t1\".\"quantity\") AS \"__agg_1_sum\", \"avg\"(\"t1\".\"rating\") AS \"__agg_2_avg\" FROM (SELECT \"t0\".\"id\", \"t0\".\"customer\", \"t0\".\"quantity\", \"t0\".\"price\", \"t0\".\"rating\" FROM \"orders\" AS \"t0\" WHERE (\"t0\".\"quantity\" > $1::integer)) AS \"t1\" GROUP BY \"t1\".\"customer\") AS \"t2\" ORDER BY \"t2\".\"customer\" ASC NULLS LAST"
    );
}

#[test]
fn having_filters_groups_by_their_aggregates() {
    let query = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((count_rows(), sum(order::Quantity)))
        .having(|(rows, total)| rows.ge(2).and(total.gt(5)));
    assert_eq!(
        query.binds(),
        [Value::Int64(2), Value::Int64(5)],
        "having values bind like every other value"
    );
    let module = query.clone().into_afterburner_ir().expect("having lowers");
    let statement = Postgres.render_query(&module).expect("having renders");
    let sql = statement.sql();
    assert!(
        sql.contains("\"__agg_0_count\" >= $1::bigint"),
        "the first aggregate is addressed by output position: {sql}"
    );
    assert!(
        sql.contains("\"__agg_1_sum\" > $2::bigint"),
        "the second aggregate compares at its promoted width: {sql}"
    );

    // The having predicate is part of the statement's identity.
    let without = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((count_rows(), sum(order::Quantity)));
    assert_ne!(query.shape(), without.shape());
}

#[test]
fn nullable_aggregates_ask_the_null_question_directly() {
    // eq(None) no longer compiles for nullable aggregates — is_null is
    // the correct spelling and renders as the SQL null test.
    let query = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((avg(order::Rating),))
        .having(|mean| mean.is_null());
    let module = query.into_afterburner_ir().expect("null test lowers");
    let statement = Postgres.render_query(&module).expect("null test renders");
    assert!(
        statement.sql().contains("\"__agg_0_avg\" IS NULL"),
        "the null test is IS NULL, not a never-true equality: {}",
        statement.sql()
    );
}

#[test]
fn limits_and_duplicate_keys_are_rejected_at_lowering() {
    // A limit's meaning under grouping is ambiguous — source rows or
    // groups — so it must not guess.
    let error = OrderEntity::find()
        .limit(10)
        .group_by((order::Customer,))
        .select_agg((count_rows(),))
        .into_afterburner_ir()
        .expect_err("limit over grouping is ambiguous");
    assert_eq!(error, jetorm::LoweringError::LimitOverGroup);

    let error = OrderEntity::find()
        .group_by((order::Customer, order::Customer))
        .select_agg((count_rows(),))
        .into_afterburner_ir()
        .expect_err("a repeated key is a user error, not a verifier error");
    assert_eq!(
        error,
        jetorm::LoweringError::DuplicateGroupKey { column: 1 }
    );
}

#[test]
fn grouped_shapes_separate_by_keys_aggregates_and_order() {
    let base = || OrderEntity::find().group_by((order::Customer,));
    let counted = base().select_agg((count_rows(),));
    let summed = base().select_agg((sum(order::Quantity),));
    assert_ne!(
        counted.shape(),
        summed.shape(),
        "different aggregates are different statements"
    );
    assert_ne!(
        counted.shape(),
        base().select_agg((count_rows(),)).order_by_keys().shape(),
        "key ordering changes the statement"
    );
    assert_eq!(
        counted.shape(),
        base().select_agg((count_rows(),)).shape(),
        "identical grouped queries share one statement"
    );
    assert_ne!(
        OrderEntity::find().shape(),
        counted.shape(),
        "a plain select never answers a grouped query"
    );
}

#[test]
fn aggregate_types_carry_database_promotion() {
    // sum(i32) is a 64-bit total; avg over a nullable i32 is an optional
    // exact decimal; max keeps the column's own type.
    trait RowTyped<Row> {}
    impl<E, K, A> RowTyped<(K::Row, A::Row)> for jetorm::GroupedSelect<E, K, A>
    where
        E: Entity,
        K: ColumnList<E>,
        A: AggregateList<E>,
    {
    }
    #[allow(clippy::type_complexity)]
    fn assert_row(query: &impl RowTyped<(String, (i64, Option<Decimal>, Decimal))>) {
        let _ = query;
    }
    let query = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((sum(order::Quantity), avg(order::Rating), max(order::Price)));
    assert_row(&query);
}

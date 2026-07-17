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
        "SELECT \"t2\".\"customer\", \"t2\".\"count_0\", \"t2\".\"sum_1\", \"t2\".\"avg_2\" FROM (SELECT \"t1\".\"customer\" AS \"customer\", \"count\"(*) AS \"count_0\", \"sum\"(\"t1\".\"quantity\") AS \"sum_1\", \"avg\"(\"t1\".\"rating\") AS \"avg_2\" FROM (SELECT \"t0\".\"id\", \"t0\".\"customer\", \"t0\".\"quantity\", \"t0\".\"price\", \"t0\".\"rating\" FROM \"orders\" AS \"t0\" WHERE (\"t0\".\"quantity\" > $1::integer)) AS \"t1\" GROUP BY \"t1\".\"customer\") AS \"t2\" ORDER BY \"t2\".\"customer\" ASC NULLS LAST"
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

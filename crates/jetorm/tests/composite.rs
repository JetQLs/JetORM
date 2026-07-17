//! Composite primary keys: typed identity, lookup, and keyset paging.

use jetorm::prelude::*;
use jetorm::{Dialect, IntoAfterBurnerIr, KeyedEntity, Postgres};

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "order_items")]
pub struct OrderItem {
    #[jet(primary_key)]
    pub order_id: i64,
    #[jet(primary_key)]
    pub line: i32,
    pub sku: String,
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key)]
    pub id: i64,
    pub name: String,
}

#[test]
fn keys_are_bare_when_single_and_tuples_when_composite() {
    assert_eq!(
        <UserEntity as KeyedEntity>::key_values(7),
        [Value::Int64(7)],
        "a single-column key is its bare value"
    );
    assert_eq!(
        <OrderItemEntity as KeyedEntity>::key_values((10, 2)),
        [Value::Int64(10), Value::Int32(2)],
        "a composite key is the tuple in declaration order"
    );
}

#[test]
fn find_by_key_chains_typed_equalities() {
    let query = OrderItemEntity::find_by_key((10, 2));
    assert_eq!(query.binds(), [Value::Int64(10), Value::Int32(2)]);
    let module = query.into_afterburner_ir().expect("key lookup lowers");
    let statement = Postgres.render_query(&module).expect("key lookup renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"order_id\", \"t0\".\"line\", \"t0\".\"sku\" \
         FROM \"order_items\" AS \"t0\" \
         WHERE ((\"t0\".\"order_id\" = $1::bigint) AND (\"t0\".\"line\" = $2::integer))"
    );

    // The single-column form is unchanged and equivalent to find_by_id.
    assert_eq!(
        UserEntity::find_by_key(7).shape(),
        UserEntity::find_by_id(7).shape(),
        "both lookups are the same statement"
    );
}

#[test]
fn composite_cursors_page_in_lexicographic_order() {
    let page = OrderItemEntity::find()
        .cursor_by((order_item::OrderId, order_item::Line))
        .after((10, 2))
        .first(20);
    let module = page
        .select()
        .clone()
        .into_afterburner_ir()
        .expect("composite cursor lowers");
    let statement = Postgres
        .render_query(&module)
        .expect("composite cursor renders");
    let sql = statement.sql();
    assert!(
        sql.contains(
            "((\"t0\".\"order_id\" > $1::bigint) OR \
             ((\"t0\".\"order_id\" = $2::bigint) AND (\"t0\".\"line\" > $3::integer)))"
        ),
        "the row-value comparison expands lexicographically: {sql}"
    );
    assert!(
        sql.contains("ORDER BY \"t0\".\"order_id\" ASC, \"t0\".\"line\" ASC"),
        "both key columns order the walk: {sql}"
    );

    // Every page of one walk shares a statement, as with single keys.
    let next = OrderItemEntity::find()
        .cursor_by((order_item::OrderId, order_item::Line))
        .after((99, 7))
        .first(20);
    assert_eq!(page.select().shape(), next.select().shape());
}

//! Grouped-aggregate execution against live PostgreSQL servers, through
//! the facade exactly as application code uses it.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm::prelude::*;
use jetorm::{Database, avg, count_rows, max, min, sum};
use rust_decimal::Decimal;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const POSTGRES_TAG: &str = "18-alpine";

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

async fn fresh_database() -> (
    testcontainers_modules::testcontainers::ContainerAsync<Postgres>,
    Database,
) {
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

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn groups_aggregate_at_their_promoted_types() {
    let (_container, db) = fresh_database().await;
    for statement in [
        "CREATE TABLE orders (
             id bigint PRIMARY KEY,
             customer text NOT NULL,
             quantity integer NOT NULL,
             price numeric NOT NULL,
             rating integer
         )",
        "INSERT INTO orders (id, customer, quantity, price, rating) VALUES
             (1, 'alice', 2, 19.99, 5),
             (2, 'alice', 3, 5.25, NULL),
             (3, 'bob', 1, 100.00, 4),
             (4, 'bob', 4, 0.75, 2),
             (5, 'carol', 10, 3.50, NULL)",
    ] {
        sqlx::query(statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("seed statement failed: {error}"));
    }

    let groups = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((count_rows(), sum(order::Quantity), max(order::Price)))
        .order_by_keys()
        .all(&db)
        .await
        .expect("grouped query runs");
    assert_eq!(
        groups,
        [
            ("alice".to_owned(), (2, 5, Decimal::new(1999, 2))),
            ("bob".to_owned(), (2, 5, Decimal::new(10000, 2))),
            ("carol".to_owned(), (1, 10, Decimal::new(350, 2))),
        ]
    );

    // A nullable column aggregates to None when every value in the group
    // is NULL — carol — and to a value when any is present.
    let ratings = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((min(order::Rating), avg(order::Rating)))
        .order_by_keys()
        .all(&db)
        .await
        .expect("nullable aggregates run");
    assert_eq!(ratings.len(), 3);
    assert_eq!(ratings[0].1.0, Some(5), "alice's single rating");
    assert_eq!(ratings[1].1.0, Some(2), "bob's lowest rating");
    assert_eq!(ratings[1].1.1, Some(Decimal::new(3, 0)), "bob averages 3");
    assert_eq!(ratings[2].1.0, None, "carol never rated");
    assert_eq!(ratings[2].1.1, None);

    // The filter is WHERE: rows drop out before grouping.
    let filtered = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((count_rows(),))
        .filter(order::Quantity.ge(3))
        .order_by_keys()
        .all(&db)
        .await
        .expect("filtered grouping runs");
    assert_eq!(
        filtered,
        [
            ("alice".to_owned(), 1),
            ("bob".to_owned(), 1),
            ("carol".to_owned(), 1),
        ]
    );

    // HAVING keeps only groups whose aggregates qualify: customers with
    // at least two orders totalling more than five items.
    let bulk = OrderEntity::find()
        .group_by((order::Customer,))
        .select_agg((count_rows(), sum(order::Quantity)))
        .having(|(orders, quantity)| orders.ge(2).and(quantity.gt(4)))
        .order_by_keys()
        .all(&db)
        .await
        .expect("having query runs");
    assert_eq!(
        bulk,
        [("alice".to_owned(), (2, 5)), ("bob".to_owned(), (2, 5))],
        "carol has one order and drops out"
    );

    db.close().await;
}

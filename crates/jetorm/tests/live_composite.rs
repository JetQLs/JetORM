//! Composite-key behavior against live PostgreSQL servers, through the
//! facade exactly as application code uses it.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm::Database;
use jetorm::prelude::*;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ImageExt;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

const POSTGRES_TAG: &str = "18-alpine";

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "order_items")]
pub struct OrderItem {
    #[jet(primary_key)]
    pub order_id: i64,
    #[jet(primary_key)]
    pub line: i32,
    pub sku: String,
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

fn item(order_id: i64, line: i32, sku: &str) -> OrderItem {
    OrderItem {
        order_id,
        line,
        sku: sku.to_owned(),
    }
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn composite_keys_identify_page_and_upsert() {
    let (_container, db) = fresh_database().await;
    sqlx::query(
        "CREATE TABLE order_items (
             order_id bigint NOT NULL,
             line integer NOT NULL,
             sku text NOT NULL,
             PRIMARY KEY (order_id, line)
         )",
    )
    .execute(db.pool())
    .await
    .expect("table creates");

    for row in [
        item(1, 1, "apple"),
        item(1, 2, "pear"),
        item(2, 1, "plum"),
        item(2, 2, "fig"),
    ] {
        OrderItemEntity::insert(row)
            .execute(&db)
            .await
            .expect("row inserts");
    }

    // The full key identifies exactly one row.
    let found = OrderItemEntity::find_by_key((1, 2))
        .one(&db)
        .await
        .expect("lookup runs");
    assert_eq!(found, Some(item(1, 2, "pear")));

    // A composite cursor walks in lexicographic order and resumes across
    // the leading column's boundary.
    let first = OrderItemEntity::find()
        .cursor_by((order_item::OrderId, order_item::Line))
        .first(3)
        .all(&db)
        .await
        .expect("first page fetches");
    let skus: Vec<&str> = first.iter().map(|row| row.sku.as_str()).collect();
    assert_eq!(skus, ["apple", "pear", "plum"]);

    let boundary = first.last().expect("page has rows");
    let second = OrderItemEntity::find()
        .cursor_by((order_item::OrderId, order_item::Line))
        .after((boundary.order_id, boundary.line))
        .first(3)
        .all(&db)
        .await
        .expect("second page fetches");
    let skus: Vec<&str> = second.iter().map(|row| row.sku.as_str()).collect();
    assert_eq!(skus, ["fig"], "the walk resumed past (2, 1) exactly");

    // Upsert conflicts on the whole composite key.
    OrderItemEntity::insert(item(1, 2, "quince"))
        .on_conflict_update()
        .execute(&db)
        .await
        .expect("composite upsert runs");
    let updated = OrderItemEntity::find_by_key((1, 2))
        .one(&db)
        .await
        .expect("lookup runs");
    assert_eq!(
        updated,
        Some(item(1, 2, "quince")),
        "the conflicting row updated in place"
    );
    let total = OrderItemEntity::find()
        .count(&db)
        .await
        .expect("count runs");
    assert_eq!(total, 4, "no duplicate row was created");

    db.close().await;
}

//! Pagination against live PostgreSQL servers, through the facade exactly
//! as application code uses it.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm::Database;
use jetorm::prelude::*;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

const POSTGRES_TAG: &str = "18-alpine";

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "items")]
pub struct Item {
    #[jet(primary_key)]
    pub id: i64,
    pub label: String,
}

async fn fresh_database() -> (ContainerAsync<Postgres>, Database) {
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

async fn seed(db: &Database) {
    sqlx::query("CREATE TABLE items (id bigint PRIMARY KEY, label text NOT NULL)")
        .execute(db.pool())
        .await
        .expect("table creates");
    // Seven rows, ids 1..=7.
    sqlx::query(
        "INSERT INTO items (id, label)
         SELECT n, 'item ' || n FROM generate_series(1, 7) AS n",
    )
    .execute(db.pool())
    .await
    .expect("rows insert");
}

fn ids(rows: &[Item]) -> Vec<i64> {
    rows.iter().map(|item| item.id).collect()
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn offset_pages_cover_every_row_with_totals() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    let paginator = ItemEntity::find().order_by(item::Id.asc()).paginate(&db, 3);
    assert_eq!(paginator.num_items().await.expect("total counts"), 7);
    assert_eq!(paginator.num_pages().await.expect("pages count"), 3);

    assert_eq!(
        ids(&paginator.fetch_page(0).await.expect("first page")),
        [1, 2, 3]
    );
    assert_eq!(
        ids(&paginator.fetch_page(1).await.expect("second page")),
        [4, 5, 6]
    );
    assert_eq!(
        ids(&paginator.fetch_page(2).await.expect("partial last page")),
        [7]
    );
    assert_eq!(
        ids(&paginator.fetch_page(3).await.expect("past the end")),
        [] as [i64; 0],
        "pages past the end are empty, not an error"
    );

    // A filtered paginator counts and pages only the matching rows.
    let filtered = ItemEntity::find()
        .filter(item::Id.gt(4))
        .order_by(item::Id.asc())
        .paginate(&db, 2);
    assert_eq!(filtered.num_items().await.expect("filtered total"), 3);
    assert_eq!(filtered.num_pages().await.expect("filtered pages"), 2);
    assert_eq!(
        ids(&filtered.fetch_page(1).await.expect("filtered page")),
        [7]
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn cursor_pages_resume_from_keys_in_both_directions() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    // Walk forward: each page resumes after the previous page's last key.
    let first = ItemEntity::find()
        .cursor_by((item::Id,))
        .first(3)
        .all(&db)
        .await
        .expect("first page");
    assert_eq!(ids(&first), [1, 2, 3]);

    let second = ItemEntity::find()
        .cursor_by((item::Id,))
        .after(first.last().expect("page has rows").id)
        .first(3)
        .all(&db)
        .await
        .expect("second page");
    assert_eq!(ids(&second), [4, 5, 6]);

    // The tail, in ascending order despite fetching descending.
    let tail = ItemEntity::find()
        .cursor_by((item::Id,))
        .last(2)
        .all(&db)
        .await
        .expect("tail page");
    assert_eq!(ids(&tail), [6, 7]);

    // Bounded window: between two keys, from the back.
    let window = ItemEntity::find()
        .cursor_by((item::Id,))
        .after(1)
        .before(6)
        .last(2)
        .all(&db)
        .await
        .expect("bounded window");
    assert_eq!(ids(&window), [4, 5]);

    db.close().await;
}

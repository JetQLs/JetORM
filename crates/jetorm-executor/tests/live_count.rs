//! Count execution against live PostgreSQL servers.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm_derive::JetModel;
use jetorm_executor::{Database, SelectExecute};
use jetorm_query::{ColumnExt, EntityQuery};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

const POSTGRES_TAG: &str = "18-alpine";

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "events", crate_path = "::jetorm_entity")]
pub struct Event {
    #[jet(primary_key)]
    pub id: i64,
    pub kind: String,
    pub payload: Option<String>,
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
    for statement in [
        "CREATE TABLE events (id bigint PRIMARY KEY, kind text NOT NULL, payload text)",
        "INSERT INTO events (id, kind, payload) VALUES
             (1, 'click', 'a'),
             (2, 'click', NULL),
             (3, 'view', 'b'),
             (4, 'click', 'c'),
             (5, 'view', NULL)",
    ] {
        sqlx::query(statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("seed statement failed: {error}"));
    }
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn count_answers_without_fetching_rows() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    let total = EventEntity::find().count(&db).await.expect("total counts");
    assert_eq!(total, 5);

    let clicks = EventEntity::find()
        .filter(event::Kind.eq("click"))
        .count(&db)
        .await
        .expect("filtered count");
    assert_eq!(clicks, 3);

    // The count sees exactly the rows the select would return: a row limit
    // caps it, and ordering neither breaks nor changes it.
    let capped = EventEntity::find()
        .filter(event::Kind.eq("click"))
        .order_by(event::Id.desc())
        .limit(2)
        .count(&db)
        .await
        .expect("limited count");
    assert_eq!(capped, 2);

    let with_payload = EventEntity::find()
        .filter(event::Payload.is_not_null())
        .count(&db)
        .await
        .expect("null-filtered count");
    assert_eq!(with_payload, 3);

    let none = EventEntity::find()
        .filter(event::Kind.eq("purchase"))
        .count(&db)
        .await
        .expect("empty count");
    assert_eq!(none, 0, "an empty match counts zero, not a missing row");

    db.close().await;
}

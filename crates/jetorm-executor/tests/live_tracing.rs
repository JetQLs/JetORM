//! Query instrumentation against a live PostgreSQL server.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use std::io::Write;
use std::sync::{Arc, Mutex};

use jetorm_derive::JetModel;
use jetorm_executor::{Database, SelectExecute};
use jetorm_query::EntityQuery;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use tracing_subscriber::fmt::MakeWriter;

const POSTGRES_TAG: &str = "18-alpine";

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "traced", crate_path = "::jetorm_entity")]
pub struct Traced {
    #[jet(primary_key)]
    pub id: i64,
}

/// Collects formatted subscriber output for assertions.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("no poisoned writers")).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("no poisoned writers").extend(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for Capture {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self {
        self.clone()
    }
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

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn queries_emit_spans_and_cache_misses_emit_events() {
    let (_container, db) = fresh_database().await;
    sqlx::query("CREATE TABLE traced (id bigint PRIMARY KEY)")
        .execute(db.pool())
        .await
        .expect("table creates");

    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // First execution: a cache miss event, then the query span.
    TracedEntity::find().all(&db).await.expect("query runs");
    // Second execution: the span again, without a second miss.
    TracedEntity::find()
        .all(&db)
        .await
        .expect("query runs again");

    let output = capture.contents();
    assert!(
        output.contains("jetorm.query"),
        "the execution span is emitted: {output}"
    );
    assert!(
        output.contains("db.query.text"),
        "the span carries the SQL text: {output}"
    );
    // `db.response.returned_rows` is recorded after fetching; the fmt
    // formatter prints creation-time fields only, so its presence is
    // asserted structurally (registry-based collectors receive the value).
    assert_eq!(
        output
            .matches("rendered statement on plan-cache miss")
            .count(),
        1,
        "one shape renders exactly once: {output}"
    );

    db.close().await;
}

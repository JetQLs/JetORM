//! Round-trips every supported column type through a live PostgreSQL server.
//!
//! Golden-SQL tests pin the spelling of a type; only a real server proves the
//! whole chain agrees on it — DDL type name, parameter cast, driver bind, and
//! driver decode. A mismatch anywhere in that chain is invisible in-process.
//! Run via `cargo xtask test-live` (needs a Docker daemon).
//!
//! Fixture rows are inserted with `sqlx` directly rather than through
//! JetORM's own bind path, so the decode assertions cannot be satisfied by a
//! bug that is symmetric across bind and decode.

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use jetorm_dialect::postgres::ddl::render_change;
use jetorm_entity::Column;
use jetorm_executor::{Database, SelectExecute};
use jetorm_query::{ColumnExt, EntityQuery};
use jetorm_schema::{SchemaChange, TableDef};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use uuid::Uuid;

const POSTGRES_TAG: &str = "18-alpine";

/// One row holding every column type JetORM supports, in both a required and
/// an optional flavour.
///
/// `crate_path` points the generated code at `jetorm-entity` directly: every
/// item the derive emits lives there, so a test below the facade does not
/// need to depend on it.
#[derive(Clone, Debug, PartialEq, jetorm_derive::JetModel)]
#[jet(table = "every_type", crate_path = "::jetorm_entity")]
pub struct EveryType {
    #[jet(primary_key)]
    pub id: i64,
    pub flag: bool,
    pub small: i16,
    pub medium: i32,
    pub large: i64,
    pub single: f32,
    pub double: f64,
    pub text: String,
    pub blob: Vec<u8>,
    pub day: NaiveDate,
    pub clock: NaiveTime,
    pub naive_moment: NaiveDateTime,
    pub utc_moment: DateTime<Utc>,
    pub identifier: Uuid,
    pub document: serde_json::Value,
    pub maybe_flag: Option<bool>,
    pub maybe_text: Option<String>,
    pub maybe_moment: Option<DateTime<Utc>>,
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

fn sample_row() -> EveryType {
    EveryType {
        id: 1,
        flag: true,
        small: i16::MIN,
        medium: i32::MIN,
        large: i64::MAX,
        single: 1.5,
        double: -2.25,
        // Quotes, a backslash, and non-ASCII must survive binding.
        text: "o'brien \u{2603} \\".to_owned(),
        blob: vec![0, 1, 254, 255],
        day: NaiveDate::from_ymd_opt(2026, 7, 17).expect("valid date"),
        clock: NaiveTime::from_hms_micro_opt(23, 59, 58, 123_456).expect("valid time"),
        naive_moment: NaiveDate::from_ymd_opt(1999, 12, 31)
            .expect("valid date")
            .and_hms_micro_opt(23, 59, 59, 999_999)
            .expect("valid time"),
        utc_moment: DateTime::<Utc>::from_timestamp(1_752_710_400, 123_456_000)
            .expect("valid timestamp"),
        identifier: Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef),
        document: serde_json::json!({ "nested": [1, true, null], "s": "v" }),
        maybe_flag: None,
        maybe_text: Some("present".to_owned()),
        maybe_moment: None,
    }
}

/// Creates the table from the entity's own metadata and inserts one fixture
/// row through `sqlx`.
async fn seed(db: &Database, row: &EveryType) {
    let table = TableDef::from_entity::<EveryTypeEntity>();
    for statement in render_change(&SchemaChange::CreateTable(table)).expect("create renders") {
        sqlx::query(&statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL rejected {statement:?}: {error}"));
    }

    sqlx::query(
        "INSERT INTO every_type (\
         id, flag, small, medium, large, single, double, text, blob, day, clock, \
         naive_moment, utc_moment, identifier, document, maybe_flag, maybe_text, maybe_moment) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)",
    )
    .bind(row.id)
    .bind(row.flag)
    .bind(row.small)
    .bind(row.medium)
    .bind(row.large)
    .bind(row.single)
    .bind(row.double)
    .bind(row.text.clone())
    .bind(row.blob.clone())
    .bind(row.day)
    .bind(row.clock)
    .bind(row.naive_moment)
    .bind(row.utc_moment)
    .bind(row.identifier)
    .bind(row.document.clone())
    .bind(row.maybe_flag)
    .bind(row.maybe_text.clone())
    .bind(row.maybe_moment)
    .execute(db.pool())
    .await
    .expect("fixture row inserts");
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn every_column_type_decodes_back_into_its_model() {
    let (_container, db) = fresh_database().await;
    let row = sample_row();
    seed(&db, &row).await;

    let fetched = EveryTypeEntity::find()
        .one(&db)
        .await
        .expect("select executes")
        .expect("the fixture row is found");
    assert_eq!(
        fetched, row,
        "every column type must decode to the value the database stores"
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn every_column_type_binds_through_its_parameter_cast() {
    // Each filter renders `$n::<type>` against a column whose DDL type came
    // from the same metadata. PostgreSQL rejects a cast it cannot apply to
    // the column, so a mapping disagreement fails here rather than silently.
    let (_container, db) = fresh_database().await;
    let row = sample_row();
    seed(&db, &row).await;

    macro_rules! assert_matches_fixture {
        ($column:path, $value:expr) => {{
            let found = EveryTypeEntity::find()
                .filter($column.eq($value))
                .one(&db)
                .await
                .unwrap_or_else(|error| {
                    panic!("filter on {:?} failed: {error}", <$column>::meta().name())
                });
            assert!(
                found.is_some(),
                "filter on {:?} matched no row",
                <$column>::meta().name()
            );
        }};
    }

    assert_matches_fixture!(every_type::Id, row.id);
    assert_matches_fixture!(every_type::Flag, row.flag);
    assert_matches_fixture!(every_type::Small, row.small);
    assert_matches_fixture!(every_type::Medium, row.medium);
    assert_matches_fixture!(every_type::Large, row.large);
    assert_matches_fixture!(every_type::Single, row.single);
    assert_matches_fixture!(every_type::Double, row.double);
    assert_matches_fixture!(every_type::Text, row.text.clone());
    assert_matches_fixture!(every_type::Blob, row.blob.clone());
    assert_matches_fixture!(every_type::Day, row.day);
    assert_matches_fixture!(every_type::Clock, row.clock);
    assert_matches_fixture!(every_type::NaiveMoment, row.naive_moment);
    assert_matches_fixture!(every_type::UtcMoment, row.utc_moment);
    assert_matches_fixture!(every_type::Identifier, row.identifier);
    assert_matches_fixture!(every_type::Document, row.document.clone());
    assert_matches_fixture!(every_type::MaybeText, "present".to_owned());

    // Null tests reach the columns whose values are absent.
    for found in [
        EveryTypeEntity::find()
            .filter(every_type::MaybeFlag.is_null())
            .one(&db)
            .await
            .expect("boolean null filter executes"),
        EveryTypeEntity::find()
            .filter(every_type::MaybeMoment.is_null())
            .one(&db)
            .await
            .expect("timestamp null filter executes"),
    ] {
        assert!(found.is_some(), "null filter must match the fixture row");
    }

    db.close().await;
}

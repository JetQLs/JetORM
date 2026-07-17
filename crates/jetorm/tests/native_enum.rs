//! Native enum types: metadata collection, diffing, and DDL.

use jetorm::prelude::*;
use jetorm_schema::{SchemaChange, SchemaSet, diff};

#[derive(Clone, Copy, Debug, PartialEq, JetEnum)]
#[jet(native = "article_status")]
pub enum Status {
    Draft,
    #[jet(rename = "live")]
    Published,
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "articles")]
pub struct Article {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub status: Status,
    pub note: Option<String>,
}

#[test]
fn native_enums_carry_their_type_through_the_metadata() {
    assert_eq!(Status::TYPE_NAME, Some("article_status"));
    assert_eq!(Status::ENUM_VARIANTS, ["draft", "live"]);
    assert_eq!(
        ArticleEntity::COLUMNS[1].type_name(),
        Some("article_status"),
        "the column knows its named type"
    );
    assert_eq!(ArticleEntity::COLUMNS[2].type_name(), None);

    let mut schema = SchemaSet::new();
    schema.insert_entity::<ArticleEntity>();
    assert_eq!(
        schema
            .enum_variants("article_status")
            .map(<[String]>::to_vec),
        Some(vec!["draft".to_owned(), "live".to_owned()]),
        "insert_entity collects the enum definition"
    );
}

#[test]
fn enum_types_bracket_the_diff_and_evolve_by_appending() {
    let mut target = SchemaSet::new();
    target.insert_entity::<ArticleEntity>();

    let changes = diff(&SchemaSet::new(), &target);
    assert!(
        matches!(changes.changes().first(), Some(SchemaChange::CreateEnum { name, .. }) if name == "article_status"),
        "the type exists before any column can take it"
    );
    let mut replayed = SchemaSet::new();
    replayed
        .apply_all(changes.changes())
        .expect("the diff applies to its own source");
    assert_eq!(replayed, target);

    // Appending a variant evolves in place.
    let mut grown = target.clone();
    grown.insert_enum(
        "article_status",
        ["draft", "live", "retracted"].map(str::to_owned),
    );
    let changes = diff(&target, &grown);
    assert_eq!(
        changes.changes(),
        [SchemaChange::AddEnumVariant {
            name: "article_status".to_owned(),
            variant: "retracted".to_owned(),
        }]
    );

    // Dropping the whole schema drops the type last, after its column.
    let teardown = diff(&target, &SchemaSet::new());
    assert!(
        matches!(teardown.changes().last(), Some(SchemaChange::DropEnum { name }) if name == "article_status"),
        "the type outlives everything that uses it"
    );
    let mut replayed = target.clone();
    replayed
        .apply_all(teardown.changes())
        .expect("teardown applies in order");
    assert_eq!(replayed, SchemaSet::new());
}

#[test]
fn a_used_enum_type_refuses_to_drop() {
    let mut schema = SchemaSet::new();
    schema.insert_entity::<ArticleEntity>();
    let error = schema
        .apply(&SchemaChange::DropEnum {
            name: "article_status".to_owned(),
        })
        .expect_err("the column still uses the type");
    assert!(matches!(error, jetorm_schema::ApplyError::EnumInUse { .. }));
}

#[test]
fn enum_ddl_spells_create_type_and_typed_columns() {
    use jetorm_dialect::postgres::ddl::render_changes;

    let mut target = SchemaSet::new();
    target.insert_entity::<ArticleEntity>();
    let changes = diff(&SchemaSet::new(), &target);
    let statements = render_changes(changes.changes()).expect("changes render");
    assert_eq!(
        statements[0],
        "CREATE TYPE \"article_status\" AS ENUM ('draft', 'live')"
    );
    assert!(
        statements[1].contains("\"status\" \"article_status\""),
        "the column takes the named type: {}",
        statements[1]
    );
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn enum_ddl_round_trips_on_live_postgres() {
    use jetorm::Database;
    use jetorm_dialect::postgres::ddl::render_changes;
    use testcontainers_modules::postgres::Postgres;
    use testcontainers_modules::testcontainers::ImageExt;
    use testcontainers_modules::testcontainers::runners::AsyncRunner;

    let container = Postgres::default()
        .with_tag("18-alpine")
        .start()
        .await
        .expect("PostgreSQL test container starts (is Docker running?)");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container maps the PostgreSQL port");
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = Database::connect(&url).await.expect("connects");

    let mut target = SchemaSet::new();
    target.insert_entity::<ArticleEntity>();
    let changes = diff(&SchemaSet::new(), &target);
    for statement in render_changes(changes.changes()).expect("changes render") {
        sqlx::query(&statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL rejected {statement:?}: {error}"));
    }

    // The typed column accepts declared variants and rejects others.
    sqlx::query("INSERT INTO articles (status) VALUES ('live')")
        .execute(db.pool())
        .await
        .expect("a declared variant inserts");
    let rejected = sqlx::query("INSERT INTO articles (status) VALUES ('retracted')")
        .execute(db.pool())
        .await;
    assert!(rejected.is_err(), "an undeclared variant is rejected");

    // Appending a variant takes effect.
    let mut grown = target.clone();
    grown.insert_enum(
        "article_status",
        ["draft", "live", "retracted"].map(str::to_owned),
    );
    for statement in render_changes(diff(&target, &grown).changes()).expect("append renders") {
        sqlx::query(&statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL rejected {statement:?}: {error}"));
    }
    sqlx::query("INSERT INTO articles (status) VALUES ('retracted')")
        .execute(db.pool())
        .await
        .expect("the appended variant inserts");

    db.close().await;
}

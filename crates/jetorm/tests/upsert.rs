//! Upsert: explicit conflict arbiters, chosen update columns, and the
//! primary-key convenience form.

use jetorm::prelude::*;
use jetorm::{Dialect, IntoAfterBurnerIr, PlanCache, Postgres};

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "items")]
pub struct Item {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    #[jet(unique)]
    pub sku: String,
    pub price: rust_decimal::Decimal,
    pub stock: i32,
}

/// Natural-key entity for the primary-key upsert convenience.
#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "settings")]
pub struct Setting {
    #[jet(primary_key)]
    pub key: String,
    pub value: String,
}

fn item(sku: &str, price: i64, stock: i32) -> Item {
    Item {
        id: 0,
        sku: sku.to_owned(),
        price: rust_decimal::Decimal::from(price),
        stock,
    }
}

#[test]
fn a_targeted_update_assigns_only_the_chosen_columns() {
    let insert = ItemEntity::insert(item("a", 10, 1))
        .on_conflict((item::Sku,))
        .update_columns((item::Price,))
        .returning();
    let module = insert.into_afterburner_ir().expect("upsert lowers");
    let sql = Postgres
        .render_query(&module)
        .expect("upsert renders")
        .sql()
        .to_owned();
    assert!(
        sql.contains("ON CONFLICT (\"sku\") DO UPDATE SET \"price\" = EXCLUDED.\"price\""),
        "the arbiter and assignment are explicit: {sql}"
    );
    assert!(
        !sql.contains("\"stock\" = EXCLUDED"),
        "unchosen columns stay untouched: {sql}"
    );
}

#[test]
fn a_targeted_ignore_skips_conflicting_rows() {
    let insert = ItemEntity::insert(item("a", 10, 1))
        .on_conflict((item::Sku,))
        .ignore()
        .returning();
    let module = insert.into_afterburner_ir().expect("ignore lowers");
    let sql = Postgres
        .render_query(&module)
        .expect("ignore renders")
        .sql()
        .to_owned();
    assert!(
        sql.contains("ON CONFLICT (\"sku\") DO NOTHING"),
        "the arbiter restricts the skip: {sql}"
    );
}

#[test]
fn upsert_arbitrates_on_the_key_and_updates_everything_else() {
    let module = SettingEntity::upsert(Setting {
        key: "theme".to_owned(),
        value: "dark".to_owned(),
    })
    .returning()
    .into_afterburner_ir()
    .expect("upsert lowers");
    let sql = Postgres
        .render_query(&module)
        .expect("upsert renders")
        .sql()
        .to_owned();
    assert!(
        sql.contains("ON CONFLICT (\"key\") DO UPDATE SET \"value\" = EXCLUDED.\"value\""),
        "the key arbitrates and every non-key column updates: {sql}"
    );
}

#[test]
fn a_generated_key_cannot_arbitrate_an_upsert() {
    // Item's id is auto-incrementing, so it is never inserted — a key
    // upsert could never conflict, and the dead update arm is refused.
    let result = ItemEntity::upsert(item("a", 10, 1)).into_afterburner_ir();
    assert!(
        matches!(
            result,
            Err(jetorm::LoweringError::UpsertKeyGenerated { ref column }) if column == "id"
        ),
        "{result:?}"
    );
}

#[test]
fn nonsense_update_lists_are_rejected_before_the_database() {
    // Assigning a conflict-target column to itself.
    let self_assign = ItemEntity::insert(item("a", 10, 1))
        .on_conflict((item::Sku,))
        .update_columns((item::Sku,))
        .into_afterburner_ir();
    assert!(
        matches!(
            self_assign,
            Err(jetorm::LoweringError::UpsertAssignsTarget { ref column }) if column == "sku"
        ),
        "{self_assign:?}"
    );

    // Assigning the excluded auto-increment value.
    let generated = ItemEntity::insert(item("a", 10, 1))
        .on_conflict((item::Sku,))
        .update_columns((item::Id,))
        .into_afterburner_ir();
    assert!(
        matches!(
            generated,
            Err(jetorm::LoweringError::UpsertAssignsGenerated { ref column }) if column == "id"
        ),
        "{generated:?}"
    );
}

#[test]
fn different_conflict_policies_never_share_a_plan() {
    let cache = PlanCache::new();
    let update = cache
        .statement(
            &ItemEntity::insert(item("a", 10, 1))
                .on_conflict((item::Sku,))
                .update_columns((item::Price,)),
        )
        .expect("update policy renders");
    let ignore = cache
        .statement(
            &ItemEntity::insert(item("a", 10, 1))
                .on_conflict((item::Sku,))
                .ignore(),
        )
        .expect("ignore policy renders");
    let wider = cache
        .statement(
            &ItemEntity::insert(item("a", 10, 1))
                .on_conflict((item::Sku,))
                .update_columns((item::Price, item::Stock)),
        )
        .expect("wider update renders");
    assert_ne!(update.sql(), ignore.sql());
    assert_ne!(update.sql(), wider.sql());
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn upserts_round_trip_on_live_postgres() {
    use jetorm::Database;
    use jetorm_schema::{SchemaSet, diff};
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
    target.insert_entity::<ItemEntity>();
    target.insert_entity::<SettingEntity>();
    for statement in
        jetorm_dialect::postgres::ddl::render_changes(diff(&SchemaSet::new(), &target).changes())
            .expect("schema renders")
    {
        sqlx::query(&statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("PostgreSQL rejected {statement:?}: {error}"));
    }

    let created = ItemEntity::insert(item("widget", 10, 5))
        .returning()
        .all(&db)
        .await
        .expect("seed insert runs");
    let id = created[0].id;

    // Conflict on the unique column updates only the chosen column.
    let updated = ItemEntity::insert(item("widget", 12, 99))
        .on_conflict((item::Sku,))
        .update_columns((item::Price,))
        .returning()
        .all(&db)
        .await
        .expect("targeted upsert runs");
    assert_eq!(
        updated[0].id, id,
        "the existing row was updated, not replaced"
    );
    assert_eq!(updated[0].price, rust_decimal::Decimal::from(12));
    assert_eq!(updated[0].stock, 5, "the unchosen column kept its value");

    // Ignore leaves the row alone and reports no returned rows.
    let ignored = ItemEntity::insert(item("widget", 99, 99))
        .on_conflict((item::Sku,))
        .ignore()
        .returning()
        .all(&db)
        .await
        .expect("ignore runs");
    assert!(ignored.is_empty(), "a skipped conflict returns nothing");

    // The convenience form arbitrates on a natural key: absent inserts,
    // present updates everything else.
    let setting = |value: &str| Setting {
        key: "theme".to_owned(),
        value: value.to_owned(),
    };
    let first = SettingEntity::upsert(setting("dark"))
        .returning()
        .all(&db)
        .await
        .expect("fresh upsert inserts");
    assert_eq!(first[0].value, "dark");
    let second = SettingEntity::upsert(setting("light"))
        .returning()
        .all(&db)
        .await
        .expect("conflicting upsert updates");
    assert_eq!(second[0].value, "light");
    let count = SettingEntity::find().count(&db).await.expect("count runs");
    assert_eq!(count, 1, "one key, one row");

    db.close().await;
}

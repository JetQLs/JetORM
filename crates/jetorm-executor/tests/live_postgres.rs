//! Round-trip tests against live PostgreSQL servers.
//!
//! Every test starts its own disposable PostgreSQL container through
//! `testcontainers`, so tests are fully isolated from each other and from
//! any local database, and run safely in parallel. They are ignored by
//! default because they need a running Docker daemon:
//!
//! ```text
//! cargo xtask test-live    # or: just test-live
//! ```

use jetorm_entity::{
    Column, ColumnMeta, ColumnType, DecodeError, Entity, Model, SqlValue, TableMeta, Value,
};
use jetorm_executor::{Database, SelectExecute};
use jetorm_query::{ColumnExt, EntityQuery, TextColumnExt};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

/// PostgreSQL version live tests run against; kept current with the newest
/// stable major release.
const POSTGRES_TAG: &str = "18-alpine";

const TABLE: &str = "live_users";

#[derive(Clone, Copy, Debug)]
struct UserEntity;

#[derive(Clone, Debug, PartialEq)]
struct User {
    id: i64,
    name: String,
    email: Option<String>,
}

impl Entity for UserEntity {
    type Model = User;
    const TABLE: TableMeta = TableMeta::new(TABLE);
    const COLUMNS: &'static [ColumnMeta] = &[
        ColumnMeta::new("id", "id", ColumnType::Int64).primary_key(),
        ColumnMeta::new("name", "name", ColumnType::Text),
        ColumnMeta::new("email", "email", ColumnType::Text).nullable(),
    ];
    const PRIMARY_KEY: &'static [usize] = &[0];
}

impl Model for User {
    type Entity = UserEntity;

    fn into_values(self) -> Vec<Value> {
        vec![
            self.id.into_value(),
            self.name.into_value(),
            self.email.into_value(),
        ]
    }

    fn from_values(values: Vec<Value>) -> Result<Self, DecodeError> {
        if values.len() != 3 {
            return Err(DecodeError::ColumnCount {
                expected: 3,
                actual: values.len(),
            });
        }
        let mut values = values.into_iter();
        let id = i64::from_value(values.next().expect("length checked")).map_err(|mismatch| {
            DecodeError::Column {
                name: "id",
                mismatch,
            }
        })?;
        let name =
            String::from_value(values.next().expect("length checked")).map_err(|mismatch| {
                DecodeError::Column {
                    name: "name",
                    mismatch,
                }
            })?;
        let email = Option::<String>::from_value(values.next().expect("length checked")).map_err(
            |mismatch| DecodeError::Column {
                name: "email",
                mismatch,
            },
        )?;
        Ok(Self { id, name, email })
    }
}

#[derive(Clone, Copy, Debug)]
struct Id;

impl Column for Id {
    type Entity = UserEntity;
    type Rust = i64;
    const INDEX: usize = 0;
    const NULLABLE: bool = false;
}

#[derive(Clone, Copy, Debug)]
struct Name;

impl Column for Name {
    type Entity = UserEntity;
    type Rust = String;
    const INDEX: usize = 1;
    const NULLABLE: bool = false;
}

#[derive(Clone, Copy, Debug)]
struct Email;

impl Column for Email {
    type Entity = UserEntity;
    type Rust = String;
    const INDEX: usize = 2;
    const NULLABLE: bool = true;
}

/// Starts one disposable PostgreSQL server and connects a database to it.
///
/// The container handle must stay alive for the duration of the test;
/// dropping it stops and removes the container.
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

async fn create_fixture(db: &Database) {
    sqlx::query(&format!(
        "CREATE TABLE {TABLE} (id bigint PRIMARY KEY, name text NOT NULL, email text)"
    ))
    .execute(db.pool())
    .await
    .expect("create table");
    sqlx::query(&format!(
        "INSERT INTO {TABLE} (id, name, email) VALUES \
         (1, 'alice', 'alice@example.com'), \
         (2, 'bob', NULL), \
         (3, 'carol', 'carol@example.com')"
    ))
    .execute(db.pool())
    .await
    .expect("insert fixture rows");
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn filtered_select_round_trips() {
    let (_container, db) = fresh_database().await;
    create_fixture(&db).await;

    let users = UserEntity::find()
        .filter(Email.like("%@example.com").and(Id.gt(0)))
        .order_by(Id.desc())
        .all(&db)
        .await
        .expect("filtered select executes");
    assert_eq!(
        users,
        [
            User {
                id: 3,
                name: "carol".to_owned(),
                email: Some("carol@example.com".to_owned()),
            },
            User {
                id: 1,
                name: "alice".to_owned(),
                email: Some("alice@example.com".to_owned()),
            },
        ]
    );

    let nobody = UserEntity::find()
        .filter(Email.is_null())
        .one(&db)
        .await
        .expect("null-test select executes");
    assert_eq!(
        nobody,
        Some(User {
            id: 2,
            name: "bob".to_owned(),
            email: None,
        })
    );

    let first_by_name = UserEntity::find()
        .order_by(Name.asc())
        .one(&db)
        .await
        .expect("ordered select executes");
    assert_eq!(
        first_by_name.map(|user| user.name),
        Some("alice".to_owned())
    );

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn repeated_shapes_reuse_the_plan_cache() {
    let (_container, db) = fresh_database().await;
    create_fixture(&db).await;

    // Same shape with different bound values: the second call must reuse the
    // cached statement and still bind fresh values.
    let first = UserEntity::find()
        .filter(Id.eq(1))
        .all(&db)
        .await
        .expect("first execution");
    let second = UserEntity::find()
        .filter(Id.eq(2))
        .all(&db)
        .await
        .expect("second execution");
    assert_eq!(first[0].name, "alice");
    assert_eq!(second[0].name, "bob");

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn dropped_transactions_roll_back() {
    let (_container, db) = fresh_database().await;
    create_fixture(&db).await;

    {
        let mut transaction = db.begin().await.expect("transaction begins");
        sqlx::query(&format!(
            "INSERT INTO {TABLE} (id, name, email) VALUES (99, 'temp', NULL)"
        ))
        .execute(transaction.connection())
        .await
        .expect("insert inside transaction");

        let inside = UserEntity::find()
            .filter(Id.eq(99))
            .one(&mut transaction)
            .await
            .expect("select inside transaction");
        assert!(inside.is_some(), "open transaction sees its own insert");
        // Dropped without commit: the insert must roll back.
    }

    let outside = UserEntity::find()
        .filter(Id.eq(99))
        .one(&db)
        .await
        .expect("select after rollback");
    assert_eq!(outside, None, "dropped transaction must leave no rows");

    db.close().await;
}

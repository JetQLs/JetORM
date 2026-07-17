//! Partial-model execution against live PostgreSQL servers, through the
//! facade exactly as application code uses it.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm::prelude::*;
use jetorm::{Database, JetPartial};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

const POSTGRES_TAG: &str = "18-alpine";

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key)]
    pub id: i64,
    pub name: String,
    pub email: Option<String>,
    pub age: i32,
}

#[derive(Clone, Debug, PartialEq, JetPartial)]
#[jet(columns = "user")]
pub struct UserSummary {
    pub id: i64,
    pub name: String,
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
async fn partials_fetch_and_decode_through_the_facade() {
    let (_container, db) = fresh_database().await;
    for statement in [
        "CREATE TABLE users (
             id bigint PRIMARY KEY,
             name text NOT NULL,
             email text,
             age integer NOT NULL
         )",
        "INSERT INTO users (id, name, email, age) VALUES
             (1, 'alice', 'alice@example.com', 34),
             (2, 'bob', NULL, 17),
             (3, 'carol', NULL, 25)",
    ] {
        sqlx::query(statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("seed statement failed: {error}"));
    }

    let adults: Vec<UserSummary> = UserEntity::find()
        .select_as::<UserSummary>()
        .filter(user::Age.ge(18))
        .order_by(user::Id.asc())
        .all(&db)
        .await
        .expect("partials fetch");
    assert_eq!(
        adults,
        [
            UserSummary {
                id: 1,
                name: "alice".to_owned(),
            },
            UserSummary {
                id: 3,
                name: "carol".to_owned(),
            },
        ]
    );

    // `one` flows through the same decode path.
    let youngest: Option<UserSummary> = UserEntity::find()
        .select_as::<UserSummary>()
        .order_by(user::Age.asc())
        .one(&db)
        .await
        .expect("single partial fetches");
    assert_eq!(
        youngest,
        Some(UserSummary {
            id: 2,
            name: "bob".to_owned(),
        })
    );

    db.close().await;
}

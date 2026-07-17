//! Relation-join execution against live PostgreSQL servers, through the
//! facade exactly as application code uses it.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm::prelude::*;
use jetorm::{Database, Inverse};
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
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "posts")]
pub struct Post {
    #[jet(primary_key)]
    pub id: i64,
    pub title: String,
    #[jet(references = "user::Id", relation = "author")]
    pub author_id: i64,
    #[jet(references = "user::Id", relation = "editor")]
    pub editor_id: Option<i64>,
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
        "CREATE TABLE users (id bigint PRIMARY KEY, name text NOT NULL)",
        "CREATE TABLE posts (
             id bigint PRIMARY KEY,
             title text NOT NULL,
             author_id bigint NOT NULL,
             editor_id bigint
         )",
        "INSERT INTO users (id, name) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        "INSERT INTO posts (id, title, author_id, editor_id) VALUES
             (10, 'intro', 1, 2),
             (11, 'part one', 1, NULL),
             (12, 'aside', 2, NULL)",
    ] {
        sqlx::query(statement)
            .execute(db.pool())
            .await
            .unwrap_or_else(|error| panic!("seed statement failed: {error}"));
    }
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn joins_pair_rows_with_their_relation_in_one_query() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    // The to-one author edge: every post pairs with exactly one user.
    let posts = PostEntity::find()
        .order_by(post::Id.asc())
        .also::<post::Author>()
        .all(&db)
        .await
        .expect("author join fetches");
    let pairs: Vec<(&str, Option<&str>)> = posts
        .iter()
        .map(|(post, author)| {
            (
                post.title.as_str(),
                author.as_ref().map(|user| user.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            ("intro", Some("alice")),
            ("part one", Some("alice")),
            ("aside", Some("bob")),
        ]
    );

    // A NULL foreign key null-extends instead of dropping the row.
    let edited = PostEntity::find()
        .order_by(post::Id.asc())
        .also::<post::Editor>()
        .all(&db)
        .await
        .expect("editor join fetches");
    let editors: Vec<Option<&str>> = edited
        .iter()
        .map(|(_, editor)| editor.as_ref().map(|user| user.name.as_str()))
        .collect();
    assert_eq!(editors, [Some("bob"), None, None]);

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn inverse_joins_expand_to_many_and_keep_unmatched_rows() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    // users ⟕ posts over the inverse edge: one row per authored post, and
    // carol — who authored nothing — still appears, paired with None.
    let rows = UserEntity::find()
        .order_by(user::Id.asc())
        .also::<Inverse<post::Author>>()
        .all(&db)
        .await
        .expect("inverse join fetches");
    let pairs: Vec<(&str, Option<&str>)> = rows
        .iter()
        .map(|(user, post)| {
            (
                user.name.as_str(),
                post.as_ref().map(|post| post.title.as_str()),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        [
            ("alice", Some("intro")),
            ("alice", Some("part one")),
            ("bob", Some("aside")),
            ("carol", None),
        ]
    );

    // Filtering still addresses the source entity over the joined rows.
    let only_bob = UserEntity::find()
        .filter(user::Name.eq("bob"))
        .also::<Inverse<post::Author>>()
        .all(&db)
        .await
        .expect("filtered join fetches");
    assert_eq!(only_bob.len(), 1);
    assert_eq!(
        only_bob[0].1.as_ref().map(|post| post.title.as_str()),
        Some("aside")
    );

    db.close().await;
}

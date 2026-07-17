//! Batch relation loading against live PostgreSQL servers.
//!
//! Run via `cargo xtask test-live` (needs a Docker daemon).

use jetorm_derive::JetModel;
use jetorm_entity::Inverse;
use jetorm_executor::{Database, SelectExecute, load_many, load_one};
use jetorm_query::{ColumnExt, EntityQuery};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};

const POSTGRES_TAG: &str = "18-alpine";

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users", crate_path = "::jetorm_entity")]
pub struct User {
    #[jet(primary_key)]
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "posts", crate_path = "::jetorm_entity")]
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
async fn load_many_groups_children_onto_their_parents() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    let users = UserEntity::find()
        .order_by(user::Id.asc())
        .all(&db)
        .await
        .expect("users load");

    // One query for all three parents, grouped back by author.
    let posts = load_many::<post::Author, _>(&users, &db)
        .await
        .expect("children load");
    let titles: Vec<Vec<&str>> = posts
        .iter()
        .map(|group| group.iter().map(|post| post.title.as_str()).collect())
        .collect();
    assert_eq!(
        titles,
        [
            vec!["intro", "part one"], // alice
            vec!["aside"],             // bob
            Vec::<&str>::new(),        // carol authored nothing
        ]
    );

    // The second edge to the same entity loads independently.
    let edited = load_many::<post::Editor, _>(&users, &db)
        .await
        .expect("edited load");
    assert_eq!(edited[0].len(), 0, "alice edited nothing");
    assert_eq!(edited[1].len(), 1, "bob edited the intro");
    assert_eq!(edited[1][0].title, "intro");

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn load_one_resolves_references_and_null_keys() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    let posts = PostEntity::find()
        .order_by(post::Id.asc())
        .all(&db)
        .await
        .expect("posts load");

    let authors = load_one::<post::Author, _>(&posts, &db)
        .await
        .expect("authors resolve");
    let names: Vec<Option<&str>> = authors
        .iter()
        .map(|author| author.as_ref().map(|user| user.name.as_str()))
        .collect();
    assert_eq!(names, [Some("alice"), Some("alice"), Some("bob")]);

    // A NULL foreign key resolves to None instead of failing the batch.
    let editors = load_one::<post::Editor, _>(&posts, &db)
        .await
        .expect("editors resolve");
    let names: Vec<Option<&str>> = editors
        .iter()
        .map(|editor| editor.as_ref().map(|user| user.name.as_str()))
        .collect();
    assert_eq!(names, [Some("bob"), None, None]);

    db.close().await;
}

#[tokio::test]
#[ignore = "requires a running Docker daemon"]
async fn inverse_edges_traverse_backwards_without_their_own_derive() {
    let (_container, db) = fresh_database().await;
    seed(&db).await;

    // Inverse<post::Author> walks users -> posts: load_one answers "some
    // post referencing each user" — a proof that the generic inverse
    // composes with the loaders without any codegen of its own.
    let users = UserEntity::find()
        .order_by(user::Id.asc())
        .all(&db)
        .await
        .expect("users load");
    let some_post = load_one::<Inverse<post::Author>, _>(&users, &db)
        .await
        .expect("inverse resolve");
    assert!(some_post[0].is_some(), "alice has posts");
    assert!(some_post[1].is_some(), "bob has a post");
    assert!(
        some_post[2].is_none(),
        "carol authored nothing, so the inverse edge finds nothing"
    );

    db.close().await;
}

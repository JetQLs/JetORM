//! Relation joins: lowering, rendering, and plan-cache identity.

use jetorm::prelude::*;
use jetorm::{Dialect, IntoAfterBurnerIr, Postgres};

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

#[test]
fn also_renders_one_left_join_over_the_relation_pair() {
    let query = PostEntity::find()
        .filter(post::Title.like("intro%"))
        .order_by(post::Id.asc())
        .also::<post::Author>();
    let module = query.clone().into_afterburner_ir().expect("join lowers");
    let statement = Postgres.render_query(&module).expect("join renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t4\".\"id\", \"t4\".\"title\", \"t4\".\"author_id\", \"t4\".\"editor_id\", \
         \"t4\".\"author__id\", \"t4\".\"author__name\" FROM \
         (SELECT \"t1\".\"id\" AS \"id\", \"t1\".\"title\" AS \"title\", \
         \"t1\".\"author_id\" AS \"author_id\", \"t1\".\"editor_id\" AS \"editor_id\", \
         \"t3\".\"id\" AS \"author__id\", \"t3\".\"name\" AS \"author__name\" FROM \
         (SELECT \"t0\".\"id\", \"t0\".\"title\", \"t0\".\"author_id\", \"t0\".\"editor_id\" \
         FROM \"posts\" AS \"t0\") AS \"t1\" LEFT JOIN \
         (SELECT \"t2\".\"id\", \"t2\".\"name\" FROM \"users\" AS \"t2\") AS \"t3\" \
         ON (\"t1\".\"author_id\" = \"t3\".\"id\")) AS \"t4\" \
         WHERE (\"t4\".\"title\" LIKE $1::text) \
         ORDER BY \"t4\".\"id\" ASC"
    );
    assert_eq!(query.binds().len(), 1);
}

#[test]
fn nullable_foreign_keys_join_through_their_own_edge() {
    // The editor edge joins on a nullable column; the condition widens the
    // non-null side instead of failing to lower.
    let module = PostEntity::find()
        .also::<post::Editor>()
        .into_afterburner_ir()
        .expect("nullable-key join lowers");
    let statement = Postgres.render_query(&module).expect("join renders");
    assert!(statement.sql().contains("LEFT JOIN"));
    assert!(statement.sql().contains("\"editor__name\""));
}

#[test]
fn joins_never_share_shapes_across_edges_or_with_their_select() {
    let select_shape = PostEntity::find().shape();
    let author = PostEntity::find().also::<post::Author>().shape();
    let editor = PostEntity::find().also::<post::Editor>().shape();
    assert_ne!(
        select_shape, author,
        "a cached bare select must never answer a join"
    );
    assert_ne!(
        author, editor,
        "two edges to one entity are different statements"
    );

    let again = PostEntity::find().also::<post::Author>().shape();
    assert_eq!(author, again, "the same edge shares one statement");
}

#[test]
fn related_predicates_address_the_joined_entitys_columns() {
    let query = PostEntity::find()
        .filter(post::Title.like("intro%"))
        .also::<post::Author>()
        .filter_related(user::Name.eq("alice"))
        .order_by_related(user::Name.asc());
    assert_eq!(
        query.binds(),
        [
            Value::Text("intro%".to_owned()),
            Value::Text("alice".to_owned()),
        ],
        "source and related binds share one positional table, in call order"
    );
    let module = query
        .clone()
        .into_afterburner_ir()
        .expect("related-filtered join lowers");
    let statement = Postgres.render_query(&module).expect("join renders");
    let sql = statement.sql();
    assert!(
        sql.contains("\"author__name\" = $2::text"),
        "the related predicate addresses the joined side: {sql}"
    );
    assert!(
        sql.contains("ORDER BY \"t4\".\"author__name\" ASC"),
        "the related key orders by the joined column: {sql}"
    );

    // The related predicate is part of the statement's identity.
    let unfiltered = PostEntity::find()
        .filter(post::Title.like("intro%"))
        .also::<post::Author>();
    assert_ne!(query.shape(), unfiltered.shape());
}

#[test]
fn distinct_over_a_join_is_rejected_at_lowering() {
    let error = PostEntity::find()
        .distinct()
        .also::<post::Author>()
        .into_afterburner_ir()
        .expect_err("two defensible meanings, so the combination must not guess");
    assert_eq!(error, jetorm::LoweringError::DistinctOverJoin);
}

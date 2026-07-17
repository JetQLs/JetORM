//! Relation markers from the derive and their metadata.

use jetorm::prelude::*;
use jetorm::{ForeignKeyMeta, ReferentialAction};

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "users")]
pub struct User {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "posts")]
pub struct Post {
    #[jet(primary_key, auto_increment)]
    pub id: i64,
    pub title: String,
    #[jet(references = "user::Id", relation = "author", on_delete = "cascade")]
    pub author_id: i64,
    /// A second edge to the same entity: named relations keep them apart.
    #[jet(references = "user::Id", relation = "editor")]
    pub editor_id: Option<i64>,
}

#[test]
fn relation_markers_carry_typed_edges() {
    fn edge<R: Relation>() -> (&'static str, &'static str, &'static str, bool) {
        (
            R::NAME,
            <R::Source as Entity>::TABLE.name(),
            <R::Target as Entity>::TABLE.name(),
            R::TO_ONE,
        )
    }
    assert_eq!(edge::<post::Author>(), ("author", "posts", "users", true));
    assert_eq!(edge::<post::Editor>(), ("editor", "posts", "users", true));

    // The joined columns are type-level facts.
    assert_eq!(
        <post::Author as Relation>::SourceColumn::meta().name(),
        "author_id"
    );
    assert_eq!(
        <post::Author as Relation>::TargetColumn::meta().name(),
        "id"
    );
}

#[test]
fn owning_edges_carry_their_constraint_and_inverses_do_not() {
    assert_eq!(
        <post::Author as Relation>::FOREIGN_KEY,
        Some(ForeignKeyMeta::new(
            ReferentialAction::Cascade,
            ReferentialAction::NoAction,
        ))
    );
    assert_eq!(
        <post::Editor as Relation>::FOREIGN_KEY,
        Some(ForeignKeyMeta::new(
            ReferentialAction::NoAction,
            ReferentialAction::NoAction,
        ))
    );

    type Posts = Inverse<post::Author>;
    assert_eq!(<Posts as Relation>::FOREIGN_KEY, None);
    const {
        assert!(!<Posts as Relation>::TO_ONE);
    }
    assert_eq!(<Posts as Relation>::SourceColumn::meta().name(), "id");
    assert_eq!(
        <Posts as Relation>::TargetColumn::meta().name(),
        "author_id"
    );
}

#[test]
fn entities_enumerate_their_foreign_keys_as_values() {
    // The value-level mirror of the relation markers: schema tooling walks
    // this without naming marker types.
    let keys = <PostEntity as Entity>::FOREIGN_KEYS;
    assert_eq!(keys.len(), 2);

    assert_eq!(keys[0].column(), 2);
    assert_eq!(keys[0].target_table().name(), "users");
    assert_eq!(keys[0].target_column(), "id");
    assert_eq!(keys[0].actions().on_delete(), ReferentialAction::Cascade);
    assert_eq!(keys[0].actions().on_update(), ReferentialAction::NoAction);

    assert_eq!(keys[1].column(), 3);
    assert_eq!(keys[1].actions().on_delete(), ReferentialAction::NoAction);

    assert!(<UserEntity as Entity>::FOREIGN_KEYS.is_empty());
}

#[test]
fn models_expose_column_values_by_position() {
    let post = Post {
        id: 5,
        title: "hello".to_owned(),
        author_id: 9,
        editor_id: None,
    };
    assert_eq!(post.value(0), Some(Value::Int64(5)));
    assert_eq!(post.value(1), Some(Value::Text("hello".to_owned())));
    assert_eq!(post.value(2), Some(Value::Int64(9)));
    assert_eq!(
        post.value(3),
        Some(Value::Null(jetorm::ColumnType::Int64)),
        "an unset nullable foreign key reads as a typed NULL"
    );
    assert_eq!(post.value(4), None);
}

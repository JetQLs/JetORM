//! Name handling in `#[derive(JetModel)]`.
//!
//! Rust spelling and SQL naming are different namespaces: a field written
//! `r#type` is the column `type`, and its generated marker must be a valid
//! Rust type name.

use jetorm::prelude::*;
use jetorm::{ColumnType, Dialect, Postgres, afterburner};

/// Every field here is a Rust keyword, so each is written as a raw
/// identifier. The generated markers (`Type`, `Match`, `Ref`, `Box`) and the
/// module name must all be valid, and the SQL names must drop the `r#`.
#[derive(Clone, Debug, JetModel)]
#[jet(table = "keyword_rows")]
pub struct KeywordRow {
    #[jet(primary_key)]
    pub id: i64,
    pub r#type: String,
    pub r#match: Option<String>,
    pub r#ref: i32,
    pub r#box: bool,
}

/// A struct whose snake_case module name is itself a keyword (`type`), which
/// the derive must escape into a raw identifier.
#[derive(Clone, Debug, JetModel)]
#[jet(table = "types")]
pub struct Type {
    #[jet(primary_key)]
    pub id: i64,
    pub label: String,
}

#[test]
fn raw_identifier_fields_map_to_unprefixed_column_names() {
    let names: Vec<&str> = KeywordRowEntity::COLUMNS
        .iter()
        .map(jetorm::ColumnMeta::name)
        .collect();
    assert_eq!(names, ["id", "type", "match", "ref", "box"]);

    let rust_names: Vec<&str> = KeywordRowEntity::COLUMNS
        .iter()
        .map(jetorm::ColumnMeta::rust_name)
        .collect();
    assert_eq!(rust_names, ["id", "type", "match", "ref", "box"]);
}

#[test]
fn raw_identifier_fields_produce_usable_column_markers() {
    // Naming the markers at all is the test: `r#type` used to produce the
    // invalid identifier `R#type` and panic the proc macro.
    assert_eq!(keyword_row::Type::meta().name(), "type");
    assert_eq!(keyword_row::Match::meta().name(), "match");
    assert_eq!(keyword_row::Ref::meta().column_type(), ColumnType::Int32);
    assert_eq!(keyword_row::Box::meta().column_type(), ColumnType::Boolean);
    assert!(keyword_row::Match::meta().is_nullable());
}

#[test]
fn raw_identifier_columns_render_as_quoted_sql() {
    let query = KeywordRowEntity::find()
        .filter(keyword_row::Type.eq("t"))
        .order_by(keyword_row::Ref.desc());
    let module = afterburner!(query).expect("query lowers to verified IR");
    let statement = Postgres.render_query(&module).expect("query renders");
    assert_eq!(
        statement.sql(),
        "SELECT \"t0\".\"id\", \"t0\".\"type\", \"t0\".\"match\", \"t0\".\"ref\", \"t0\".\"box\" \
         FROM \"keyword_rows\" AS \"t0\" \
         WHERE (\"t0\".\"type\" = $1::text) \
         ORDER BY \"t0\".\"ref\" DESC"
    );
}

#[test]
fn keyword_module_names_are_escaped() {
    // The module for `struct Type` is `r#type`; reaching a marker through it
    // proves the escape.
    assert_eq!(r#type::Label::meta().name(), "label");
    assert_eq!(TypeEntity::TABLE.name(), "types");
}

#[test]
fn column_markers_expose_metadata_consistent_with_their_type_parameters() {
    // `Column::NULLABLE` and the column metadata are written by the same
    // derive; they must never disagree.
    fn assert_consistent<C: Column>() {
        assert_eq!(
            C::NULLABLE,
            C::meta().is_nullable(),
            "marker nullability disagrees with metadata for column {:?}",
            C::meta().name()
        );
    }
    assert_consistent::<keyword_row::Id>();
    assert_consistent::<keyword_row::Type>();
    assert_consistent::<keyword_row::Match>();
    assert_consistent::<keyword_row::Ref>();
    assert_consistent::<keyword_row::Box>();
}

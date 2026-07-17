//! Pagination builders: statement sharing and page arithmetic.

use jetorm::prelude::*;

#[derive(Clone, Debug, PartialEq, JetModel)]
#[jet(table = "items")]
pub struct Item {
    #[jet(primary_key)]
    pub id: i64,
    pub label: String,
}

#[test]
fn every_cursor_page_of_one_walk_shares_a_statement() {
    // Page one and page fifty differ only in the bound resume key, so the
    // parameters-not-literals invariant makes them one prepared statement.
    let page_one = ItemEntity::find().cursor_by((item::Id,)).after(0).first(20);
    let page_fifty = ItemEntity::find()
        .cursor_by((item::Id,))
        .after(980)
        .first(20);
    assert_eq!(page_one.select().shape(), page_fifty.select().shape());

    // Forward and backward pages order differently: different statements.
    let backward = ItemEntity::find().cursor_by((item::Id,)).after(0).last(20);
    assert_ne!(page_one.select().shape(), backward.select().shape());
}

#[test]
fn cursor_by_replaces_earlier_ordering() {
    // The cursor's correctness depends on its own key order; an earlier
    // order_by must not survive underneath it.
    let page = ItemEntity::find()
        .order_by(item::Label.desc())
        .cursor_by((item::Id,))
        .first(5);
    let plain = ItemEntity::find().cursor_by((item::Id,)).first(5);
    assert_eq!(page.select().shape(), plain.select().shape());
}

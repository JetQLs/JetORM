use jetorm_entity::{Column, Entity};

use crate::expr::{ColumnExt, Expr, OrderKey};
use crate::select::Select;

/// A tuple of column markers a cursor pages over, one to four columns.
///
/// A single column is the 1-tuple `(item::Id,)`, whose key is its bare
/// value — the same spelling projections use. Multi-column keys page in
/// lexicographic order — `(a, b) > (x, y)` — spelled through plain
/// comparisons (`a > x OR (a = x AND b > y)`), so the statement's shape
/// stays independent of the key's values, like every other query.
pub trait CursorKey<E>: Copy
where
    E: Entity,
{
    /// Rust value of one full cursor position; a single column's is bare.
    type Key;

    /// Ordering keys, leading column first.
    fn order_keys(self, descending: bool) -> Vec<OrderKey<E>>;

    /// Rows strictly after the position in ascending key order.
    fn after_expr(self, key: Self::Key) -> Expr<E, bool>;

    /// Rows strictly before the position in ascending key order.
    fn before_expr(self, key: Self::Key) -> Expr<E, bool>;
}

/// Folds per-column boundary comparisons into the lexicographic form:
/// `(a, b, c) > key` is `a > x OR (a = x AND (b > y OR (b = y AND c > z)))`.
fn lexicographic<E>(mut strict: Vec<Expr<E, bool>>, mut equal: Vec<Expr<E, bool>>) -> Expr<E, bool>
where
    E: Entity,
{
    let mut expression = strict.pop().expect("cursor keys are non-empty");
    equal.pop();
    while let (Some(bound), Some(tie)) = (strict.pop(), equal.pop()) {
        expression = bound.or(tie.and(expression));
    }
    expression
}

/// A single projected column keys by its bare value, not a 1-tuple.
impl<E, A> CursorKey<E> for (A,)
where
    E: Entity,
    A: Column<Entity = E> + Copy,
    A::Rust: Clone,
{
    type Key = A::Rust;

    fn order_keys(self, descending: bool) -> Vec<OrderKey<E>> {
        vec![if descending {
            self.0.desc()
        } else {
            self.0.asc()
        }]
    }

    fn after_expr(self, key: Self::Key) -> Expr<E, bool> {
        self.0.gt(key)
    }

    fn before_expr(self, key: Self::Key) -> Expr<E, bool> {
        self.0.lt(key)
    }
}

macro_rules! impl_cursor_key_for_tuple {
    ($($column:ident $key:ident $index:tt),+) => {
        impl<E, $($column),+> CursorKey<E> for ($($column,)+)
        where
            E: Entity,
            $($column: Column<Entity = E> + Copy, $column::Rust: Clone,)+
        {
            type Key = ($($column::Rust,)+);

            fn order_keys(self, descending: bool) -> Vec<OrderKey<E>> {
                if descending {
                    vec![$(self.$index.desc()),+]
                } else {
                    vec![$(self.$index.asc()),+]
                }
            }

            fn after_expr(self, key: Self::Key) -> Expr<E, bool> {
                let ($($key,)+) = key;
                lexicographic(
                    vec![$(self.$index.gt($key.clone())),+],
                    vec![$(self.$index.eq($key)),+],
                )
            }

            fn before_expr(self, key: Self::Key) -> Expr<E, bool> {
                let ($($key,)+) = key;
                lexicographic(
                    vec![$(self.$index.lt($key.clone())),+],
                    vec![$(self.$index.eq($key)),+],
                )
            }
        }
    };
}

impl_cursor_key_for_tuple!(A a 0, B b 1);
impl_cursor_key_for_tuple!(A a 0, B b 1, C c 2);
impl_cursor_key_for_tuple!(A a 0, B b 1, C c 2, D d 3);

/// Keyset pagination over an ordered key.
///
/// Created by [`Select::cursor_by`]. Where offset pagination re-scans and
/// discards every skipped row, a cursor page filters on the last seen key —
/// `WHERE key > $1 ORDER BY key LIMIT $2` — so page one thousand costs the
/// same as page one. The trade-off is positional access: a cursor walks
/// forward from a key, it cannot jump to page `n`.
///
/// The key as a whole should be unique — a primary key, a unique column,
/// or a composite ending in one — since paging on a non-unique key can
/// skip rows that share the boundary value. Composite keys make the
/// tie-breaker part of the key: `cursor_by((doc::CreatedAt, doc::Id))`.
#[derive(Clone, Debug)]
pub struct Cursor<E, K>
where
    E: Entity,
    K: CursorKey<E>,
{
    select: Select<E>,
    key: K,
}

impl<E> Select<E>
where
    E: Entity,
{
    /// Starts keyset pagination ordered by the given key — a tuple of
    /// column markers, a single column being `(item::Id,)`, paging in
    /// lexicographic order.
    ///
    /// Filters already on the select carry over; ordering set earlier is
    /// replaced, since the cursor's correctness depends on its own key
    /// order.
    #[must_use]
    pub fn cursor_by<K>(mut self, key: K) -> Cursor<E, K>
    where
        K: CursorKey<E>,
    {
        self.order.clear();
        Cursor { select: self, key }
    }
}

impl<E, K> Cursor<E, K>
where
    E: Entity,
    K: CursorKey<E>,
{
    /// Restricts the page to rows after the key, exclusive.
    ///
    /// This is the resume point: pass the last row's key from the previous
    /// page.
    #[must_use]
    pub fn after(mut self, key: impl Into<K::Key>) -> Self {
        self.select = self.select.filter(self.key.after_expr(key.into()));
        self
    }

    /// Restricts the page to rows before the key, exclusive.
    #[must_use]
    pub fn before(mut self, key: impl Into<K::Key>) -> Self {
        self.select = self.select.filter(self.key.before_expr(key.into()));
        self
    }

    /// Takes the first `count` rows in ascending key order.
    #[must_use]
    pub fn first(self, count: u64) -> CursorPage<E> {
        let mut select = self.select;
        for order in self.key.order_keys(false) {
            select = select.order_by(order);
        }
        CursorPage {
            select: select.limit(count),
            reversed: false,
        }
    }

    /// Takes the last `count` rows, returned in ascending key order.
    ///
    /// The query fetches in descending order to stop at `count` rows;
    /// execution reverses the page so both directions read the same way.
    #[must_use]
    pub fn last(self, count: u64) -> CursorPage<E> {
        let mut select = self.select;
        for order in self.key.order_keys(true) {
            select = select.order_by(order);
        }
        CursorPage {
            select: select.limit(count),
            reversed: true,
        }
    }
}

/// One bounded cursor page, ready to execute.
///
/// The underlying select is an ordinary query — cursor pages of one shape
/// share one cached statement like any other select.
#[derive(Clone, Debug)]
pub struct CursorPage<E>
where
    E: Entity,
{
    select: Select<E>,
    reversed: bool,
}

impl<E> CursorPage<E>
where
    E: Entity,
{
    /// Returns the page's underlying select.
    #[must_use]
    pub fn select(&self) -> &Select<E> {
        &self.select
    }

    /// Consumes the page into its select and whether fetched rows must be
    /// reversed to restore ascending key order.
    #[must_use]
    pub fn into_parts(self) -> (Select<E>, bool) {
        (self.select, self.reversed)
    }
}

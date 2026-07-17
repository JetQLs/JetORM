use jetorm_entity::{Column, Entity};

use crate::expr::ColumnExt;
use crate::select::Select;

/// Keyset pagination over one ordered column.
///
/// Created by [`Select::cursor_by`]. Where offset pagination re-scans and
/// discards every skipped row, a cursor page filters on the last seen key —
/// `WHERE key > $1 ORDER BY key LIMIT $2` — so page one thousand costs the
/// same as page one. The trade-off is positional access: a cursor walks
/// forward from a key, it cannot jump to page `n`.
///
/// The cursor column should be unique (a primary key or unique column);
/// paging on a non-unique key can skip rows that share the boundary value.
#[derive(Clone, Debug)]
pub struct Cursor<E, C>
where
    E: Entity,
    C: Column<Entity = E> + Default,
{
    select: Select<E>,
    column: C,
}

impl<E> Select<E>
where
    E: Entity,
{
    /// Starts keyset pagination ordered by the given column.
    ///
    /// Filters already on the select carry over; ordering set earlier is
    /// replaced, since the cursor's correctness depends on its own key
    /// order.
    #[must_use]
    pub fn cursor_by<C>(mut self, column: C) -> Cursor<E, C>
    where
        C: Column<Entity = E> + Default,
    {
        self.order.clear();
        Cursor {
            select: self,
            column,
        }
    }
}

impl<E, C> Cursor<E, C>
where
    E: Entity,
    C: Column<Entity = E> + Default + Copy,
{
    /// Restricts the page to rows after the key, exclusive.
    ///
    /// This is the resume point: pass the last row's key from the previous
    /// page.
    #[must_use]
    pub fn after(mut self, value: impl Into<C::Rust>) -> Self {
        self.select = self.select.filter(self.column.gt(value));
        self
    }

    /// Restricts the page to rows before the key, exclusive.
    #[must_use]
    pub fn before(mut self, value: impl Into<C::Rust>) -> Self {
        self.select = self.select.filter(self.column.lt(value));
        self
    }

    /// Takes the first `count` rows in ascending key order.
    #[must_use]
    pub fn first(self, count: u64) -> CursorPage<E> {
        CursorPage {
            select: self.select.order_by(self.column.asc()).limit(count),
            reversed: false,
        }
    }

    /// Takes the last `count` rows, returned in ascending key order.
    ///
    /// The query fetches in descending order to stop at `count` rows;
    /// execution reverses the page so both directions read the same way.
    #[must_use]
    pub fn last(self, count: u64) -> CursorPage<E> {
        CursorPage {
            select: self.select.order_by(self.column.desc()).limit(count),
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

use std::future::Future;

use jetorm_entity::Entity;
use jetorm_query::{CursorPage, Select};

use crate::database::Executor;
use crate::error::ExecuteError;
use crate::select::SelectExecute;

/// Offset pagination with totals over one select.
///
/// Created by [`PaginateExecute::paginate`]. Every page runs the same
/// cached statement with different offset and limit binds, and the total
/// comes from the select's own `count(*)`. For deep pagination prefer
/// [`jetorm_query::Cursor`]: an offset makes the database scan and discard
/// every skipped row, a cursor does not.
#[derive(Clone, Debug)]
pub struct Paginator<E, X>
where
    E: Entity,
{
    select: Select<E>,
    executor: X,
    page_size: u64,
}

/// Pagination entry point for typed select queries.
pub trait PaginateExecute<E>: Sized
where
    E: Entity,
{
    /// Splits the query into pages of `page_size` rows.
    ///
    /// The executor is captured for the paginator's lifetime, so this
    /// takes a connection handle (`&Database`); inside a transaction, page
    /// manually with [`Select::limit`] and [`Select::offset`].
    ///
    /// # Panics
    ///
    /// Panics when `page_size` is zero — no page could ever make progress.
    fn paginate<X>(self, executor: X, page_size: u64) -> Paginator<E, X>
    where
        X: Executor + Copy;
}

impl<E> PaginateExecute<E> for Select<E>
where
    E: Entity,
{
    fn paginate<X>(self, executor: X, page_size: u64) -> Paginator<E, X>
    where
        X: Executor + Copy,
    {
        assert!(page_size > 0, "page size must be at least one row");
        Paginator {
            select: self,
            executor,
            page_size,
        }
    }
}

impl<E, X> Paginator<E, X>
where
    E: Entity,
    X: Executor + Copy,
{
    /// Fetches one zero-based page.
    ///
    /// Pages past the end are empty, not an error.
    ///
    /// # Errors
    ///
    /// Returns an error when the underlying query fails.
    pub async fn fetch_page(&self, page: u64) -> Result<Vec<E::Model>, ExecuteError> {
        self.select
            .clone()
            .offset(page.saturating_mul(self.page_size))
            .limit(self.page_size)
            .all(self.executor)
            .await
    }

    /// Counts all rows the query matches, across every page.
    ///
    /// # Errors
    ///
    /// Returns an error when the count query fails.
    pub async fn num_items(&self) -> Result<u64, ExecuteError> {
        self.select.clone().count(self.executor).await
    }

    /// Counts the pages needed for every matching row.
    ///
    /// # Errors
    ///
    /// Returns an error when the count query fails.
    pub async fn num_pages(&self) -> Result<u64, ExecuteError> {
        let items = self.num_items().await?;
        Ok(items.div_ceil(self.page_size))
    }

    /// Returns the configured rows per page.
    #[must_use]
    pub const fn page_size(&self) -> u64 {
        self.page_size
    }
}

/// Execution entry points for cursor pages.
pub trait CursorExecute<E>: Sized
where
    E: Entity,
{
    /// Fetches the page, always in ascending key order.
    fn all<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Vec<E::Model>, ExecuteError>> + Send
    where
        X: Executor;
}

impl<E> CursorExecute<E> for CursorPage<E>
where
    E: Entity,
{
    async fn all<X>(self, executor: X) -> Result<Vec<E::Model>, ExecuteError>
    where
        X: Executor,
    {
        let (select, reversed) = self.into_parts();
        let mut rows = select.all(executor).await?;
        // A `last` page fetched descending to stop at the limit; reversing
        // restores the ascending order both directions promise.
        if reversed {
            rows.reverse();
        }
        Ok(rows)
    }
}

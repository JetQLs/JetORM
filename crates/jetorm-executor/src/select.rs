use std::future::Future;

use jetorm_entity::{ColumnMeta, ColumnType, DecodeError, Entity, Model, SqlValue};
use jetorm_query::{ColumnList, Projected, Select};

use crate::database::Executor;
use crate::error::ExecuteError;

/// Output column types of an entity's full row.
fn full_row_types(columns: &[ColumnMeta]) -> Vec<ColumnType> {
    columns.iter().map(ColumnMeta::column_type).collect()
}

/// Execution entry points for typed select queries.
///
/// The methods consume the builder, run the full pipeline — plan-cache
/// lookup (lowering, verification, and SQL rendering on a miss), binding,
/// fetching, and decoding — and return entity models.
pub trait SelectExecute<E>: Sized
where
    E: Entity,
{
    /// Fetches every matching row as entity models.
    fn all<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Vec<E::Model>, ExecuteError>> + Send
    where
        X: Executor;

    /// Fetches at most one row, applying `LIMIT 1` to the query.
    fn one<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Option<E::Model>, ExecuteError>> + Send
    where
        X: Executor;

    /// Counts the rows this query would return, without fetching them.
    ///
    /// Executes as a single `count(*)` over the same filter, `DISTINCT`,
    /// and row limit; ordering is dropped, since it cannot change the
    /// count, so counts differing only in `order_by` share one cached
    /// statement.
    fn count<X>(self, executor: X) -> impl Future<Output = Result<u64, ExecuteError>> + Send
    where
        X: Executor;
}

impl<E> SelectExecute<E> for Select<E>
where
    E: Entity,
{
    async fn all<X>(self, executor: X) -> Result<Vec<E::Model>, ExecuteError>
    where
        X: Executor,
    {
        let statement = executor.plan_cache().statement(&self)?;
        let rows = executor
            .fetch_rows(statement, self.into_binds(), full_row_types(E::COLUMNS))
            .await?;

        let mut models = Vec::with_capacity(rows.len());
        for (index, row) in rows.into_iter().enumerate() {
            models.push(
                E::Model::from_values(row.into_values())
                    .map_err(|source| ExecuteError::Decode { row: index, source })?,
            );
        }
        Ok(models)
    }

    async fn one<X>(self, executor: X) -> Result<Option<E::Model>, ExecuteError>
    where
        X: Executor,
    {
        let mut models = self.limit(1).all(executor).await?;
        Ok(models.pop())
    }

    async fn count<X>(self, executor: X) -> Result<u64, ExecuteError>
    where
        X: Executor,
    {
        let query = self.into_count();
        let statement = executor.plan_cache().statement(&query)?;
        let rows = executor
            .fetch_rows(statement, query.into_binds(), vec![ColumnType::Int64])
            .await?;

        // A grand-total aggregate returns exactly one row with one value; a
        // driver delivering anything else is reported, not unwrapped.
        let Some(row) = rows.into_iter().next() else {
            return Err(ExecuteError::Decode {
                row: 0,
                source: DecodeError::ColumnCount {
                    expected: 1,
                    actual: 0,
                },
            });
        };
        let mut values = row.into_values();
        let value = values.pop().ok_or(ExecuteError::Decode {
            row: 0,
            source: DecodeError::ColumnCount {
                expected: 1,
                actual: 0,
            },
        })?;
        let count = i64::from_value(value).map_err(|mismatch| ExecuteError::Decode {
            row: 0,
            source: DecodeError::Column {
                name: "count",
                mismatch,
            },
        })?;
        // SQL's count is never negative; the fallback is unreachable.
        Ok(u64::try_from(count).unwrap_or_default())
    }
}

/// Execution entry points for projected queries.
///
/// Rows decode into the column list's Rust type — one column yields its
/// field type, a tuple of columns yields a tuple — through the same
/// pipeline as full-model queries.
pub trait ProjectedExecute<E, C>: Sized
where
    E: Entity,
    C: ColumnList<E>,
{
    /// Fetches every matching row as projected values.
    fn all<X>(self, executor: X) -> impl Future<Output = Result<Vec<C::Row>, ExecuteError>> + Send
    where
        X: Executor;

    /// Fetches at most one row, applying `LIMIT 1` to the query.
    fn one<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Option<C::Row>, ExecuteError>> + Send
    where
        X: Executor;
}

impl<E, C> ProjectedExecute<E, C> for Projected<E, C>
where
    E: Entity,
    C: ColumnList<E> + Send,
{
    async fn all<X>(self, executor: X) -> Result<Vec<C::Row>, ExecuteError>
    where
        X: Executor,
    {
        let select = self.into_select();
        let statement = executor.plan_cache().statement(&select)?;
        let column_types: Vec<ColumnType> = select
            .projection()
            .expect("a projected query always carries its projection")
            .iter()
            .map(|index| E::COLUMNS[*index].column_type())
            .collect();
        let rows = executor
            .fetch_rows(statement, select.into_binds(), column_types)
            .await?;

        let mut decoded = Vec::with_capacity(rows.len());
        for (index, row) in rows.into_iter().enumerate() {
            decoded.push(
                C::decode(row.into_values())
                    .map_err(|source| ExecuteError::Decode { row: index, source })?,
            );
        }
        Ok(decoded)
    }

    async fn one<X>(self, executor: X) -> Result<Option<C::Row>, ExecuteError>
    where
        X: Executor,
    {
        let mut rows = self.limit(1).all(executor).await?;
        Ok(rows.pop())
    }
}

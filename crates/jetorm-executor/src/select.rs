use std::future::Future;

use jetorm_entity::{ColumnMeta, ColumnType, Entity, Model};
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

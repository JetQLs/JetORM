//! Execution entry points for row-count and returning mutations.

use std::future::Future;

use jetorm_entity::{ColumnMeta, ColumnType, Entity, Model};
use jetorm_query::{Delete, Insert, Returning, Update};

use crate::database::Executor;
use crate::error::ExecuteError;

fn full_row_types(columns: &[ColumnMeta]) -> Vec<ColumnType> {
    columns.iter().map(ColumnMeta::column_type).collect()
}

/// Executes a mutation that returns only its affected-row count.
pub trait MutationExecute: Sized {
    /// Runs the mutation and returns the database-reported affected rows.
    fn execute<X>(self, executor: X) -> impl Future<Output = Result<u64, ExecuteError>> + Send
    where
        X: Executor;
}

macro_rules! impl_mutation_execute {
    ($mutation:ident) => {
        impl<E: Entity> MutationExecute for $mutation<E> {
            async fn execute<X>(self, executor: X) -> Result<u64, ExecuteError>
            where
                X: Executor,
            {
                let binds = self.binds().to_vec();
                let statement = executor.plan_cache().statement(&self)?;
                executor.execute_statement(statement, binds).await
            }
        }
    };
}

impl_mutation_execute!(Insert);
impl_mutation_execute!(Update);
impl_mutation_execute!(Delete);

/// Fetches entity rows from a mutation carrying `RETURNING`.
pub trait ReturningExecute<E>: Sized
where
    E: Entity,
{
    /// Executes the mutation and decodes every returned entity row.
    fn all<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Vec<E::Model>, ExecuteError>> + Send
    where
        X: Executor;

    /// Executes the complete mutation and returns its first decoded row, if any.
    ///
    /// This method does not limit the number of rows affected by the mutation.
    fn one<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Option<E::Model>, ExecuteError>> + Send
    where
        X: Executor;
}

macro_rules! impl_returning_execute {
    ($mutation:ident) => {
        impl<E: Entity> ReturningExecute<E> for Returning<$mutation<E>> {
            async fn all<X>(self, executor: X) -> Result<Vec<E::Model>, ExecuteError>
            where
                X: Executor,
            {
                let binds = self.binds().to_vec();
                let statement = executor.plan_cache().statement(&self)?;
                let rows = executor
                    .fetch_rows(statement, binds, full_row_types(E::COLUMNS))
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
                Ok(self.all(executor).await?.into_iter().next())
            }
        }
    };
}

impl_returning_execute!(Insert);
impl_returning_execute!(Update);
impl_returning_execute!(Delete);

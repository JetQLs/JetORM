use std::future::Future;

use afterburner::afterburner;
use jetorm_entity::{Entity, Model};
use jetorm_query::Select;

use crate::database::Executor;
use crate::error::ExecuteError;
use crate::value::decode_row;

/// Execution entry points for typed select queries.
///
/// The methods consume the builder, run the full pipeline — lowering,
/// verification, plan-cache lookup, binding, fetching, and decoding — and
/// return entity models.
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
        let binds = self.binds().to_vec();
        let module = afterburner!(self).map_err(ExecuteError::Build)?;
        let statement = executor.plan_cache().statement(&module)?;
        drop(module);

        let rows = executor.fetch_rows(statement, binds).await?;
        let mut models = Vec::with_capacity(rows.len());
        for (index, row) in rows.iter().enumerate() {
            let values = decode_row(row, E::COLUMNS)?;
            models.push(
                E::Model::from_values(values)
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

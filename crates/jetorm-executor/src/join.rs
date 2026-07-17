use std::future::Future;

use jetorm_entity::{Column, ColumnMeta, ColumnType, Entity, Model, Relation};
use jetorm_query::JoinSelect;

use crate::database::Executor;
use crate::error::ExecuteError;

/// Rows on the referencing side of a relation.
type SourceModel<R> = <<R as Relation>::Source as Entity>::Model;
/// Rows on the referenced side of a relation.
type TargetModel<R> = <<R as Relation>::Target as Entity>::Model;

/// Execution entry points for relation-join queries.
///
/// Each fetched row splits into the source model and the joined target
/// model — `None` when the `LEFT JOIN` matched nothing — through the same
/// pipeline as every other query: plan cache, binding, positional decode.
pub trait JoinExecute<R>: Sized
where
    R: Relation,
{
    /// Fetches every matching row as source–target pairs.
    #[allow(clippy::type_complexity)]
    fn all<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Vec<(SourceModel<R>, Option<TargetModel<R>>)>, ExecuteError>> + Send
    where
        X: Executor;

    /// Fetches at most one pair, applying `LIMIT 1` to the query.
    #[allow(clippy::type_complexity)]
    fn one<X>(
        self,
        executor: X,
    ) -> impl Future<Output = Result<Option<(SourceModel<R>, Option<TargetModel<R>>)>, ExecuteError>>
    + Send
    where
        X: Executor;
}

impl<R> JoinExecute<R> for JoinSelect<R>
where
    R: Relation,
{
    async fn all<X>(
        self,
        executor: X,
    ) -> Result<Vec<(SourceModel<R>, Option<TargetModel<R>>)>, ExecuteError>
    where
        X: Executor,
    {
        let statement = executor.plan_cache().statement(&self)?;
        let mut column_types: Vec<ColumnType> = R::Source::COLUMNS
            .iter()
            .map(ColumnMeta::column_type)
            .collect();
        column_types.extend(R::Target::COLUMNS.iter().map(ColumnMeta::column_type));
        let source_width = R::Source::COLUMNS.len();

        let rows = executor
            .fetch_rows(statement, self.into_binds(), column_types)
            .await?;

        let mut pairs = Vec::with_capacity(rows.len());
        for (index, row) in rows.into_iter().enumerate() {
            let mut values = row.into_values();
            let target_values = values.split_off(source_width);
            let source = SourceModel::<R>::from_values(values)
                .map_err(|source| ExecuteError::Decode { row: index, source })?;
            // The join key discriminates a match from null extension: the
            // condition only matches on non-NULL keys, so a NULL joined key
            // means the left row matched nothing.
            let target = if target_values[<R::TargetColumn as Column>::INDEX].is_null() {
                None
            } else {
                Some(
                    TargetModel::<R>::from_values(target_values)
                        .map_err(|source| ExecuteError::Decode { row: index, source })?,
                )
            };
            pairs.push((source, target));
        }
        Ok(pairs)
    }

    async fn one<X>(
        self,
        executor: X,
    ) -> Result<Option<(SourceModel<R>, Option<TargetModel<R>>)>, ExecuteError>
    where
        X: Executor,
    {
        let mut pairs = self.limit(1).all(executor).await?;
        Ok(pairs.pop())
    }
}

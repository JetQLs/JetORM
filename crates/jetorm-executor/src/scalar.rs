//! Execution and decoding for existence queries.

use std::future::Future;

use jetorm_entity::{ColumnType, DecodeError, Entity, SqlValue};
use jetorm_query::Exists;

use crate::database::Executor;
use crate::error::ExecuteError;

/// Executes a typed existence query.
pub trait ExistsExecute: Sized {
    /// Returns whether the query can produce at least one row.
    fn get<X>(self, executor: X) -> impl Future<Output = Result<bool, ExecuteError>> + Send
    where
        X: Executor;
}

impl<E: Entity> ExistsExecute for Exists<E> {
    async fn get<X>(self, executor: X) -> Result<bool, ExecuteError>
    where
        X: Executor,
    {
        let statement = executor.plan_cache().statement(&self)?;
        let rows = executor
            .fetch_rows(statement, self.into_binds(), vec![ColumnType::Boolean])
            .await?;
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
        bool::from_value(value).map_err(|mismatch| ExecuteError::Decode {
            row: 0,
            source: DecodeError::Column {
                name: "exists",
                mismatch,
            },
        })
    }
}

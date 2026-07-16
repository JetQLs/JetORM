use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use jetorm_dialect::Statement;
use jetorm_entity::{ColumnMeta, ColumnType, Value};
use sqlx::Row;
use sqlx::postgres::{PgArguments, PgRow};
use uuid::Uuid;

use crate::error::ExecuteError;

pub(crate) type PgQuery<'query> = sqlx::query::Query<'query, sqlx::Postgres, PgArguments>;

/// Builds the driver query for one statement, binding values in `$n` order.
pub(crate) fn build_query<'statement>(
    statement: &'statement Statement,
    binds: &[Value],
) -> Result<PgQuery<'statement>, ExecuteError> {
    let mut query = sqlx::query(statement.sql());
    for position in statement.bind_order() {
        let value = binds
            .get(*position as usize)
            .ok_or(ExecuteError::MissingBind {
                position: *position,
            })?;
        query = bind_value(query, value);
    }
    Ok(query)
}

/// Binds one runtime value through its exact PostgreSQL type.
fn bind_value<'query>(query: PgQuery<'query>, value: &Value) -> PgQuery<'query> {
    match value {
        Value::Null(column_type) => bind_null(query, *column_type),
        Value::Boolean(value) => query.bind(*value),
        Value::Int16(value) => query.bind(*value),
        Value::Int32(value) => query.bind(*value),
        Value::Int64(value) => query.bind(*value),
        Value::Float32(value) => query.bind(*value),
        Value::Float64(value) => query.bind(*value),
        Value::Text(value) => query.bind(value.clone()),
        Value::Bytes(value) => query.bind(value.clone()),
        Value::Date(value) => query.bind(*value),
        Value::Time(value) => query.bind(*value),
        Value::Timestamp(value) => query.bind(*value),
        Value::TimestampUtc(value) => query.bind(*value),
        Value::Uuid(value) => query.bind(*value),
        Value::Json(value) => query.bind(value.clone()),
    }
}

/// Binds SQL `NULL` with the driver-visible type of its column.
fn bind_null(query: PgQuery<'_>, column_type: ColumnType) -> PgQuery<'_> {
    match column_type {
        ColumnType::Boolean => query.bind(None::<bool>),
        ColumnType::Int16 => query.bind(None::<i16>),
        ColumnType::Int32 => query.bind(None::<i32>),
        ColumnType::Int64 => query.bind(None::<i64>),
        ColumnType::Float32 => query.bind(None::<f32>),
        ColumnType::Float64 => query.bind(None::<f64>),
        ColumnType::Text => query.bind(None::<String>),
        ColumnType::Bytes => query.bind(None::<Vec<u8>>),
        ColumnType::Date => query.bind(None::<NaiveDate>),
        ColumnType::Time => query.bind(None::<NaiveTime>),
        ColumnType::Timestamp => query.bind(None::<NaiveDateTime>),
        ColumnType::TimestampUtc => query.bind(None::<DateTime<Utc>>),
        ColumnType::Uuid => query.bind(None::<Uuid>),
        ColumnType::Json => query.bind(None::<serde_json::Value>),
    }
}

/// Decodes one driver row into positional values in column order.
pub(crate) fn decode_row(
    row: &PgRow,
    columns: &'static [ColumnMeta],
) -> Result<Vec<Value>, ExecuteError> {
    let mut values = Vec::with_capacity(columns.len());
    for (index, column) in columns.iter().enumerate() {
        values.push(decode_column(row, index, column.column_type())?);
    }
    Ok(values)
}

fn decode_column(
    row: &PgRow,
    index: usize,
    column_type: ColumnType,
) -> Result<Value, ExecuteError> {
    fn wrap<T>(
        decoded: Option<T>,
        column_type: ColumnType,
        into_value: impl FnOnce(T) -> Value,
    ) -> Value {
        decoded.map_or(Value::Null(column_type), into_value)
    }

    Ok(match column_type {
        ColumnType::Boolean => wrap(row.try_get(index)?, column_type, Value::Boolean),
        ColumnType::Int16 => wrap(row.try_get(index)?, column_type, Value::Int16),
        ColumnType::Int32 => wrap(row.try_get(index)?, column_type, Value::Int32),
        ColumnType::Int64 => wrap(row.try_get(index)?, column_type, Value::Int64),
        ColumnType::Float32 => wrap(row.try_get(index)?, column_type, Value::Float32),
        ColumnType::Float64 => wrap(row.try_get(index)?, column_type, Value::Float64),
        ColumnType::Text => wrap(row.try_get(index)?, column_type, Value::Text),
        ColumnType::Bytes => wrap(row.try_get(index)?, column_type, Value::Bytes),
        ColumnType::Date => wrap(row.try_get(index)?, column_type, Value::Date),
        ColumnType::Time => wrap(row.try_get(index)?, column_type, Value::Time),
        ColumnType::Timestamp => wrap(row.try_get(index)?, column_type, Value::Timestamp),
        ColumnType::TimestampUtc => wrap(row.try_get(index)?, column_type, Value::TimestampUtc),
        ColumnType::Uuid => wrap(row.try_get(index)?, column_type, Value::Uuid),
        ColumnType::Json => wrap(row.try_get(index)?, column_type, Value::Json),
    })
}

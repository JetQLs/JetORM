use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use jetorm_dialect::Statement;
use jetorm_entity::{ColumnType, SqlValue, Value};
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
        query = bind_value(query, value, *position)?;
    }
    Ok(query)
}

/// Binds one runtime value through its exact PostgreSQL type.
fn bind_value<'query>(
    query: PgQuery<'query>,
    value: &Value,
    position: u32,
) -> Result<PgQuery<'query>, ExecuteError> {
    Ok(match value {
        Value::Null(column_type) => bind_null(query, *column_type),
        Value::Boolean(value) => query.bind(*value),
        Value::Int16(value) => query.bind(*value),
        Value::Int32(value) => query.bind(*value),
        Value::Int64(value) => query.bind(*value),
        Value::Float32(value) => query.bind(*value),
        Value::Float64(value) => query.bind(*value),
        Value::Decimal(value) => query.bind(*value),
        Value::Text(value) => query.bind(value.clone()),
        Value::Bytes(value) => query.bind(value.clone()),
        Value::Date(value) => query.bind(*value),
        Value::Time(value) => query.bind(*value),
        Value::Timestamp(value) => query.bind(*value),
        Value::TimestampUtc(value) => query.bind(*value),
        Value::Uuid(value) => query.bind(*value),
        Value::Json(value) => query.bind(value.clone()),
        Value::Array { element, values } => bind_array(query, *element, values, position)?,
    })
}

/// Binds a homogeneous array as one PostgreSQL array parameter.
///
/// The query layer constructs arrays through typed conversion, so every
/// element already holds the declared kind; a stray mismatch is reported as
/// a malformed bind rather than silently coerced.
fn bind_array<'query>(
    query: PgQuery<'query>,
    element: ColumnType,
    values: &[Value],
    position: u32,
) -> Result<PgQuery<'query>, ExecuteError> {
    fn collect<T: SqlValue>(values: &[Value], position: u32) -> Result<Vec<T>, ExecuteError> {
        values
            .iter()
            .map(|value| {
                T::from_value(value.clone()).map_err(|mismatch| ExecuteError::MalformedBind {
                    position,
                    detail: mismatch.to_string(),
                })
            })
            .collect()
    }

    Ok(match element {
        ColumnType::Boolean => query.bind(collect::<bool>(values, position)?),
        ColumnType::Int16 => query.bind(collect::<i16>(values, position)?),
        ColumnType::Int32 => query.bind(collect::<i32>(values, position)?),
        ColumnType::Int64 => query.bind(collect::<i64>(values, position)?),
        ColumnType::Float32 => query.bind(collect::<f32>(values, position)?),
        ColumnType::Float64 => query.bind(collect::<f64>(values, position)?),
        ColumnType::Decimal => query.bind(collect::<rust_decimal::Decimal>(values, position)?),
        ColumnType::Text => query.bind(collect::<String>(values, position)?),
        ColumnType::Bytes => query.bind(collect::<Vec<u8>>(values, position)?),
        ColumnType::Date => query.bind(collect::<NaiveDate>(values, position)?),
        ColumnType::Time => query.bind(collect::<NaiveTime>(values, position)?),
        ColumnType::Timestamp => query.bind(collect::<NaiveDateTime>(values, position)?),
        ColumnType::TimestampUtc => query.bind(collect::<DateTime<Utc>>(values, position)?),
        ColumnType::Uuid => query.bind(collect::<Uuid>(values, position)?),
        ColumnType::Json => query.bind(collect::<serde_json::Value>(values, position)?),
        ColumnType::ArrayOf(_) => {
            return Err(ExecuteError::MalformedBind {
                position,
                detail: "arrays cannot nest".to_owned(),
            });
        }
    })
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
        ColumnType::Decimal => query.bind(None::<rust_decimal::Decimal>),
        ColumnType::Text => query.bind(None::<String>),
        ColumnType::Bytes => query.bind(None::<Vec<u8>>),
        ColumnType::Date => query.bind(None::<NaiveDate>),
        ColumnType::Time => query.bind(None::<NaiveTime>),
        ColumnType::Timestamp => query.bind(None::<NaiveDateTime>),
        ColumnType::TimestampUtc => query.bind(None::<DateTime<Utc>>),
        ColumnType::Uuid => query.bind(None::<Uuid>),
        ColumnType::Json => query.bind(None::<serde_json::Value>),
        ColumnType::ArrayOf(element) => bind_null_array(query, element),
    }
}

/// Binds SQL `NULL` typed as an array of the element kind.
fn bind_null_array(query: PgQuery<'_>, element: jetorm_entity::ElementType) -> PgQuery<'_> {
    use jetorm_entity::ElementType;
    match element {
        ElementType::Boolean => query.bind(None::<Vec<bool>>),
        ElementType::Int16 => query.bind(None::<Vec<i16>>),
        ElementType::Int32 => query.bind(None::<Vec<i32>>),
        ElementType::Int64 => query.bind(None::<Vec<i64>>),
        ElementType::Float32 => query.bind(None::<Vec<f32>>),
        ElementType::Float64 => query.bind(None::<Vec<f64>>),
        ElementType::Decimal => query.bind(None::<Vec<rust_decimal::Decimal>>),
        ElementType::Text => query.bind(None::<Vec<String>>),
        ElementType::Date => query.bind(None::<Vec<NaiveDate>>),
        ElementType::Time => query.bind(None::<Vec<NaiveTime>>),
        ElementType::Timestamp => query.bind(None::<Vec<NaiveDateTime>>),
        ElementType::TimestampUtc => query.bind(None::<Vec<DateTime<Utc>>>),
        ElementType::Uuid => query.bind(None::<Vec<Uuid>>),
    }
}

/// Decodes one driver row into positional values of the given types.
pub(crate) fn decode_row(
    row: &PgRow,
    columns: &[ColumnType],
) -> Result<crate::row::JetRow, ExecuteError> {
    let mut values = Vec::with_capacity(columns.len());
    for (index, column_type) in columns.iter().enumerate() {
        values.push(decode_column(row, index, *column_type)?);
    }
    Ok(crate::row::JetRow::new(values))
}

pub(crate) fn decode_column(
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
        ColumnType::Decimal => wrap(row.try_get(index)?, column_type, Value::Decimal),
        ColumnType::Text => wrap(row.try_get(index)?, column_type, Value::Text),
        ColumnType::Bytes => wrap(row.try_get(index)?, column_type, Value::Bytes),
        ColumnType::Date => wrap(row.try_get(index)?, column_type, Value::Date),
        ColumnType::Time => wrap(row.try_get(index)?, column_type, Value::Time),
        ColumnType::Timestamp => wrap(row.try_get(index)?, column_type, Value::Timestamp),
        ColumnType::TimestampUtc => wrap(row.try_get(index)?, column_type, Value::TimestampUtc),
        ColumnType::Uuid => wrap(row.try_get(index)?, column_type, Value::Uuid),
        ColumnType::Json => wrap(row.try_get(index)?, column_type, Value::Json),
        ColumnType::ArrayOf(element) => decode_array(row, index, element)?,
    })
}

/// Decodes one array column into element values.
fn decode_array(
    row: &PgRow,
    index: usize,
    element: jetorm_entity::ElementType,
) -> Result<Value, ExecuteError> {
    use jetorm_entity::{ElementType, SqlValue};

    // Elements decode as options: PostgreSQL permits NULL elements in any
    // array column, JetORM's own DDL included, even though `Vec<T>` cannot
    // hold one. An external writer's NULL element must fail as a clear
    // named condition, not a raw driver error.
    fn wrap<T: SqlValue>(
        decoded: Option<Vec<Option<T>>>,
        element: ElementType,
        index: usize,
    ) -> Result<Value, ExecuteError> {
        match decoded {
            Some(values) => {
                let values = values
                    .into_iter()
                    .map(|value| {
                        value
                            .map(SqlValue::into_value)
                            .ok_or_else(|| ExecuteError::ArrayDecode {
                                column: index,
                                detail: format!(
                                    "stored array contains a NULL element; the \
                                     entity's field holds non-null {element:?} \
                                     elements"
                                ),
                            })
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Value::Array {
                    element: element.as_column_type(),
                    values,
                })
            }
            None => Ok(Value::Null(jetorm_entity::ColumnType::ArrayOf(element))),
        }
    }

    match element {
        ElementType::Boolean => wrap::<bool>(row.try_get(index)?, element, index),
        ElementType::Int16 => wrap::<i16>(row.try_get(index)?, element, index),
        ElementType::Int32 => wrap::<i32>(row.try_get(index)?, element, index),
        ElementType::Int64 => wrap::<i64>(row.try_get(index)?, element, index),
        ElementType::Float32 => wrap::<f32>(row.try_get(index)?, element, index),
        ElementType::Float64 => wrap::<f64>(row.try_get(index)?, element, index),
        ElementType::Decimal => wrap::<rust_decimal::Decimal>(row.try_get(index)?, element, index),
        ElementType::Text => wrap::<String>(row.try_get(index)?, element, index),
        ElementType::Date => wrap::<NaiveDate>(row.try_get(index)?, element, index),
        ElementType::Time => wrap::<NaiveTime>(row.try_get(index)?, element, index),
        ElementType::Timestamp => wrap::<NaiveDateTime>(row.try_get(index)?, element, index),
        ElementType::TimestampUtc => wrap::<DateTime<Utc>>(row.try_get(index)?, element, index),
        ElementType::Uuid => wrap::<Uuid>(row.try_get(index)?, element, index),
    }
}

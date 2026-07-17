use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use uuid::Uuid;

use crate::error::ValueTypeMismatch;
use crate::meta::ColumnType;

/// Runtime value currency exchanged between models, binds, and row decoding.
///
/// A `Value` pairs one Rust payload with its dialect-independent SQL kind.
/// SQL `NULL` retains its column type explicitly so parameter binding and
/// migration diffing never have to guess the type of an absent value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// SQL `NULL` for the retained column type.
    Null(ColumnType),
    /// Boolean value.
    Boolean(bool),
    /// 16-bit signed integer value.
    Int16(i16),
    /// 32-bit signed integer value.
    Int32(i32),
    /// 64-bit signed integer value.
    Int64(i64),
    /// 32-bit floating-point value.
    Float32(f32),
    /// 64-bit floating-point value.
    Float64(f64),
    /// Unicode text value.
    Text(String),
    /// Opaque byte-sequence value.
    Bytes(Vec<u8>),
    /// Calendar date value.
    Date(NaiveDate),
    /// Time-of-day value.
    Time(NaiveTime),
    /// Date-and-time value without time-zone semantics.
    Timestamp(NaiveDateTime),
    /// Date-and-time value in Coordinated Universal Time.
    TimestampUtc(DateTime<Utc>),
    /// Universally unique identifier value.
    Uuid(Uuid),
    /// Structured JSON value.
    Json(serde_json::Value),
    /// Homogeneous array value, above all for `IN`-style membership binds.
    Array {
        /// Kind of every element.
        element: ColumnType,
        /// Elements in order; each holds the declared element kind.
        values: Vec<Value>,
    },
}

impl Value {
    /// Returns the dialect-independent column type of this value.
    #[must_use]
    pub const fn column_type(&self) -> ColumnType {
        match self {
            Self::Null(column_type) => *column_type,
            Self::Boolean(_) => ColumnType::Boolean,
            Self::Int16(_) => ColumnType::Int16,
            Self::Int32(_) => ColumnType::Int32,
            Self::Int64(_) => ColumnType::Int64,
            Self::Float32(_) => ColumnType::Float32,
            Self::Float64(_) => ColumnType::Float64,
            Self::Text(_) => ColumnType::Text,
            Self::Bytes(_) => ColumnType::Bytes,
            Self::Date(_) => ColumnType::Date,
            Self::Time(_) => ColumnType::Time,
            Self::Timestamp(_) => ColumnType::Timestamp,
            Self::TimestampUtc(_) => ColumnType::TimestampUtc,
            Self::Uuid(_) => ColumnType::Uuid,
            Self::Json(_) => ColumnType::Json,
            // Columns cannot hold arrays yet, so an array value reports the
            // kind of its elements — the type a membership test compares.
            Self::Array { element, .. } => *element,
        }
    }

    /// Reports whether this value is SQL `NULL`.
    #[must_use]
    pub const fn is_null(&self) -> bool {
        matches!(self, Self::Null(_))
    }

    /// Returns the payload variant name used in decode diagnostics.
    #[must_use]
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Null(_) => "null",
            Self::Boolean(_) => "boolean",
            Self::Int16(_) => "int16",
            Self::Int32(_) => "int32",
            Self::Int64(_) => "int64",
            Self::Float32(_) => "float32",
            Self::Float64(_) => "float64",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
            Self::Date(_) => "date",
            Self::Time(_) => "time",
            Self::Timestamp(_) => "timestamp",
            Self::TimestampUtc(_) => "timestamp with time zone",
            Self::Uuid(_) => "uuid",
            Self::Json(_) => "json",
            Self::Array { .. } => "array",
        }
    }
}

/// Bidirectional conversion between one Rust field type and [`Value`].
///
/// The trait fixes the canonical [`ColumnType`] of the Rust type, so derive
/// macros and expression builders can name a column's SQL kind without a
/// runtime witness. `Option<T>` composes over any implementation and maps
/// `None` onto a typed SQL `NULL`.
pub trait SqlValue: Sized {
    /// Canonical dialect-independent column type of this Rust type.
    const COLUMN_TYPE: ColumnType;

    /// Whether this Rust type itself models SQL `NULL` (`Option<T>`).
    const NULLABLE: bool = false;

    /// Converts the Rust value into the runtime value currency.
    fn into_value(self) -> Value;

    /// Reconstructs the Rust value from the runtime value currency.
    ///
    /// # Errors
    ///
    /// Returns a mismatch when the payload kind does not match this type.
    fn from_value(value: Value) -> Result<Self, ValueTypeMismatch>;
}

macro_rules! impl_sql_value {
    ($rust:ty, $column_type:ident, $variant:ident) => {
        impl SqlValue for $rust {
            const COLUMN_TYPE: ColumnType = ColumnType::$column_type;

            fn into_value(self) -> Value {
                Value::$variant(self)
            }

            fn from_value(value: Value) -> Result<Self, ValueTypeMismatch> {
                match value {
                    Value::$variant(inner) => Ok(inner),
                    other => Err(ValueTypeMismatch::new(
                        ColumnType::$column_type,
                        other.kind_name(),
                    )),
                }
            }
        }
    };
}

impl_sql_value!(bool, Boolean, Boolean);
impl_sql_value!(i16, Int16, Int16);
impl_sql_value!(i32, Int32, Int32);
impl_sql_value!(i64, Int64, Int64);
impl_sql_value!(f32, Float32, Float32);
impl_sql_value!(f64, Float64, Float64);
impl_sql_value!(String, Text, Text);
impl_sql_value!(Vec<u8>, Bytes, Bytes);
impl_sql_value!(NaiveDate, Date, Date);
impl_sql_value!(NaiveTime, Time, Time);
impl_sql_value!(NaiveDateTime, Timestamp, Timestamp);
impl_sql_value!(DateTime<Utc>, TimestampUtc, TimestampUtc);
impl_sql_value!(Uuid, Uuid, Uuid);
impl_sql_value!(serde_json::Value, Json, Json);

impl<T> SqlValue for Option<T>
where
    T: SqlValue,
{
    const COLUMN_TYPE: ColumnType = T::COLUMN_TYPE;
    const NULLABLE: bool = true;

    fn into_value(self) -> Value {
        match self {
            Some(inner) => inner.into_value(),
            None => Value::Null(T::COLUMN_TYPE),
        }
    }

    fn from_value(value: Value) -> Result<Self, ValueTypeMismatch> {
        match value {
            Value::Null(_) => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}

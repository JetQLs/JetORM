use afterburner::ir::{SqlType, TimeZone};

use crate::error::RenderError;
use crate::postgres::quote_identifier;

/// Maximum fractional-second digits PostgreSQL stores for temporal types.
const MAX_TEMPORAL_PRECISION: u8 = 6;

/// Renders the PostgreSQL type name for one IR scalar kind.
///
/// Nullability is not part of a SQL type; callers apply it through table
/// definitions or leave it to expression semantics.
pub(crate) fn type_name(kind: &SqlType) -> Result<String, RenderError> {
    match kind {
        SqlType::Boolean => Ok("boolean".to_owned()),
        SqlType::Integer {
            bits: 16,
            signed: true,
        } => Ok("smallint".to_owned()),
        SqlType::Integer {
            bits: 32,
            signed: true,
        } => Ok("integer".to_owned()),
        SqlType::Integer {
            bits: 64,
            signed: true,
        } => Ok("bigint".to_owned()),
        SqlType::Integer { signed: false, .. } => Err(RenderError::unsupported(
            "PostgreSQL has no unsigned integer types",
        )),
        SqlType::Integer { bits, .. } => Err(RenderError::unsupported(format!(
            "PostgreSQL has no {bits}-bit integer type"
        ))),
        SqlType::Float { bits: 32 } => Ok("real".to_owned()),
        SqlType::Float { bits: 64 } => Ok("double precision".to_owned()),
        SqlType::Float { bits } => Err(RenderError::unsupported(format!(
            "PostgreSQL has no {bits}-bit float type"
        ))),
        SqlType::Decimal { precision, scale } => Ok(format!("numeric({precision}, {scale})")),
        SqlType::Utf8 => Ok("text".to_owned()),
        SqlType::Binary => Ok("bytea".to_owned()),
        SqlType::Date => Ok("date".to_owned()),
        SqlType::Time { precision } => {
            check_temporal_precision(*precision)?;
            Ok(format!("time({precision})"))
        }
        SqlType::Timestamp {
            precision,
            timezone,
        } => {
            check_temporal_precision(*precision)?;
            match timezone {
                TimeZone::Naive => Ok(format!("timestamp({precision})")),
                TimeZone::Utc => Ok(format!("timestamptz({precision})")),
                TimeZone::Named(zone) => Err(RenderError::unsupported(format!(
                    "PostgreSQL types carry no fixed time zone; cannot render zone {zone:?}"
                ))),
            }
        }
        SqlType::Interval => Ok("interval".to_owned()),
        SqlType::Uuid => Ok("uuid".to_owned()),
        SqlType::Json => Ok("jsonb".to_owned()),
        SqlType::Custom(name) => custom_type_name(name),
    }
}

fn check_temporal_precision(precision: u8) -> Result<(), RenderError> {
    if precision > MAX_TEMPORAL_PRECISION {
        return Err(RenderError::unsupported(format!(
            "PostgreSQL temporal precision is at most {MAX_TEMPORAL_PRECISION}, got {precision}"
        )));
    }
    Ok(())
}

/// Renders a frontend-retained custom type as a quoted, optionally
/// schema-qualified type identifier.
fn custom_type_name(name: &str) -> Result<String, RenderError> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err(RenderError::unsupported(format!(
            "custom type name {name:?} is not a valid qualified identifier"
        )));
    }
    let quoted: Vec<String> = parts
        .into_iter()
        .map(quote_identifier)
        .collect::<Result<_, _>>()?;
    Ok(quoted.join("."))
}

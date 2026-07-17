use std::{error::Error, fmt};

use crate::meta::ColumnType;

/// Payload-kind mismatch found while converting one [`crate::Value`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValueTypeMismatch {
    expected: ColumnType,
    actual: &'static str,
}

impl ValueTypeMismatch {
    /// Creates a mismatch between an expected column type and a payload kind.
    #[must_use]
    pub const fn new(expected: ColumnType, actual: &'static str) -> Self {
        Self { expected, actual }
    }

    /// Returns the column type required by the Rust target type.
    #[must_use]
    pub const fn expected(&self) -> ColumnType {
        self.expected
    }

    /// Returns the kind name of the rejected payload.
    #[must_use]
    pub const fn actual(&self) -> &'static str {
        self.actual
    }
}

impl fmt::Display for ValueTypeMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "expected {:?} value, found {} payload",
            self.expected, self.actual
        )
    }
}

impl Error for ValueTypeMismatch {}

/// Failure produced while reconstructing a model from positional values.
///
/// The set of failure modes grows as entities gain features, so callers must
/// handle unknown variants.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// The row width does not match the entity's column count.
    ColumnCount {
        /// Number of columns declared by the entity.
        expected: usize,
        /// Number of values supplied by the row.
        actual: usize,
    },
    /// One column's payload kind does not match its Rust field type.
    Column {
        /// SQL name of the mismatched column.
        name: &'static str,
        /// Underlying payload-kind mismatch.
        mismatch: ValueTypeMismatch,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ColumnCount { expected, actual } => write!(
                formatter,
                "row has {actual} values but the entity declares {expected} columns"
            ),
            Self::Column { name, mismatch } => {
                write!(formatter, "column {name:?}: {mismatch}")
            }
        }
    }
}

impl Error for DecodeError {}

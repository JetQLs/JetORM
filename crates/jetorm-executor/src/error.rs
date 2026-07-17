use std::{error::Error, fmt};

use jetorm_dialect::{RenderError, StatementResult};
use jetorm_entity::DecodeError;
use jetorm_query::LoweringError;

/// Failure produced while executing a JetORM statement.
///
/// The set of failure modes grows as JetORM gains write paths and
/// dialect-level features, so callers must handle unknown variants.
#[derive(Debug)]
#[non_exhaustive]
pub enum ExecuteError {
    /// The typed builder could not be lowered into IR.
    Build(LoweringError),
    /// The lowered module failed verification or could not be rendered as
    /// dialect SQL.
    Render(RenderError),
    /// The database driver reported a connection or execution failure.
    Database(sqlx::Error),
    /// The statement referenced a bind position the builder never captured;
    /// this indicates a bug in lowering or rendering.
    MissingBind {
        /// Bind-table position the statement expected.
        position: u32,
    },
    /// A bind value could not be handed to the driver — for example an
    /// array whose element disagrees with its declared kind. This indicates
    /// a bug in query construction.
    MalformedBind {
        /// Bind-table position of the rejected value.
        position: u32,
        /// Why the driver binding failed.
        detail: String,
    },
    /// A relation load could not read or interpret a join key.
    Relation {
        /// Why the load failed.
        detail: String,
    },
    /// The execution API disagreed with the statement's declared result shape.
    ResultMismatch {
        /// Result shape required by the execution API.
        expected: StatementResult,
        /// Result shape declared by the rendered statement.
        actual: StatementResult,
    },
    /// A stored array value holds a state the entity's field type cannot:
    /// a `NULL` element or extra dimensions. JetORM never writes such
    /// values; an external writer did.
    ArrayDecode {
        /// Zero-based result-set index of the offending column.
        column: usize,
        /// What the stored array holds that the field cannot.
        detail: String,
    },
    /// One fetched row could not be decoded into the entity's model.
    Decode {
        /// Zero-based index of the offending row in the result set.
        row: usize,
        /// Underlying positional decode failure.
        source: DecodeError,
    },
    /// A projected select was executed as a full-model fetch.
    ///
    /// A projection reorders or narrows the SELECT list, while full-model
    /// decoding assigns values to fields positionally — running one as the
    /// other would either fail confusingly or, worse, transpose same-typed
    /// columns without any error. Execute projected queries through
    /// [`crate::ProjectedExecute`] instead.
    ProjectedModelFetch,
}

/// Driver-independent classification of an execution failure.
///
/// Callers branch on this instead of parsing SQLSTATE codes or matching
/// driver error types, so handling "the email is taken" never couples an
/// application to `sqlx` internals. The set grows with JetORM's feature
/// surface; unknown variants must be handled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// A unique constraint rejected a duplicate value.
    UniqueViolation,
    /// A foreign-key constraint rejected the change.
    ForeignKeyViolation,
    /// A `NOT NULL` constraint rejected an absent value.
    NotNullViolation,
    /// A `CHECK` constraint rejected the change.
    CheckViolation,
    /// The connection could not be established, timed out, or was lost.
    Connection,
    /// A failure outside the classified cases.
    Other,
}

impl ExecuteError {
    pub(crate) const fn lowering(error: LoweringError) -> Self {
        Self::Build(error)
    }

    /// Classifies this failure independently of the driver.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        let Self::Database(error) = self else {
            return ErrorKind::Other;
        };
        match error {
            sqlx::Error::Database(database) => match database.kind() {
                sqlx::error::ErrorKind::UniqueViolation => ErrorKind::UniqueViolation,
                sqlx::error::ErrorKind::ForeignKeyViolation => ErrorKind::ForeignKeyViolation,
                sqlx::error::ErrorKind::NotNullViolation => ErrorKind::NotNullViolation,
                sqlx::error::ErrorKind::CheckViolation => ErrorKind::CheckViolation,
                _ => ErrorKind::Other,
            },
            sqlx::Error::PoolTimedOut
            | sqlx::Error::PoolClosed
            | sqlx::Error::Io(_)
            | sqlx::Error::Tls(_) => ErrorKind::Connection,
            _ => ErrorKind::Other,
        }
    }
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => write!(formatter, "query lowering failed: {error}"),
            Self::Render(error) => write!(formatter, "SQL rendering failed: {error}"),
            Self::Database(error) => write!(formatter, "database error: {error}"),
            Self::ArrayDecode { column, detail } => write!(
                formatter,
                "column {column}: {detail}"
            ),
            Self::MissingBind { position } => write!(
                formatter,
                "statement references bind position {position} that the query never captured"
            ),
            Self::MalformedBind { position, detail } => {
                write!(formatter, "bind position {position} is malformed: {detail}")
            }
            Self::Relation { detail } => write!(formatter, "relation load failed: {detail}"),
            Self::ResultMismatch { expected, actual } => write!(
                formatter,
                "execution expected {expected:?}, but the statement produces {actual:?}"
            ),
            Self::Decode { row, source } => {
                write!(formatter, "row {row} could not be decoded: {source}")
            }
            Self::ProjectedModelFetch => formatter.write_str(
                "a projected select cannot fetch full models; execute it through                  ProjectedExecute",
            ),
        }
    }
}

impl Error for ExecuteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Render(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::MissingBind { .. }
            | Self::MalformedBind { .. }
            | Self::ArrayDecode { .. }
            | Self::Relation { .. }
            | Self::ResultMismatch { .. }
            | Self::ProjectedModelFetch => None,
            Self::Decode { source, .. } => Some(source),
        }
    }
}

impl From<RenderError> for ExecuteError {
    fn from(error: RenderError) -> Self {
        Self::Render(error)
    }
}

impl From<sqlx::Error> for ExecuteError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

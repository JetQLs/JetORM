use std::{error::Error, fmt};

use afterburner::AfterBurnerError;
use afterburner::ir::FingerprintError;
use jetorm_dialect::RenderError;
use jetorm_entity::DecodeError;
use jetorm_query::LoweringError;

/// Failure produced while executing a JetORM query.
#[derive(Debug)]
pub enum ExecuteError {
    /// The typed query could not be lowered into verified IR.
    Build(AfterBurnerError<LoweringError>),
    /// The verified module could not be fingerprinted for the plan cache.
    Fingerprint(FingerprintError),
    /// The verified module could not be rendered as dialect SQL.
    Render(RenderError),
    /// The database driver reported a connection or execution failure.
    Database(sqlx::Error),
    /// The statement referenced a bind position the query never captured;
    /// this indicates a bug in lowering or rendering.
    MissingBind {
        /// Bind-table position the statement expected.
        position: u32,
    },
    /// One fetched row could not be decoded into the entity's model.
    Decode {
        /// Zero-based index of the offending row in the result set.
        row: usize,
        /// Underlying positional decode failure.
        source: DecodeError,
    },
}

impl fmt::Display for ExecuteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(error) => write!(formatter, "query lowering failed: {error}"),
            Self::Fingerprint(error) => write!(formatter, "plan-cache keying failed: {error}"),
            Self::Render(error) => write!(formatter, "SQL rendering failed: {error}"),
            Self::Database(error) => write!(formatter, "database error: {error}"),
            Self::MissingBind { position } => write!(
                formatter,
                "statement references bind position {position} that the query never captured"
            ),
            Self::Decode { row, source } => {
                write!(formatter, "row {row} could not be decoded: {source}")
            }
        }
    }
}

impl Error for ExecuteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Build(error) => Some(error),
            Self::Fingerprint(error) => Some(error),
            Self::Render(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::MissingBind { .. } => None,
            Self::Decode { source, .. } => Some(source),
        }
    }
}

impl From<FingerprintError> for ExecuteError {
    fn from(error: FingerprintError) -> Self {
        Self::Fingerprint(error)
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

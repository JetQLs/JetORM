use std::{error::Error, fmt};

use jetorm_dialect::RenderError;
use jetorm_executor::ExecuteError;
use jetorm_schema::ApplyError;

/// Failure produced while loading, replaying, or applying migrations.
///
/// The set of failure modes grows as the migration system gains features, so
/// callers must handle unknown variants.
#[derive(Debug)]
#[non_exhaustive]
pub enum MigrationError {
    /// Two migrations claim the same version.
    DuplicateVersion {
        /// The repeated version.
        version: String,
    },
    /// A migration version is not a usable identity.
    InvalidVersion {
        /// The rejected version.
        version: String,
        /// Why it was rejected.
        detail: String,
    },
    /// Replaying a migration's steps contradicted the schema they were
    /// generated against.
    Replay {
        /// Migration whose replay failed.
        version: String,
        /// Underlying schema-model failure, boxed to keep the error type
        /// small on the `Result` hot path.
        source: Box<ApplyError>,
    },
    /// The database has a migration applied that the migration set does not
    /// contain, so history cannot be interpreted.
    ///
    /// Deleting or renaming an applied migration file causes this: the
    /// recorded version no longer resolves to any steps, and the schema the
    /// database is in can no longer be reconstructed.
    UnknownAppliedVersion {
        /// Version recorded in the database.
        version: String,
    },
    /// A migration step could not be rendered as dialect SQL.
    Render {
        /// Migration whose step failed to render.
        version: String,
        /// Underlying rendering failure.
        source: RenderError,
    },
    /// The database rejected a statement or was unreachable.
    Database(ExecuteError),
    /// One or more constraints failed validation against existing rows.
    Validation {
        /// Each failed constraint with what the database said.
        failures: Vec<(String, String)>,
    },
    /// A migration file could not be read or parsed.
    File {
        /// Path that failed.
        path: String,
        /// Why it failed.
        detail: String,
    },
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateVersion { version } => {
                write!(formatter, "migration version {version:?} is declared twice")
            }
            Self::InvalidVersion { version, detail } => {
                write!(
                    formatter,
                    "migration version {version:?} is invalid: {detail}"
                )
            }
            Self::Replay { version, source } => write!(
                formatter,
                "migration {version:?} does not apply to the schema it follows: {source}"
            ),
            Self::UnknownAppliedVersion { version } => write!(
                formatter,
                "the database has migration {version:?} applied but no such migration exists; \
                 restore its file or reset the database"
            ),
            Self::Render { version, source } => {
                write!(formatter, "migration {version:?} cannot render: {source}")
            }
            Self::Database(error) => write!(formatter, "{error}"),
            Self::Validation { failures } => {
                write!(formatter, "existing rows violate: ")?;
                for (index, (constraint, detail)) in failures.iter().enumerate() {
                    if index > 0 {
                        formatter.write_str("; ")?;
                    }
                    write!(formatter, "{constraint} ({detail})")?;
                }
                write!(
                    formatter,
                    "; repair the data, then run `jet migrate validate`"
                )
            }
            Self::File { path, detail } => write!(formatter, "{path}: {detail}"),
        }
    }
}

impl Error for MigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Replay { source, .. } => Some(source.as_ref()),
            Self::Render { source, .. } => Some(source),
            Self::Database(error) => Some(error),
            Self::DuplicateVersion { .. }
            | Self::InvalidVersion { .. }
            | Self::UnknownAppliedVersion { .. }
            | Self::Validation { .. }
            | Self::File { .. } => None,
        }
    }
}

impl From<ExecuteError> for MigrationError {
    fn from(error: ExecuteError) -> Self {
        Self::Database(error)
    }
}

impl From<sqlx::Error> for MigrationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(ExecuteError::Database(error))
    }
}

use std::{error::Error, fmt};

use afterburner::ir::VerificationError;

/// Failure produced while rendering IR into dialect SQL.
///
/// The set of failure modes grows as dialects gain coverage, so callers must
/// handle unknown variants.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderError {
    /// The module failed AfterBurner IR verification.
    InvalidModule(Vec<VerificationError>),
    /// The module uses an operation, type, or shape this dialect cannot
    /// render faithfully.
    Unsupported {
        /// Human-readable description of the unsupported construct.
        detail: String,
    },
    /// A verified module contradicted the renderer's expectations; this
    /// indicates a bug in the renderer or the verifier.
    Inconsistent {
        /// Human-readable description of the contradiction.
        detail: String,
    },
}

impl RenderError {
    /// Creates an unsupported-construct diagnostic.
    #[must_use]
    pub fn unsupported(detail: impl Into<String>) -> Self {
        Self::Unsupported {
            detail: detail.into(),
        }
    }

    /// Creates an internal-contradiction diagnostic.
    #[must_use]
    pub fn inconsistent(detail: impl Into<String>) -> Self {
        Self::Inconsistent {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModule(errors) => write!(
                formatter,
                "module failed IR verification ({} errors)",
                errors.len()
            ),
            Self::Unsupported { detail } => {
                write!(formatter, "unsupported by this dialect: {detail}")
            }
            Self::Inconsistent { detail } => {
                write!(formatter, "renderer invariant violated: {detail}")
            }
        }
    }
}

impl Error for RenderError {}

use std::{convert::Infallible, error::Error, fmt};

use crate::ir::{Module, VerificationError};

/// Stable options accepted by the [`crate::afterburner!`] entry point.
///
/// IR verification is enabled by default. Additional optimizer and PGO options
/// can be added here without changing the macro syntax or frontend contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct AfterBurnerOptions {
    verify_ir: bool,
}

impl AfterBurnerOptions {
    /// Creates options with all integrity checks enabled.
    #[must_use]
    pub const fn new() -> Self {
        Self { verify_ir: true }
    }

    /// Reports whether the lowered module is verified before it is returned.
    #[must_use]
    pub const fn verifies_ir(self) -> bool {
        self.verify_ir
    }

    /// Enables or disables verification of the lowered module.
    ///
    /// Disabling verification is intended only for trusted internal pipelines
    /// that verify the same module at a later compiler boundary.
    #[must_use]
    pub const fn with_ir_verification(mut self, enabled: bool) -> Self {
        self.verify_ir = enabled;
        self
    }
}

impl Default for AfterBurnerOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Frontend contract for lowering a typed ORM query into AfterBurner IR.
///
/// Frontend crates implement this trait for their query AST. The contract does
/// not accept SQL text: parsing, model semantics, and parameter binding remain
/// owned by the frontend, while AfterBurner receives typed IR.
pub trait IntoAfterBurnerIr {
    /// Frontend-specific lowering failure.
    type Error;

    /// Consumes the typed query and constructs its complete IR module.
    ///
    /// The returned module may be unverified. [`crate::afterburner!`] verifies it
    /// by default according to [`AfterBurnerOptions`].
    ///
    /// # Errors
    ///
    /// Returns the frontend's lowering error when the query cannot be represented
    /// as AfterBurner IR.
    fn into_afterburner_ir(self) -> Result<Module, Self::Error>;
}

impl IntoAfterBurnerIr for Module {
    type Error = Infallible;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        Ok(self)
    }
}

/// Failure produced by the [`crate::afterburner!`] frontend boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AfterBurnerError<E> {
    /// The typed frontend could not lower its query into IR.
    Lowering(E),
    /// The lowered module violated one or more IR invariants.
    Verification(Vec<VerificationError>),
}

impl<E> fmt::Display for AfterBurnerError<E>
where
    E: fmt::Display,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lowering(error) => write!(formatter, "AfterBurner lowering failed: {error}"),
            Self::Verification(errors) => write!(
                formatter,
                "AfterBurner IR verification failed ({} errors)",
                errors.len()
            ),
        }
    }
}

impl<E> Error for AfterBurnerError<E>
where
    E: Error + 'static,
{
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Lowering(error) => Some(error),
            Self::Verification(_) => None,
        }
    }
}

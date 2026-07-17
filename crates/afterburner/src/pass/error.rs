use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use crate::ir::VerificationError;

use super::{Analysis, PassInfo, PipelineReport};

type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// Failure produced while computing a typed analysis.
#[derive(Debug)]
pub struct AnalysisError {
    analysis: &'static str,
    kind: AnalysisErrorKind,
}

#[derive(Debug)]
enum AnalysisErrorKind {
    Failed(BoxError),
    DependencyCycle(Vec<&'static str>),
}

impl AnalysisError {
    pub(crate) fn failed<A>(source: A::Error) -> Self
    where
        A: Analysis,
    {
        let source: BoxError = Box::new(source);
        let source = match source.downcast::<Self>() {
            Ok(error) => return *error,
            Err(source) => source,
        };
        Self {
            analysis: A::name(),
            kind: AnalysisErrorKind::Failed(source),
        }
    }

    pub(crate) const fn cycle(analysis: &'static str, cycle: Vec<&'static str>) -> Self {
        Self {
            analysis,
            kind: AnalysisErrorKind::DependencyCycle(cycle),
        }
    }

    /// Returns the analysis that could not be produced.
    #[must_use]
    pub const fn analysis(&self) -> &'static str {
        self.analysis
    }

    /// Returns the dependency cycle, including the repeated endpoint.
    #[must_use]
    pub fn dependency_cycle(&self) -> Option<&[&'static str]> {
        match &self.kind {
            AnalysisErrorKind::DependencyCycle(cycle) => Some(cycle),
            AnalysisErrorKind::Failed(_) => None,
        }
    }
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            AnalysisErrorKind::Failed(source) => {
                write!(formatter, "analysis {} failed: {source}", self.analysis)
            }
            AnalysisErrorKind::DependencyCycle(cycle) => write!(
                formatter,
                "analysis dependency cycle while computing {}: {}",
                self.analysis,
                cycle.join(" -> ")
            ),
        }
    }
}

impl Error for AnalysisError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            AnalysisErrorKind::Failed(source) => Some(source.as_ref()),
            AnalysisErrorKind::DependencyCycle(_) => None,
        }
    }
}

/// Failure returned directly by a transformation pass.
#[derive(Debug)]
pub struct PassError {
    message: Cow<'static, str>,
    source: Option<BoxError>,
}

impl PassError {
    /// Creates a failure without an underlying error.
    #[must_use]
    pub fn new(message: impl Into<Cow<'static, str>>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    /// Creates a failure with a typed underlying error.
    #[must_use]
    pub fn with_source<E>(message: impl Into<Cow<'static, str>>, source: E) -> Self
    where
        E: Error + Send + Sync + 'static,
    {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Returns the pass-provided diagnostic message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PassError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for PassError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

impl From<AnalysisError> for PassError {
    fn from(error: AnalysisError) -> Self {
        let message = format!("required analysis {} is unavailable", error.analysis());
        Self::with_source(message, error)
    }
}

/// Concrete reason pass-manager execution stopped.
#[derive(Debug)]
#[non_exhaustive]
pub enum PassFailure {
    /// The configured fixed-point iteration limit was zero.
    InvalidIterationLimit,
    /// Input verification failed before any pass ran.
    InvalidInput {
        /// Complete verifier diagnostics.
        errors: Vec<VerificationError>,
    },
    /// One pass returned an error.
    PassFailed {
        /// Failing pass invocation.
        pass: PassInfo,
        /// Pass-provided failure.
        source: PassError,
    },
    /// Strict verification rejected IR produced by one pass.
    InvalidAfterPass {
        /// Pass that produced invalid IR.
        pass: PassInfo,
        /// Complete verifier diagnostics.
        errors: Vec<VerificationError>,
    },
    /// Final output verification failed.
    InvalidOutput {
        /// Complete verifier diagnostics.
        errors: Vec<VerificationError>,
    },
}

impl PassFailure {
    /// Returns the associated pass invocation, when one exists.
    #[must_use]
    pub const fn pass(&self) -> Option<PassInfo> {
        match self {
            Self::PassFailed { pass, .. } | Self::InvalidAfterPass { pass, .. } => Some(*pass),
            Self::InvalidIterationLimit
            | Self::InvalidInput { .. }
            | Self::InvalidOutput { .. } => None,
        }
    }

    /// Returns verifier diagnostics carried by this failure.
    #[must_use]
    pub fn verification_errors(&self) -> Option<&[VerificationError]> {
        match self {
            Self::InvalidInput { errors }
            | Self::InvalidAfterPass { errors, .. }
            | Self::InvalidOutput { errors } => Some(errors),
            Self::InvalidIterationLimit | Self::PassFailed { .. } => None,
        }
    }
}

impl fmt::Display for PassFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIterationLimit => {
                formatter.write_str("fixed-point iteration limit must be greater than zero")
            }
            Self::InvalidInput { errors } => {
                write!(
                    formatter,
                    "input IR failed verification with {} error(s)",
                    errors.len()
                )
            }
            Self::PassFailed { pass, source } => write!(
                formatter,
                "pass {} at pipeline index {} failed: {source}",
                pass.name(),
                pass.index()
            ),
            Self::InvalidAfterPass { pass, errors } => write!(
                formatter,
                "pass {} produced IR with {} verification error(s)",
                pass.name(),
                errors.len()
            ),
            Self::InvalidOutput { errors } => write!(
                formatter,
                "pipeline output failed verification with {} error(s)",
                errors.len()
            ),
        }
    }
}

impl Error for PassFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::PassFailed { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Pass-manager failure paired with the partial execution report.
#[derive(Debug)]
pub struct PassManagerError {
    inner: Box<PassManagerErrorInner>,
}

#[derive(Debug)]
struct PassManagerErrorInner {
    failure: PassFailure,
    report: PipelineReport,
}

impl PassManagerError {
    pub(crate) fn new(failure: PassFailure, report: PipelineReport) -> Self {
        Self {
            inner: Box::new(PassManagerErrorInner { failure, report }),
        }
    }

    /// Returns the concrete failure.
    #[must_use]
    pub const fn failure(&self) -> &PassFailure {
        &self.inner.failure
    }

    /// Returns measurements from every pass completed before failure.
    #[must_use]
    pub const fn report(&self) -> &PipelineReport {
        &self.inner.report
    }

    /// Decomposes the error into its failure and partial report.
    #[must_use]
    pub fn into_parts(self) -> (PassFailure, PipelineReport) {
        let PassManagerErrorInner { failure, report } = *self.inner;
        (failure, report)
    }
}

impl fmt::Display for PassManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.failure.fmt(formatter)
    }
}

impl Error for PassManagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.inner.failure.source()
    }
}

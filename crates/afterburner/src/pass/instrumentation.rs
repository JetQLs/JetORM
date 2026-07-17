use std::time::Duration;

use crate::ir::Module;

use super::{AnalysisStatistics, PassFailure};

/// Stable identity of one pass invocation within a pipeline run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PassInfo {
    name: &'static str,
    index: usize,
    iteration: usize,
}

impl PassInfo {
    pub(crate) const fn new(name: &'static str, index: usize, iteration: usize) -> Self {
        Self {
            name,
            index,
            iteration,
        }
    }

    /// Returns the pass's stable diagnostic name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Returns the zero-based position in the configured pipeline.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// Returns the one-based fixed-point iteration number.
    #[must_use]
    pub const fn iteration(self) -> usize {
        self.iteration
    }
}

/// Measurements and change information for one successful pass invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PassRunReport {
    pass: PassInfo,
    duration: Duration,
    revision_before: u64,
    revision_after: u64,
    changed: bool,
    invalidated_analyses: u64,
}

impl PassRunReport {
    pub(crate) const fn new(
        pass: PassInfo,
        duration: Duration,
        revision_before: u64,
        revision_after: u64,
        changed: bool,
        invalidated_analyses: u64,
    ) -> Self {
        Self {
            pass,
            duration,
            revision_before,
            revision_after,
            changed,
            invalidated_analyses,
        }
    }

    /// Returns the pass identity.
    #[must_use]
    pub const fn pass(self) -> PassInfo {
        self.pass
    }

    /// Returns wall-clock time spent inside the pass itself.
    ///
    /// Verification and instrumentation callbacks are intentionally excluded.
    #[must_use]
    pub const fn duration(self) -> Duration {
        self.duration
    }

    /// Returns the module revision observed before the pass.
    #[must_use]
    pub const fn revision_before(self) -> u64 {
        self.revision_before
    }

    /// Returns the module revision observed after the pass.
    #[must_use]
    pub const fn revision_after(self) -> u64 {
        self.revision_after
    }

    /// Returns whether controlled IR mutation advanced the revision.
    #[must_use]
    pub const fn changed(self) -> bool {
        self.changed
    }

    /// Returns how many cached analysis results this pass discarded.
    ///
    /// This includes explicit invalidation requested by the pass and automatic
    /// invalidation of stale results after a revision change.
    #[must_use]
    pub const fn invalidated_analyses(self) -> u64 {
        self.invalidated_analyses
    }
}

/// Reason a pipeline invocation stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PipelineTermination {
    /// A normal single pipeline traversal completed.
    Completed,
    /// A fixed-point traversal completed an iteration without IR changes.
    FixedPoint,
    /// A fixed-point traversal changed IR in its final allowed iteration.
    IterationLimit,
    /// A pass, analysis, configuration, or verifier failure stopped execution.
    Failed,
}

/// Complete report for one pass-manager invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineReport {
    revision_before: u64,
    revision_after: u64,
    iterations: usize,
    changed: bool,
    termination: PipelineTermination,
    pass_runs: Vec<PassRunReport>,
    analysis_statistics: AnalysisStatistics,
}

impl PipelineReport {
    pub(crate) fn new(revision: u64, pass_capacity: usize) -> Self {
        Self {
            revision_before: revision,
            revision_after: revision,
            iterations: 0,
            changed: false,
            termination: PipelineTermination::Completed,
            pass_runs: Vec::with_capacity(pass_capacity),
            analysis_statistics: AnalysisStatistics::default(),
        }
    }

    pub(crate) fn push(&mut self, pass: PassRunReport) {
        self.changed |= pass.changed();
        self.revision_after = pass.revision_after();
        self.pass_runs.push(pass);
    }

    pub(crate) const fn complete_iteration(&mut self) {
        self.iterations += 1;
    }

    pub(crate) fn finish(
        &mut self,
        revision: u64,
        termination: PipelineTermination,
        statistics: AnalysisStatistics,
    ) {
        self.revision_after = revision;
        self.termination = termination;
        self.analysis_statistics = statistics;
    }

    /// Returns the module revision before the pipeline began.
    #[must_use]
    pub const fn revision_before(&self) -> u64 {
        self.revision_before
    }

    /// Returns the module revision when the pipeline stopped.
    #[must_use]
    pub const fn revision_after(&self) -> u64 {
        self.revision_after
    }

    /// Returns the number of complete pipeline traversals performed.
    #[must_use]
    pub const fn iterations(&self) -> usize {
        self.iterations
    }

    /// Returns whether any successful pass invocation changed IR.
    #[must_use]
    pub const fn changed(&self) -> bool {
        self.changed
    }

    /// Returns why execution stopped.
    #[must_use]
    pub const fn termination(&self) -> PipelineTermination {
        self.termination
    }

    /// Returns every successful pass invocation in execution order.
    #[must_use]
    pub fn pass_runs(&self) -> &[PassRunReport] {
        &self.pass_runs
    }

    /// Returns cumulative analysis-cache activity.
    #[must_use]
    pub const fn analysis_statistics(&self) -> AnalysisStatistics {
        self.analysis_statistics
    }
}

/// Observer hooks around pass-manager execution.
///
/// Instrumentation receives immutable module views and cannot silently mutate
/// IR or invalidate analyses. Hooks are excluded from pass timing. A hook panic
/// follows normal Rust panic semantics and is not caught by the manager.
pub trait PassInstrumentation: Send {
    /// Called once before input verification.
    fn before_pipeline(&mut self, _module: &Module) {}

    /// Called immediately before one pass invocation.
    fn before_pass(&mut self, _pass: PassInfo, _module: &Module) {}

    /// Called after a successful pass and any strict post-pass verification.
    fn after_pass(&mut self, _report: &PassRunReport, _module: &Module) {}

    /// Called once after successful pipeline completion.
    fn after_pipeline(&mut self, _report: &PipelineReport, _module: &Module) {}

    /// Called once when execution stops with a failure.
    ///
    /// A failed pass is not rolled back, so the module may contain its partial
    /// edits or verifier-invalid output. Observers must treat it as diagnostic
    /// input only.
    fn after_pipeline_failed(
        &mut self,
        _failure: &PassFailure,
        _report: &PipelineReport,
        _module: &Module,
    ) {
    }
}

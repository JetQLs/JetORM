use std::fmt;
use std::time::Instant;

use crate::ir::{Module, verify_module};

use super::analysis::AnalysisCache;
use super::{
    Pass, PassContext, PassFailure, PassInfo, PassInstrumentation, PassManagerError, PassRunReport,
    PipelineReport, PipelineTermination,
};

/// IR verification performed around a pass pipeline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VerificationPolicy {
    /// Skips verification entirely.
    ///
    /// Use only when validity is guaranteed by a surrounding boundary.
    None,
    /// Verifies the input and changed final output.
    ///
    /// This is the default production balance: verifier cost is independent of
    /// the number of passes.
    #[default]
    InputAndOutput,
    /// Verifies the input and every pass invocation that changes IR.
    ///
    /// This pinpoints the first broken invariant and is recommended while
    /// developing new transformations.
    AfterEachPass,
}

impl VerificationPolicy {
    const fn verifies_input(self) -> bool {
        !matches!(self, Self::None)
    }

    const fn verifies_each_pass(self) -> bool {
        matches!(self, Self::AfterEachPass)
    }

    const fn verifies_output(self) -> bool {
        matches!(self, Self::InputAndOutput)
    }
}

/// Ordered, reusable pipeline of module transformation passes.
///
/// A fresh analysis cache is created for each [`PassManager::run`] or
/// [`PassManager::run_to_fixed_point`] invocation, preventing results from one
/// module from leaking into another. The cache is shared by every pass and
/// fixed-point iteration within that invocation.
pub struct PassManager {
    passes: Vec<Box<dyn Pass>>,
    instrumentation: Vec<Box<dyn PassInstrumentation>>,
    verification: VerificationPolicy,
}

impl PassManager {
    /// Creates an empty pipeline using input-and-output verification.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            passes: Vec::new(),
            instrumentation: Vec::new(),
            verification: VerificationPolicy::InputAndOutput,
        }
    }

    /// Appends one pass and returns the manager for chained configuration.
    pub fn add_pass<P>(&mut self, pass: P) -> &mut Self
    where
        P: Pass + 'static,
    {
        self.passes.push(Box::new(pass));
        self
    }

    /// Appends one pass and returns the owned manager.
    #[must_use]
    pub fn with_pass<P>(mut self, pass: P) -> Self
    where
        P: Pass + 'static,
    {
        self.add_pass(pass);
        self
    }

    /// Appends one instrumentation observer.
    pub fn add_instrumentation<I>(&mut self, instrumentation: I) -> &mut Self
    where
        I: PassInstrumentation + 'static,
    {
        self.instrumentation.push(Box::new(instrumentation));
        self
    }

    /// Appends one instrumentation observer and returns the owned manager.
    #[must_use]
    pub fn with_instrumentation<I>(mut self, instrumentation: I) -> Self
    where
        I: PassInstrumentation + 'static,
    {
        self.add_instrumentation(instrumentation);
        self
    }

    /// Replaces the verification policy.
    pub const fn set_verification_policy(&mut self, policy: VerificationPolicy) -> &mut Self {
        self.verification = policy;
        self
    }

    /// Replaces the verification policy and returns the owned manager.
    #[must_use]
    pub const fn with_verification_policy(mut self, policy: VerificationPolicy) -> Self {
        self.verification = policy;
        self
    }

    /// Returns the active verification policy.
    #[must_use]
    pub const fn verification_policy(&self) -> VerificationPolicy {
        self.verification
    }

    /// Returns the number of configured passes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.passes.len()
    }

    /// Returns whether the pipeline contains no passes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.passes.is_empty()
    }

    /// Removes every configured pass while retaining policy and instrumentation.
    pub fn clear(&mut self) {
        self.passes.clear();
    }

    /// Executes the configured pipeline exactly once.
    ///
    /// # Errors
    ///
    /// Returns a contextual failure with a partial report when verification,
    /// analysis, or a pass fails.
    pub fn run(&mut self, module: &mut Module) -> Result<PipelineReport, PassManagerError> {
        self.run_internal(module, RunMode::Once)
    }

    /// Repeats the complete pipeline until an iteration makes no IR changes.
    ///
    /// Reaching `max_iterations` after a changed iteration is a successful,
    /// bounded run reported as [`PipelineTermination::IterationLimit`]. This
    /// lets latency-sensitive callers use useful partial optimization without
    /// converting a budget decision into an error.
    ///
    /// # Errors
    ///
    /// Returns an error when `max_iterations` is zero, verification fails, or a
    /// pass or required analysis fails.
    pub fn run_to_fixed_point(
        &mut self,
        module: &mut Module,
        max_iterations: usize,
    ) -> Result<PipelineReport, PassManagerError> {
        self.run_internal(module, RunMode::FixedPoint(max_iterations))
    }

    fn run_internal(
        &mut self,
        module: &mut Module,
        mode: RunMode,
    ) -> Result<PipelineReport, PassManagerError> {
        let verification = self.verification;
        let pass_capacity = self.passes.len().saturating_mul(mode.capacity_iterations());
        let mut report = PipelineReport::new(module.revision(), pass_capacity);
        let mut analyses = AnalysisCache::default();

        for observer in &mut self.instrumentation {
            observer.before_pipeline(module);
        }

        let max_iterations = match mode {
            RunMode::Once => 1,
            RunMode::FixedPoint(0) => {
                return Err(fail(
                    PassFailure::InvalidIterationLimit,
                    &mut report,
                    &analyses,
                    module,
                    &mut self.instrumentation,
                ));
            }
            RunMode::FixedPoint(iterations) => iterations,
        };

        if verification.verifies_input()
            && let Err(errors) = verify_module(module)
        {
            return Err(fail(
                PassFailure::InvalidInput { errors },
                &mut report,
                &analyses,
                module,
                &mut self.instrumentation,
            ));
        }

        let mut termination = PipelineTermination::Completed;
        for iteration_index in 0..max_iterations {
            let mut iteration_changed = false;
            for (pass_index, pass) in self.passes.iter_mut().enumerate() {
                let pass_info = PassInfo::new(pass.name(), pass_index, iteration_index + 1);
                for observer in &mut self.instrumentation {
                    observer.before_pass(pass_info, module);
                }

                let revision_before = module.revision();
                let invalidations_before = analyses.statistics().invalidations();
                let started = Instant::now();
                let outcome = {
                    let mut context = PassContext::new(&mut analyses);
                    pass.run(module, &mut context)
                };
                let duration = started.elapsed();
                let preserved = match outcome {
                    Ok(preserved) => preserved,
                    Err(source) => {
                        return Err(fail(
                            PassFailure::PassFailed {
                                pass: pass_info,
                                source,
                            },
                            &mut report,
                            &analyses,
                            module,
                            &mut self.instrumentation,
                        ));
                    }
                };
                let revision_after = module.revision();
                let changed = revision_before != revision_after;
                if changed {
                    analyses.invalidate_after_change(revision_after, &preserved);
                }
                let invalidated = analyses
                    .statistics()
                    .invalidations()
                    .saturating_sub(invalidations_before);
                let pass_report = PassRunReport::new(
                    pass_info,
                    duration,
                    revision_before,
                    revision_after,
                    changed,
                    invalidated,
                );
                report.push(pass_report);
                iteration_changed |= changed;

                if changed
                    && verification.verifies_each_pass()
                    && let Err(errors) = verify_module(module)
                {
                    return Err(fail(
                        PassFailure::InvalidAfterPass {
                            pass: pass_info,
                            errors,
                        },
                        &mut report,
                        &analyses,
                        module,
                        &mut self.instrumentation,
                    ));
                }

                for observer in &mut self.instrumentation {
                    observer.after_pass(&pass_report, module);
                }
            }
            report.complete_iteration();

            match mode {
                RunMode::Once => {
                    termination = PipelineTermination::Completed;
                    break;
                }
                RunMode::FixedPoint(_) if !iteration_changed => {
                    termination = PipelineTermination::FixedPoint;
                    break;
                }
                RunMode::FixedPoint(_) if iteration_index + 1 == max_iterations => {
                    termination = PipelineTermination::IterationLimit;
                }
                RunMode::FixedPoint(_) => {}
            }
        }

        if report.changed()
            && verification.verifies_output()
            && let Err(errors) = verify_module(module)
        {
            return Err(fail(
                PassFailure::InvalidOutput { errors },
                &mut report,
                &analyses,
                module,
                &mut self.instrumentation,
            ));
        }

        report.finish(module.revision(), termination, analyses.statistics());
        for observer in &mut self.instrumentation {
            observer.after_pipeline(&report, module);
        }
        Ok(report)
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PassManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pass_names = self
            .passes
            .iter()
            .map(|pass| pass.name())
            .collect::<Vec<_>>();
        formatter
            .debug_struct("PassManager")
            .field("passes", &pass_names)
            .field("instrumentation", &self.instrumentation.len())
            .field("verification", &self.verification)
            .finish()
    }
}

#[derive(Clone, Copy)]
enum RunMode {
    Once,
    FixedPoint(usize),
}

impl RunMode {
    const fn capacity_iterations(self) -> usize {
        match self {
            Self::Once => 1,
            Self::FixedPoint(iterations) => {
                if iterations < 4 {
                    iterations
                } else {
                    4
                }
            }
        }
    }
}

fn fail(
    failure: PassFailure,
    report: &mut PipelineReport,
    analyses: &AnalysisCache,
    module: &Module,
    instrumentation: &mut [Box<dyn PassInstrumentation>],
) -> PassManagerError {
    report.finish(
        module.revision(),
        PipelineTermination::Failed,
        analyses.statistics(),
    );
    for observer in instrumentation {
        observer.after_pipeline_failed(&failure, report, module);
    }
    PassManagerError::new(failure, report.clone())
}

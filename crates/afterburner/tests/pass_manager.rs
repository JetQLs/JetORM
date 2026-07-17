use std::convert::Infallible;
use std::error::Error;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use afterburner::ir::{
    Attribute, Field, Literal, LogicalOp, Module, OperationId, OperationSpec, Schema, SourceSpan,
    SqlType, TerminatorOp, Type, verify_module,
};
use afterburner::pass::{
    Analysis, AnalysisContext, AnalysisError, Pass, PassContext, PassError, PassFailure, PassInfo,
    PassInstrumentation, PassManager, PassRunReport, PipelineReport, PipelineTermination,
    PreservedAnalyses, VerificationPolicy,
};

struct Fixture {
    module: Module,
    values: OperationId,
    query_return: OperationId,
}

fn build_fixture() -> Fixture {
    let mut module = Module::new();
    let root = module.root_block();
    let (values, query_return) = {
        let mut editor = module.editor();
        let schema = editor.intern_schema(Schema::new(vec![Field::new(
            "id",
            Type::scalar(
                SqlType::Integer {
                    bits: 64,
                    signed: true,
                },
                false,
            ),
        )]));
        let values = editor
            .append_operation(
                root,
                OperationSpec::new(LogicalOp::Values {
                    rows: vec![vec![Literal::Integer(1)]],
                })
                .with_result(Type::relation(schema)),
            )
            .expect("values appends");
        let relation = editor.result(values, 0).expect("values has a result");
        let query_return = editor
            .append_operation(
                root,
                OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
            )
            .expect("return appends");
        (values, query_return)
    };
    verify_module(&module).expect("fixture verifies");
    Fixture {
        module,
        values,
        query_return,
    }
}

struct OperationCount;

impl Analysis for OperationCount {
    type Error = Infallible;
    type Output = usize;

    fn analyze(
        module: &Module,
        _context: &mut AnalysisContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        Ok(module.operation_ids().count())
    }
}

struct DoubleOperationCount;

impl Analysis for DoubleOperationCount {
    type Error = AnalysisError;
    type Output = usize;

    fn analyze(
        module: &Module,
        context: &mut AnalysisContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        Ok(*context.analysis::<OperationCount>(module)? * 2)
    }
}

struct CyclicA;
struct CyclicB;

impl Analysis for CyclicA {
    type Error = AnalysisError;
    type Output = ();

    fn analyze(
        module: &Module,
        context: &mut AnalysisContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        context.analysis::<CyclicB>(module)?;
        Ok(())
    }
}

impl Analysis for CyclicB {
    type Error = AnalysisError;
    type Output = ();

    fn analyze(
        module: &Module,
        context: &mut AnalysisContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        context.analysis::<CyclicA>(module)?;
        Ok(())
    }
}

struct ObserveCount;

impl Pass for ObserveCount {
    fn run(
        &mut self,
        module: &mut Module,
        context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        context.analysis::<OperationCount>(module)?;
        Ok(PreservedAnalyses::all())
    }
}

struct ObserveDoubleCount;

impl Pass for ObserveDoubleCount {
    fn run(
        &mut self,
        module: &mut Module,
        context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        context.analysis::<DoubleOperationCount>(module)?;
        Ok(PreservedAnalyses::all())
    }
}

struct Annotate {
    operation: OperationId,
    preserve_count: bool,
}

impl Pass for Annotate {
    fn run(
        &mut self,
        module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        module
            .editor()
            .set_attribute(self.operation, "observed", Attribute::Boolean(true))
            .map_err(|error| PassError::with_source("annotation failed", error))?;
        let mut preserved = PreservedAnalyses::none();
        if self.preserve_count {
            preserved.preserve::<OperationCount>();
        }
        Ok(preserved)
    }
}

struct NoChange;

impl Pass for NoChange {
    fn run(
        &mut self,
        _module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        Ok(PreservedAnalyses::none())
    }
}

#[test]
fn unchanged_passes_share_one_analysis_result() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager.add_pass(ObserveCount).add_pass(ObserveCount);

    let report = manager.run(&mut fixture.module).expect("pipeline succeeds");

    assert!(!report.changed());
    assert_eq!(report.analysis_statistics().misses(), 1);
    assert_eq!(report.analysis_statistics().hits(), 1);
    assert_eq!(report.analysis_statistics().invalidations(), 0);
}

#[test]
fn analyses_reuse_cached_dependencies() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager.add_pass(ObserveDoubleCount).add_pass(ObserveCount);

    let report = manager.run(&mut fixture.module).expect("pipeline succeeds");

    assert_eq!(report.analysis_statistics().misses(), 2);
    assert_eq!(report.analysis_statistics().hits(), 1);
}

#[test]
fn changed_passes_invalidate_only_unpreserved_analyses() {
    let mut invalidated = build_fixture();
    let mut manager = PassManager::new();
    manager
        .add_pass(ObserveCount)
        .add_pass(Annotate {
            operation: invalidated.values,
            preserve_count: false,
        })
        .add_pass(ObserveCount);
    let report = manager
        .run(&mut invalidated.module)
        .expect("invalidation pipeline succeeds");
    assert_eq!(report.analysis_statistics().misses(), 2);
    assert_eq!(report.analysis_statistics().hits(), 0);
    assert_eq!(report.analysis_statistics().invalidations(), 1);
    assert_eq!(report.pass_runs()[1].invalidated_analyses(), 1);

    let mut preserved = build_fixture();
    let mut manager = PassManager::new();
    manager
        .add_pass(ObserveCount)
        .add_pass(Annotate {
            operation: preserved.values,
            preserve_count: true,
        })
        .add_pass(ObserveCount);
    let report = manager
        .run(&mut preserved.module)
        .expect("preservation pipeline succeeds");
    assert_eq!(report.analysis_statistics().misses(), 1);
    assert_eq!(report.analysis_statistics().hits(), 1);
    assert_eq!(report.analysis_statistics().invalidations(), 0);
}

#[test]
fn no_revision_change_preserves_every_analysis_automatically() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager
        .add_pass(ObserveCount)
        .add_pass(NoChange)
        .add_pass(ObserveCount);

    let report = manager.run(&mut fixture.module).expect("pipeline succeeds");

    assert_eq!(report.analysis_statistics().misses(), 1);
    assert_eq!(report.analysis_statistics().hits(), 1);
    assert_eq!(report.analysis_statistics().invalidations(), 0);
}

struct ExplicitlyInvalidate;

impl Pass for ExplicitlyInvalidate {
    fn run(
        &mut self,
        module: &mut Module,
        context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        context.analysis::<OperationCount>(module)?;
        assert!(context.invalidate::<OperationCount>());
        Ok(PreservedAnalyses::all())
    }
}

#[test]
fn explicit_invalidation_is_attributed_to_the_running_pass() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager
        .add_pass(ExplicitlyInvalidate)
        .add_pass(ObserveCount);

    let report = manager.run(&mut fixture.module).expect("pipeline succeeds");

    assert_eq!(report.analysis_statistics().misses(), 2);
    assert_eq!(report.analysis_statistics().invalidations(), 1);
    assert_eq!(report.pass_runs()[0].invalidated_analyses(), 1);
}

struct AnalyzeAcrossEdit {
    operation: OperationId,
    recomputed: Arc<AtomicBool>,
}

impl Pass for AnalyzeAcrossEdit {
    fn run(
        &mut self,
        module: &mut Module,
        context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        let before = context.analysis::<OperationCount>(module)?;
        module
            .editor()
            .set_source_span(self.operation, Some(SourceSpan::new("query", 0, 1)))
            .map_err(|error| PassError::with_source("source annotation failed", error))?;
        let after = context.analysis::<OperationCount>(module)?;
        self.recomputed
            .store(!Arc::ptr_eq(&before, &after), Ordering::Relaxed);
        Ok(PreservedAnalyses::none())
    }
}

#[test]
fn analysis_requests_after_an_edit_never_return_stale_results() {
    let mut fixture = build_fixture();
    let recomputed = Arc::new(AtomicBool::new(false));
    let mut manager = PassManager::new();
    manager.add_pass(AnalyzeAcrossEdit {
        operation: fixture.values,
        recomputed: Arc::clone(&recomputed),
    });

    let report = manager.run(&mut fixture.module).expect("pipeline succeeds");

    assert!(recomputed.load(Ordering::Relaxed));
    assert_eq!(report.analysis_statistics().misses(), 2);
    assert_eq!(report.analysis_statistics().invalidations(), 0);
}

struct RequestCycle;

impl Pass for RequestCycle {
    fn run(
        &mut self,
        module: &mut Module,
        context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        context.analysis::<CyclicA>(module)?;
        Ok(PreservedAnalyses::all())
    }
}

#[test]
fn analysis_dependency_cycles_are_contextual_errors() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager.add_pass(RequestCycle);

    let error = manager
        .run(&mut fixture.module)
        .expect_err("cycle must fail");
    let PassFailure::PassFailed { pass, source } = error.failure() else {
        panic!("expected pass failure");
    };
    assert_eq!(pass.index(), 0);
    let analysis = source
        .source()
        .and_then(|source| source.downcast_ref::<AnalysisError>())
        .expect("analysis error is retained as the source");
    let cycle = analysis.dependency_cycle().expect("cycle is retained");
    assert_eq!(cycle.first(), cycle.last());
    assert_eq!(cycle.len(), 3);
    assert_eq!(error.report().termination(), PipelineTermination::Failed);
}

struct EraseReturn {
    operation: OperationId,
}

impl Pass for EraseReturn {
    fn run(
        &mut self,
        module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        module
            .editor()
            .erase_operation(self.operation)
            .map_err(|error| PassError::with_source("return erasure failed", error))?;
        Ok(PreservedAnalyses::none())
    }
}

#[test]
fn strict_verification_identifies_the_first_invalidating_pass() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager
        .set_verification_policy(VerificationPolicy::AfterEachPass)
        .add_pass(EraseReturn {
            operation: fixture.query_return,
        });

    let error = manager
        .run(&mut fixture.module)
        .expect_err("invalid rewrite must fail");

    let PassFailure::InvalidAfterPass { pass, errors } = error.failure() else {
        panic!("expected post-pass verification failure");
    };
    assert_eq!(pass.index(), 0);
    assert_eq!(pass.iteration(), 1);
    assert!(!errors.is_empty());
    assert_eq!(error.report().pass_runs().len(), 1);
    assert!(error.report().pass_runs()[0].changed());
    assert_eq!(error.report().termination(), PipelineTermination::Failed);
}

#[test]
fn default_policy_rejects_invalid_changed_output() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new().with_pass(EraseReturn {
        operation: fixture.query_return,
    });

    let error = manager
        .run(&mut fixture.module)
        .expect_err("invalid final output must fail");

    let PassFailure::InvalidOutput { errors } = error.failure() else {
        panic!("expected final output verification failure");
    };
    assert!(!errors.is_empty());
    assert_eq!(error.report().pass_runs().len(), 1);
    assert!(error.report().pass_runs()[0].changed());
    assert_eq!(error.report().termination(), PipelineTermination::Failed);
}

#[test]
fn default_policy_rejects_invalid_input_before_running_passes() {
    let mut module = Module::new();
    let mut manager = PassManager::new().with_pass(NoChange);

    let error = manager
        .run(&mut module)
        .expect_err("incomplete input must fail");

    assert!(matches!(error.failure(), PassFailure::InvalidInput { .. }));
    assert_eq!(error.report().iterations(), 0);
    assert!(error.report().pass_runs().is_empty());
}

struct ChangeOnce {
    operation: OperationId,
    changed: bool,
}

impl Pass for ChangeOnce {
    fn run(
        &mut self,
        module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        if !self.changed {
            module
                .editor()
                .set_attribute(self.operation, "once", Attribute::Boolean(true))
                .map_err(|error| PassError::with_source("one-time edit failed", error))?;
            self.changed = true;
        }
        Ok(PreservedAnalyses::none())
    }
}

struct AlwaysChange {
    operation: OperationId,
    invocation: i128,
}

impl Pass for AlwaysChange {
    fn run(
        &mut self,
        module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        self.invocation += 1;
        module
            .editor()
            .set_attribute(
                self.operation,
                "invocation",
                Attribute::Integer(self.invocation),
            )
            .map_err(|error| PassError::with_source("repeated edit failed", error))?;
        Ok(PreservedAnalyses::none())
    }
}

#[test]
fn fixed_point_execution_stops_on_quiescence_or_the_iteration_limit() {
    let mut converging = build_fixture();
    let mut manager = PassManager::new().with_pass(ChangeOnce {
        operation: converging.values,
        changed: false,
    });
    let report = manager
        .run_to_fixed_point(&mut converging.module, 8)
        .expect("fixed point succeeds");
    assert_eq!(report.termination(), PipelineTermination::FixedPoint);
    assert_eq!(report.iterations(), 2);
    assert_eq!(report.pass_runs().len(), 2);
    assert!(report.pass_runs()[0].changed());
    assert!(!report.pass_runs()[1].changed());

    let mut bounded = build_fixture();
    let mut manager = PassManager::new().with_pass(AlwaysChange {
        operation: bounded.values,
        invocation: 0,
    });
    let report = manager
        .run_to_fixed_point(&mut bounded.module, 3)
        .expect("bounded optimization succeeds");
    assert_eq!(report.termination(), PipelineTermination::IterationLimit);
    assert_eq!(report.iterations(), 3);
    assert_eq!(report.pass_runs().len(), 3);
}

#[test]
fn zero_fixed_point_limit_is_a_configuration_error() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();

    let error = manager
        .run_to_fixed_point(&mut fixture.module, 0)
        .expect_err("zero limit must fail");

    assert!(matches!(
        error.failure(),
        PassFailure::InvalidIterationLimit
    ));
    assert_eq!(error.report().termination(), PipelineTermination::Failed);
}

struct Fail;

impl Pass for Fail {
    fn name(&self) -> &'static str {
        "fail"
    }

    fn run(
        &mut self,
        _module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        Err(PassError::new("requested failure"))
    }
}

#[test]
fn pass_failures_keep_identity_and_the_partial_report() {
    let mut fixture = build_fixture();
    let mut manager = PassManager::new();
    manager.add_pass(NoChange).add_pass(Fail);

    let error = manager
        .run(&mut fixture.module)
        .expect_err("second pass must fail");

    let PassFailure::PassFailed { pass, source } = error.failure() else {
        panic!("expected pass failure");
    };
    assert_eq!(pass.name(), "fail");
    assert_eq!(pass.index(), 1);
    assert_eq!(source.message(), "requested failure");
    assert_eq!(error.report().pass_runs().len(), 1);
    assert_eq!(error.report().termination(), PipelineTermination::Failed);
}

struct NamedPass;

impl Pass for NamedPass {
    fn name(&self) -> &'static str {
        "named-pass"
    }

    fn run(
        &mut self,
        _module: &mut Module,
        _context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError> {
        Ok(PreservedAnalyses::all())
    }
}

struct Recorder {
    events: Arc<Mutex<Vec<String>>>,
}

impl PassInstrumentation for Recorder {
    fn before_pipeline(&mut self, _module: &Module) {
        self.events.lock().unwrap().push("pipeline:start".into());
    }

    fn before_pass(&mut self, pass: PassInfo, _module: &Module) {
        self.events
            .lock()
            .unwrap()
            .push(format!("pass:start:{}", pass.name()));
    }

    fn after_pass(&mut self, report: &PassRunReport, _module: &Module) {
        self.events
            .lock()
            .unwrap()
            .push(format!("pass:end:{}", report.pass().name()));
    }

    fn after_pipeline(&mut self, _report: &PipelineReport, _module: &Module) {
        self.events.lock().unwrap().push("pipeline:end".into());
    }

    fn after_pipeline_failed(
        &mut self,
        failure: &PassFailure,
        report: &PipelineReport,
        _module: &Module,
    ) {
        self.events.lock().unwrap().push(format!(
            "pipeline:failed:{}:{}",
            failure.pass().map_or("pipeline", PassInfo::name),
            report.pass_runs().len(),
        ));
    }
}

#[test]
fn instrumentation_observes_verified_execution_order() {
    let mut fixture = build_fixture();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut manager = PassManager::new()
        .with_pass(NamedPass)
        .with_instrumentation(Recorder {
            events: Arc::clone(&events),
        });

    manager.run(&mut fixture.module).expect("pipeline succeeds");

    assert_eq!(
        *events.lock().unwrap(),
        [
            "pipeline:start",
            "pass:start:named-pass",
            "pass:end:named-pass",
            "pipeline:end",
        ]
    );
}

#[test]
fn instrumentation_observes_failure_without_a_success_callback() {
    let mut fixture = build_fixture();
    let events = Arc::new(Mutex::new(Vec::new()));
    let mut manager = PassManager::new()
        .with_pass(Fail)
        .with_instrumentation(Recorder {
            events: Arc::clone(&events),
        });

    manager
        .run(&mut fixture.module)
        .expect_err("failing pass must stop the pipeline");

    assert_eq!(
        *events.lock().unwrap(),
        [
            "pipeline:start",
            "pass:start:fail",
            "pipeline:failed:fail:0",
        ]
    );
}

#[test]
fn pass_manager_is_send_between_optimizer_workers() {
    fn assert_send<T: Send>() {}
    assert_send::<PassManager>();
    assert!(std::mem::size_of::<afterburner::pass::PassManagerError>() <= 16);
}

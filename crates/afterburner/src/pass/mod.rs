//! Reusable optimization-pass execution and analysis caching.
//!
//! A [`PassManager`] owns an ordered pipeline of module passes. Passes mutate
//! IR exclusively through the normal [`crate::ir::IrEditor`] API and request
//! typed, revision-aware analyses through [`PassContext`]. The manager derives
//! change information from [`crate::ir::Module::revision`] instead of trusting
//! a pass-provided flag, selectively invalidates cached analyses, optionally
//! verifies IR boundaries, and reports timing and invalidation data.
//!
//! # Example
//!
//! ```
//! use std::convert::Infallible;
//!
//! use afterburner::ir::{
//!     Field, Literal, LogicalOp, Module, OperationSpec, Schema, SqlType,
//!     TerminatorOp, Type,
//! };
//! use afterburner::pass::{
//!     Analysis, AnalysisContext, Pass, PassContext, PassError, PassManager,
//!     PreservedAnalyses,
//! };
//!
//! struct OperationCount;
//!
//! impl Analysis for OperationCount {
//!     type Error = Infallible;
//!     type Output = usize;
//!
//!     fn analyze(
//!         module: &Module,
//!         _context: &mut AnalysisContext<'_>,
//!     ) -> Result<Self::Output, Self::Error> {
//!         Ok(module.operation_ids().count())
//!     }
//! }
//!
//! struct ObserveSize;
//!
//! impl Pass for ObserveSize {
//!     fn run(
//!         &mut self,
//!         module: &mut Module,
//!         context: &mut PassContext<'_>,
//!     ) -> Result<PreservedAnalyses, PassError> {
//!         let _operations = context.analysis::<OperationCount>(module)?;
//!         Ok(PreservedAnalyses::all())
//!     }
//! }
//!
//! let mut module = Module::new();
//! let root = module.root_block();
//! {
//!     let mut editor = module.editor();
//!     let schema = editor.intern_schema(Schema::new(vec![Field::new(
//!         "id",
//!         Type::scalar(SqlType::Integer { bits: 64, signed: true }, false),
//!     )]));
//!     let values = editor
//!         .append_operation(
//!             root,
//!             OperationSpec::new(LogicalOp::Values {
//!                 rows: vec![vec![Literal::Integer(1)]],
//!             })
//!             .with_result(Type::relation(schema)),
//!         )
//!         .unwrap();
//!     let relation = editor.result(values, 0).unwrap();
//!     editor
//!         .append_operation(
//!             root,
//!             OperationSpec::new(TerminatorOp::QueryReturn)
//!                 .with_operands(vec![relation]),
//!         )
//!         .unwrap();
//! }
//!
//! let mut manager = PassManager::new().with_pass(ObserveSize);
//! let report = manager.run(&mut module).unwrap();
//! assert!(!report.changed());
//! ```

mod analysis;
mod error;
mod instrumentation;
mod manager;

use crate::ir::Module;

pub use analysis::{Analysis, AnalysisContext, AnalysisStatistics, PassContext, PreservedAnalyses};
pub use error::{AnalysisError, PassError, PassFailure, PassManagerError};
pub use instrumentation::{
    PassInfo, PassInstrumentation, PassRunReport, PipelineReport, PipelineTermination,
};
pub use manager::{PassManager, VerificationPolicy};

/// One stateful module transformation in a [`PassManager`] pipeline.
///
/// Implementations may retain configuration and reusable scratch storage
/// between runs. A pass must use the module's controlled editing APIs so every
/// controlled IR mutation advances the module revision. The manager uses that
/// revision to determine whether the pass changed IR.
pub trait Pass: Send {
    /// Stable diagnostic name for this pass.
    ///
    /// The default is the fully qualified Rust type name and allocates nothing.
    fn name(&self) -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Runs this pass and declares which previously cached analyses remain valid.
    ///
    /// When the module revision does not change, the manager preserves every
    /// analysis regardless of the returned declaration. When it does change,
    /// undeclared stale results are discarded before the next pass runs.
    ///
    /// # Errors
    ///
    /// Returns a [`PassError`] when the transformation cannot complete. The
    /// manager retains the partial pipeline report and identifies this pass in
    /// the resulting [`PassManagerError`]. Passes should validate preconditions
    /// before editing because failed passes are not transactionally rolled back.
    fn run(
        &mut self,
        module: &mut Module,
        context: &mut PassContext<'_>,
    ) -> Result<PreservedAnalyses, PassError>;
}

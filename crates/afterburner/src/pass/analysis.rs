use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;

use crate::ir::Module;

use super::AnalysisError;

type ErasedAnalysis = dyn Any + Send + Sync;

/// Typed, module-wide analysis computed on demand by a pass.
///
/// Results are cached by analysis type and module revision. Analyses may reuse
/// other analyses through the supplied [`AnalysisContext`]; dependency cycles
/// are detected and returned as [`AnalysisError`] instead of recursing forever.
/// An analysis should depend only on revision-tracked IR. A pass that changes
/// external state or native attachments used by an analysis must explicitly
/// invalidate that type through [`PassContext::invalidate`].
pub trait Analysis: Send + Sync + 'static {
    /// Cached result type.
    type Output: Send + Sync + 'static;

    /// Failure produced while computing this analysis.
    type Error: Error + Send + Sync + 'static;

    /// Stable diagnostic name for this analysis.
    fn name() -> &'static str {
        std::any::type_name::<Self>()
    }

    /// Computes the result for the module's current revision.
    ///
    /// # Errors
    ///
    /// Returns the analysis-specific failure. The pass manager wraps it with
    /// the analysis name before exposing it to a pass.
    fn analyze(
        module: &Module,
        context: &mut AnalysisContext<'_>,
    ) -> Result<Self::Output, Self::Error>;
}

/// Cache activity accumulated during one pipeline invocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AnalysisStatistics {
    hits: u64,
    misses: u64,
    invalidations: u64,
}

impl AnalysisStatistics {
    /// Returns the number of requests served from a current cached result.
    #[must_use]
    pub const fn hits(self) -> u64 {
        self.hits
    }

    /// Returns the number of analysis computations attempted.
    #[must_use]
    pub const fn misses(self) -> u64 {
        self.misses
    }

    /// Returns the number of cached results discarded after IR changes or
    /// explicit invalidation.
    #[must_use]
    pub const fn invalidations(self) -> u64 {
        self.invalidations
    }
}

/// Analyses a transformation guarantees remain valid after its edits.
///
/// The common cases allocate nothing: [`PreservedAnalyses::all`] uses a flag,
/// and [`PreservedAnalyses::none`] stores an empty vector. Selective preservation
/// uses a compact type-id vector because pass declarations are normally tiny.
#[derive(Clone, Debug)]
pub struct PreservedAnalyses {
    preservation: Preservation,
}

#[derive(Clone, Debug)]
enum Preservation {
    All,
    Set(Vec<TypeId>),
}

impl PreservedAnalyses {
    /// Preserves every cached analysis.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            preservation: Preservation::All,
        }
    }

    /// Invalidates every stale cached analysis.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            preservation: Preservation::Set(Vec::new()),
        }
    }

    /// Adds one analysis type to the preserved set.
    pub fn preserve<A>(&mut self) -> &mut Self
    where
        A: Analysis,
    {
        let Preservation::Set(analyses) = &mut self.preservation else {
            return self;
        };
        let analysis = TypeId::of::<A>();
        if !analyses.contains(&analysis) {
            analyses.push(analysis);
        }
        self
    }

    /// Adds one analysis type and returns the updated declaration.
    #[must_use]
    pub fn preserving<A>(mut self) -> Self
    where
        A: Analysis,
    {
        self.preserve::<A>();
        self
    }

    /// Returns whether every cached analysis is preserved.
    #[must_use]
    pub const fn preserves_all(&self) -> bool {
        matches!(self.preservation, Preservation::All)
    }

    /// Returns whether one analysis type is preserved.
    #[must_use]
    pub fn preserves<A>(&self) -> bool
    where
        A: Analysis,
    {
        self.contains(TypeId::of::<A>())
    }

    pub(crate) fn contains(&self, analysis: TypeId) -> bool {
        match &self.preservation {
            Preservation::All => true,
            Preservation::Set(analyses) => analyses.contains(&analysis),
        }
    }
}

impl Default for PreservedAnalyses {
    fn default() -> Self {
        Self::none()
    }
}

struct CachedAnalysis {
    revision: u64,
    output: Arc<ErasedAnalysis>,
}

#[derive(Default)]
pub(crate) struct AnalysisCache {
    entries: HashMap<TypeId, CachedAnalysis>,
    active: Vec<(TypeId, &'static str)>,
    statistics: AnalysisStatistics,
}

impl AnalysisCache {
    fn get<A>(&mut self, module: &Module) -> Result<Arc<A::Output>, AnalysisError>
    where
        A: Analysis,
    {
        let analysis = TypeId::of::<A>();
        let revision = module.revision();
        let cached = self
            .entries
            .get(&analysis)
            .filter(|entry| entry.revision == revision)
            .map(|entry| Arc::clone(&entry.output));
        if let Some(cached) = cached {
            self.statistics.hits = self.statistics.hits.saturating_add(1);
            return Ok(cached
                .downcast::<A::Output>()
                .expect("analysis type id and cached output type disagree"));
        }

        if let Some(start) = self
            .active
            .iter()
            .position(|(active, _)| *active == analysis)
        {
            let mut cycle = self.active[start..]
                .iter()
                .map(|(_, name)| *name)
                .collect::<Vec<_>>();
            cycle.push(A::name());
            return Err(AnalysisError::cycle(A::name(), cycle));
        }

        self.statistics.misses = self.statistics.misses.saturating_add(1);
        self.active.push((analysis, A::name()));
        let computed = {
            let mut context = AnalysisContext { cache: self };
            A::analyze(module, &mut context)
        };
        let removed = self.active.pop();
        debug_assert_eq!(removed, Some((analysis, A::name())));
        let output = Arc::new(computed.map_err(|source| AnalysisError::failed::<A>(source))?);
        let erased: Arc<ErasedAnalysis> = output.clone();
        self.entries.insert(
            analysis,
            CachedAnalysis {
                revision,
                output: erased,
            },
        );
        Ok(output)
    }

    pub(crate) fn invalidate<A>(&mut self) -> bool
    where
        A: Analysis,
    {
        let removed = self.entries.remove(&TypeId::of::<A>()).is_some();
        if removed {
            self.statistics.invalidations = self.statistics.invalidations.saturating_add(1);
        }
        removed
    }

    pub(crate) fn clear(&mut self) -> u64 {
        let removed = u64::try_from(self.entries.len()).unwrap_or(u64::MAX);
        self.entries.clear();
        self.statistics.invalidations = self.statistics.invalidations.saturating_add(removed);
        removed
    }

    pub(crate) fn invalidate_after_change(
        &mut self,
        revision: u64,
        preserved: &PreservedAnalyses,
    ) -> u64 {
        let before = self.entries.len();
        self.entries.retain(|analysis, entry| {
            if entry.revision == revision {
                return true;
            }
            if preserved.contains(*analysis) {
                entry.revision = revision;
                true
            } else {
                false
            }
        });
        let invalidated = before.saturating_sub(self.entries.len());
        let invalidated = u64::try_from(invalidated).unwrap_or(u64::MAX);
        self.statistics.invalidations = self.statistics.invalidations.saturating_add(invalidated);
        invalidated
    }

    pub(crate) const fn statistics(&self) -> AnalysisStatistics {
        self.statistics
    }
}

/// Analysis lookup available while computing another analysis.
pub struct AnalysisContext<'cache> {
    cache: &'cache mut AnalysisCache,
}

impl AnalysisContext<'_> {
    /// Returns the cached or newly computed result for another analysis.
    ///
    /// # Errors
    ///
    /// Returns an error when the dependency fails or forms a dependency cycle.
    pub fn analysis<A>(&mut self, module: &Module) -> Result<Arc<A::Output>, AnalysisError>
    where
        A: Analysis,
    {
        self.cache.get::<A>(module)
    }
}

/// Per-run services exposed to one transformation pass.
pub struct PassContext<'run> {
    cache: &'run mut AnalysisCache,
}

impl<'run> PassContext<'run> {
    pub(crate) const fn new(cache: &'run mut AnalysisCache) -> Self {
        Self { cache }
    }

    /// Returns an `Arc` to the cached or newly computed analysis result.
    ///
    /// Holding the returned snapshot across an IR edit is memory-safe, but the
    /// pass must not treat it as describing the edited revision unless it knows
    /// the analysis is unaffected.
    ///
    /// # Errors
    ///
    /// Returns an error when analysis computation fails or dependencies cycle.
    pub fn analysis<A>(&mut self, module: &Module) -> Result<Arc<A::Output>, AnalysisError>
    where
        A: Analysis,
    {
        self.cache.get::<A>(module)
    }

    /// Explicitly removes one cached analysis.
    ///
    /// Revision tracking already handles semantic IR edits. This method is for
    /// analyses that intentionally depend on external state or non-semantic
    /// attachments changed by the running pass.
    pub fn invalidate<A>(&mut self) -> bool
    where
        A: Analysis,
    {
        self.cache.invalidate::<A>()
    }

    /// Removes every cached analysis and returns the number removed.
    pub fn invalidate_all(&mut self) -> u64 {
        self.cache.clear()
    }

    /// Returns cache statistics accumulated so far in this pipeline run.
    #[must_use]
    pub const fn analysis_statistics(&self) -> AnalysisStatistics {
        self.cache.statistics()
    }
}

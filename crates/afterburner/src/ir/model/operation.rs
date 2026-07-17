use std::collections::BTreeMap;

use super::{
    BlockId, FunctionRef, Literal, ProfileSiteId, RegionId, SourceSpan, TableRef, Type, ValueId,
    Volatility,
};

/// Deterministically encodable metadata attached to an operation.
///
/// Attributes are part of the operation's semantics and therefore participate
/// in structural fingerprints. Diagnostic provenance and runtime profile-site
/// identity are represented separately by [`OperationMetadata`]. Rust-native
/// analysis objects belong in [`crate::ir::Module::insert_attachment`], which is
/// intentionally non-semantic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attribute {
    /// Boolean attribute.
    Boolean(bool),
    /// Signed integer attribute.
    Integer(i128),
    /// Unsigned integer attribute.
    Unsigned(u128),
    /// UTF-8 string attribute.
    String(String),
    /// Opaque byte attribute.
    Bytes(Vec<u8>),
    /// IR type attribute.
    Type(Type),
    /// Ordered homogeneous or heterogeneous attribute sequence.
    Array(Vec<Attribute>),
    /// Deterministically ordered attribute dictionary.
    Dictionary(BTreeMap<String, Attribute>),
}

/// Conservative side-effect facts used to decide whether a transformation is legal.
///
/// An optimizer may only assume the effects represented here. Unknown calls or
/// extension operations should therefore declare every effect they may exhibit.
/// [`EffectSet::PURE`] is the empty set, so it can be combined with other facts
/// without special handling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectSet(u8);

impl EffectSet {
    /// No observable side effects and no evaluation-time failure.
    pub const PURE: Self = Self(0);
    /// Reads database state.
    pub const READS_DATABASE: Self = Self(1 << 0);
    /// Writes database state.
    pub const WRITES_DATABASE: Self = Self(1 << 1);
    /// May produce a different result on each evaluation.
    pub const VOLATILE: Self = Self(1 << 2);
    /// May terminate evaluation with a SQL or backend error.
    pub const MAY_ERROR: Self = Self(1 << 3);

    /// Creates an effect set, discarding currently unassigned bits.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits & 0b1111)
    }

    /// Returns the compact bit representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Combines two effect sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Reports whether every requested effect is present.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Reports whether no observable effect is present.
    #[must_use]
    pub const fn is_pure(self) -> bool {
        self.0 == 0
    }
}

/// SQL join semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JoinKind {
    /// Rows satisfying the condition on both sides.
    Inner,
    /// All left rows plus matching right rows.
    Left,
    /// All right rows plus matching left rows.
    Right,
    /// All rows from both sides with null extension.
    Full,
    /// Left rows for which a right match exists.
    Semi,
    /// Left rows for which no right match exists.
    Anti,
    /// Cartesian product without a condition.
    Cross,
}

/// SQL set operation applied to compatible relations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SetOperator {
    /// Union of input rows.
    Union,
    /// Rows present in every input.
    Intersect,
    /// Rows from the first input absent from later inputs.
    Except,
}

/// Ordering direction for one sort expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SortDirection {
    /// Lowest value first.
    Ascending,
    /// Highest value first.
    Descending,
}

/// Placement of SQL `NULL` values in a sort key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NullOrder {
    /// Null values precede non-null values.
    First,
    /// Null values follow non-null values.
    Last,
    /// Preserve the target database's default placement.
    DialectDefault,
}

/// Static ordering options aligned with one yielded sort expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SortKey {
    direction: SortDirection,
    null_order: NullOrder,
}

impl SortKey {
    /// Creates a sort-key descriptor.
    #[must_use]
    pub const fn new(direction: SortDirection, null_order: NullOrder) -> Self {
        Self {
            direction,
            null_order,
        }
    }

    /// Returns the value ordering direction.
    #[must_use]
    pub const fn direction(self) -> SortDirection {
        self.direction
    }

    /// Returns the null-placement policy.
    #[must_use]
    pub const fn null_order(self) -> NullOrder {
        self.null_order
    }
}

/// Unit used to measure a SQL window frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowFrameUnit {
    /// Counts physical rows relative to the current row.
    Rows,
    /// Uses the ordering value domain relative to the current row.
    Range,
    /// Counts peer groups defined by the window ordering.
    Groups,
}

/// One boundary of a SQL window frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WindowFrameBound {
    /// Starts at the first row or peer group in the partition.
    UnboundedPreceding,
    /// Uses the next frame-offset operand as a distance before the current row.
    Preceding,
    /// Uses the current row or its peer group as the boundary.
    CurrentRow,
    /// Uses the next frame-offset operand as a distance after the current row.
    Following,
    /// Ends at the last row or peer group in the partition.
    UnboundedFollowing,
}

/// Rows removed from a SQL window frame after its boundaries are applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WindowFrameExclusion {
    /// Retains every row selected by the frame boundaries.
    #[default]
    NoOthers,
    /// Excludes only the current row.
    CurrentRow,
    /// Excludes the current row and all of its ordering peers.
    Group,
    /// Excludes ordering peers while retaining the current row.
    Ties,
}

/// Complete SQL window-frame description.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowFrame {
    unit: WindowFrameUnit,
    start: WindowFrameBound,
    end: Option<WindowFrameBound>,
    exclusion: WindowFrameExclusion,
}

impl WindowFrame {
    /// Creates the single-bound SQL form with an implicit `CURRENT ROW` end.
    ///
    /// The exclusion defaults to [`WindowFrameExclusion::NoOthers`].
    #[must_use]
    pub const fn new(unit: WindowFrameUnit, start: WindowFrameBound) -> Self {
        Self {
            unit,
            start,
            end: None,
            exclusion: WindowFrameExclusion::NoOthers,
        }
    }

    /// Creates a frame with explicit `BETWEEN` boundaries.
    #[must_use]
    pub const fn between(
        unit: WindowFrameUnit,
        start: WindowFrameBound,
        end: WindowFrameBound,
    ) -> Self {
        Self {
            unit,
            start,
            end: Some(end),
            exclusion: WindowFrameExclusion::NoOthers,
        }
    }

    /// Sets the rows excluded after applying the frame boundaries.
    #[must_use]
    pub const fn with_exclusion(mut self, exclusion: WindowFrameExclusion) -> Self {
        self.exclusion = exclusion;
        self
    }

    /// Returns the frame measurement unit.
    #[must_use]
    pub const fn unit(self) -> WindowFrameUnit {
        self.unit
    }

    /// Returns the starting boundary.
    #[must_use]
    pub const fn start(self) -> WindowFrameBound {
        self.start
    }

    /// Returns the optional ending boundary.
    #[must_use]
    pub const fn end(self) -> Option<WindowFrameBound> {
        self.end
    }

    /// Returns the row-exclusion policy.
    #[must_use]
    pub const fn exclusion(self) -> WindowFrameExclusion {
        self.exclusion
    }

    /// Returns the number of trailing SSA operands consumed by frame offsets.
    ///
    /// When both bounds use offsets, the start-bound operand precedes the
    /// end-bound operand.
    #[must_use]
    pub const fn offset_count(self) -> usize {
        let start = match self.start {
            WindowFrameBound::Preceding | WindowFrameBound::Following => 1,
            WindowFrameBound::UnboundedPreceding
            | WindowFrameBound::CurrentRow
            | WindowFrameBound::UnboundedFollowing => 0,
        };
        let end = match self.end {
            Some(WindowFrameBound::Preceding | WindowFrameBound::Following) => 1,
            Some(
                WindowFrameBound::UnboundedPreceding
                | WindowFrameBound::CurrentRow
                | WindowFrameBound::UnboundedFollowing,
            )
            | None => 0,
        };
        start + end
    }
}

/// Operand layout and frame metadata for one window-function call.
///
/// Operation operands are ordered as function arguments, partition keys,
/// ordering expressions, and optional start/end frame-offset expressions. The
/// owning [`ScalarOp::WindowCall`] records the function-argument count. This
/// keeps every referenced expression in ordinary SSA def-use chains instead of
/// hiding [`ValueId`] handles inside attributes. Offset operands exist only for
/// [`WindowFrameBound::Preceding`] and [`WindowFrameBound::Following`], with the
/// start-bound offset before the end-bound offset.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct WindowSpec {
    partition_keys: u32,
    order_keys: Vec<SortKey>,
    frame: Option<WindowFrame>,
}

impl WindowSpec {
    /// Creates a specification without an explicit frame.
    #[must_use]
    pub fn new(partition_keys: u32, order_keys: Vec<SortKey>) -> Self {
        Self {
            partition_keys,
            order_keys,
            frame: None,
        }
    }

    /// Creates the global, unordered window represented by `OVER ()`.
    #[must_use]
    pub const fn global() -> Self {
        Self {
            partition_keys: 0,
            order_keys: Vec::new(),
            frame: None,
        }
    }

    /// Attaches an explicit frame.
    #[must_use]
    pub fn with_frame(mut self, frame: WindowFrame) -> Self {
        self.frame = Some(frame);
        self
    }

    /// Returns the number of partition-key operands.
    #[must_use]
    pub const fn partition_key_count(&self) -> u32 {
        self.partition_keys
    }

    /// Returns descriptors aligned with the ordering-expression operands.
    #[must_use]
    pub fn order_keys(&self) -> &[SortKey] {
        &self.order_keys
    }

    /// Returns the optional frame.
    #[must_use]
    pub const fn frame(&self) -> Option<&WindowFrame> {
        self.frame.as_ref()
    }
}

/// Built-in logical relational dialect.
///
/// Operands, results, nested regions, and CFG successors live in the generic
/// [`Operation`] shell. These variants store only operation-specific attributes.
/// Each variant documents the shell shape required by the verifier. Expression
/// regions contain one block whose arguments model an input row and whose final
/// [`TerminatorOp::Yield`] returns the computed expressions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogicalOp {
    /// Reads named columns and produces one relation without relation operands.
    Scan {
        /// Resolved source table.
        table: TableRef,
        /// Source column names aligned with the result schema.
        columns: Vec<String>,
    },
    /// Produces one finite literal relation without relation operands.
    Values {
        /// Literal rows aligned with the result schema.
        rows: Vec<Vec<Literal>>,
    },
    /// Produces one empty relation without relation operands.
    Empty,
    /// Retains rows whose predicate region yields one Boolean value.
    ///
    /// The single relation operand and result have the same type.
    Filter,
    /// Computes a new relation schema in one expression region.
    ///
    /// The operation consumes and produces one relation. Yielded value types
    /// match the result schema in field order.
    Project,
    /// Combines two relation operands into one relation result.
    ///
    /// Result fields follow positional SQL semantics. Inner and cross joins
    /// concatenate the left and right rows; outer joins additionally widen
    /// nullability on the null-extended side; semi and anti joins retain only
    /// the left row.
    Join {
        /// Join null-extension and membership semantics.
        kind: JoinKind,
        /// Whether the operation owns a Boolean condition region.
        has_condition: bool,
    },
    /// Computes grouping keys and aggregate values in one expression region.
    ///
    /// The region yields `group_keys` grouping expressions followed by values
    /// matching the result schema. Grouping expressions therefore remain
    /// explicit even when they are not returned by the aggregate relation.
    /// Output expressions that depend on input rows must use these exact SSA
    /// values as dependency roots; recomputing an equivalent expression does
    /// not establish grouping identity.
    Aggregate {
        /// Number of leading yielded values used as grouping expressions.
        group_keys: u32,
    },
    /// Computes window-function values without collapsing rows.
    ///
    /// The operation consumes and produces one relation through an expression
    /// region whose yielded value types match the result schema. Each
    /// [`ScalarOp::WindowCall`] owns an independent [`WindowSpec`], so one
    /// region may express multiple window definitions.
    Window,
    /// Orders one relation by expressions yielded from an optional key region.
    ///
    /// The input and result relation types are identical. An empty key list has
    /// no region; otherwise each key descriptor corresponds to one yielded value.
    Sort {
        /// Ordering policies aligned with yielded expressions.
        keys: Vec<SortKey>,
    },
    /// Restricts or offsets one relation while preserving its type.
    Limit {
        /// Number of rows skipped before emission.
        offset: Option<u64>,
        /// Maximum emitted row count.
        fetch: Option<u64>,
    },
    /// Removes duplicate rows while preserving the relation type.
    Distinct,
    /// Applies a set operation to at least two identically typed relations.
    ///
    /// The single result has that same relation type and the operation owns no
    /// nested regions.
    Set {
        /// Set operation semantics.
        operator: SetOperator,
        /// Whether duplicates are retained.
        all: bool,
    },
}

/// Single-operand scalar operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOperator {
    /// SQL logical negation.
    Not,
    /// Arithmetic sign negation.
    Negate,
    /// Tests whether the operand is null.
    IsNull,
    /// Tests whether the operand is not null.
    IsNotNull,
}

/// Two-operand scalar operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOperator {
    /// Numeric addition.
    Add,
    /// Numeric subtraction.
    Subtract,
    /// Numeric multiplication.
    Multiply,
    /// Numeric division.
    Divide,
    /// Numeric remainder.
    Modulo,
    /// SQL equality comparison.
    Equal,
    /// SQL inequality comparison.
    NotEqual,
    /// Strict less-than comparison.
    LessThan,
    /// Inclusive less-than comparison.
    LessThanOrEqual,
    /// Strict greater-than comparison.
    GreaterThan,
    /// Inclusive greater-than comparison.
    GreaterThanOrEqual,
    /// Three-valued logical conjunction.
    And,
    /// Three-valued logical disjunction.
    Or,
    /// String or binary concatenation.
    Concat,
    /// SQL pattern comparison.
    Like,
    /// Case-insensitive SQL pattern comparison.
    CaseInsensitiveLike,
    /// Null-safe distinctness comparison.
    IsDistinctFrom,
}

/// Scalar SSA dialect used inside relational expression regions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScalarOp {
    /// Embeds a compile-time SQL literal.
    Literal(Literal),
    /// References a bound statement parameter.
    Parameter {
        /// Zero-based bind position.
        position: u32,
        /// Optional frontend-level parameter name.
        name: Option<String>,
    },
    /// Applies one unary operator.
    Unary(UnaryOperator),
    /// Applies one binary operator.
    Binary(BinaryOperator),
    /// Calls a scalar function.
    Call {
        /// Resolved or frontend-retained function name.
        function: FunctionRef,
        /// Reordering and folding contract.
        volatility: Volatility,
        /// Effects not already implied by volatility.
        effects: EffectSet,
    },
    /// Converts one value to a target type.
    Cast {
        /// Required result type.
        to: Type,
    },
    /// Operands are `(condition, value)` pairs followed by one else value.
    Case {
        /// Number of condition/value operand pairs.
        arms: u32,
    },
    /// Calls an aggregate function in an aggregate region.
    AggregateCall {
        /// Aggregate function identity.
        function: FunctionRef,
        /// Whether duplicate arguments are discarded.
        distinct: bool,
        /// Reordering and folding contract.
        volatility: Volatility,
        /// Effects not already implied by volatility.
        effects: EffectSet,
    },
    /// Calls a window function in a window region.
    WindowCall {
        /// Window function identity.
        function: FunctionRef,
        /// Number of leading operands passed to the function itself.
        argument_count: u32,
        /// Partitioning, ordering, and frame semantics plus operand layout.
        window: WindowSpec,
        /// Reordering and folding contract.
        volatility: Volatility,
        /// Effects not already implied by volatility.
        effects: EffectSet,
    },
}

/// Built-in terminators that close AfterBurner regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TerminatorOp {
    /// Returns values from a nested expression region to its owning operation.
    Yield,
    /// Returns a relation from the module's root region.
    QueryReturn,
}

/// Escape hatch for downstream logical, physical, and runtime dialects.
///
/// Extension operations share the generic operand, result, region, successor,
/// effect, and verification infrastructure with built-in operations. Dialect-
/// specific invariants remain the responsibility of the downstream dialect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionOp {
    dialect: String,
    name: String,
    effects: EffectSet,
    terminator: bool,
}

impl ExtensionOp {
    /// Creates a pure, non-terminating extension operation descriptor.
    #[must_use]
    pub fn new(dialect: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            dialect: dialect.into(),
            name: name.into(),
            effects: EffectSet::PURE,
            terminator: false,
        }
    }

    /// Sets the conservative effect contract.
    #[must_use]
    pub const fn with_effects(mut self, effects: EffectSet) -> Self {
        self.effects = effects;
        self
    }

    /// Marks this extension as a basic-block terminator.
    #[must_use]
    pub const fn as_terminator(mut self) -> Self {
        self.terminator = true;
        self
    }

    /// Returns the dialect namespace.
    #[must_use]
    pub fn dialect(&self) -> &str {
        &self.dialect
    }

    /// Returns the operation name within the dialect.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the conservative effect contract.
    #[must_use]
    pub const fn effects(&self) -> EffectSet {
        self.effects
    }

    /// Reports whether this operation terminates its block.
    #[must_use]
    pub const fn is_terminator(&self) -> bool {
        self.terminator
    }
}

/// Dialect-specific semantic payload stored by a generic SSA operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OperationKind {
    /// Built-in logical relational operation.
    Logical(LogicalOp),
    /// Built-in scalar expression operation.
    Scalar(ScalarOp),
    /// Built-in region terminator.
    Terminator(TerminatorOp),
    /// Downstream dialect operation.
    Extension(ExtensionOp),
}

impl From<LogicalOp> for OperationKind {
    fn from(value: LogicalOp) -> Self {
        Self::Logical(value)
    }
}

impl From<ScalarOp> for OperationKind {
    fn from(value: ScalarOp) -> Self {
        Self::Scalar(value)
    }
}

impl From<TerminatorOp> for OperationKind {
    fn from(value: TerminatorOp) -> Self {
        Self::Terminator(value)
    }
}

impl From<ExtensionOp> for OperationKind {
    fn from(value: ExtensionOp) -> Self {
        Self::Extension(value)
    }
}

impl OperationKind {
    /// Reports whether the operation must be final in its block.
    #[must_use]
    pub const fn is_terminator(&self) -> bool {
        match self {
            Self::Terminator(_) => true,
            Self::Extension(extension) => extension.is_terminator(),
            Self::Logical(_) | Self::Scalar(_) => false,
        }
    }

    /// Returns conservative side-effect facts for transformation legality.
    #[must_use]
    pub const fn effects(&self) -> EffectSet {
        match self {
            Self::Logical(LogicalOp::Scan { .. }) => EffectSet::READS_DATABASE,
            Self::Scalar(ScalarOp::Call {
                volatility,
                effects,
                ..
            })
            | Self::Scalar(ScalarOp::AggregateCall {
                volatility,
                effects,
                ..
            })
            | Self::Scalar(ScalarOp::WindowCall {
                volatility,
                effects,
                ..
            }) => effects.union(match volatility {
                Volatility::Immutable => EffectSet::PURE,
                Volatility::Stable => EffectSet::READS_DATABASE,
                Volatility::Volatile => EffectSet::VOLATILE,
            }),
            Self::Scalar(ScalarOp::Cast { .. })
            | Self::Scalar(ScalarOp::Binary(BinaryOperator::Divide | BinaryOperator::Modulo)) => {
                EffectSet::MAY_ERROR
            }
            Self::Extension(extension) => extension.effects(),
            Self::Logical(_) | Self::Scalar(_) | Self::Terminator(_) => EffectSet::PURE,
        }
    }
}

/// Metadata that does not change an operation's SSA shape.
///
/// Semantic attributes participate in structural fingerprints. Source spans and
/// profile-site ids are operational metadata and are deliberately excluded.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OperationMetadata {
    pub(crate) source_span: Option<SourceSpan>,
    pub(crate) profile_site: Option<ProfileSiteId>,
    pub(crate) attributes: BTreeMap<String, Attribute>,
}

impl OperationMetadata {
    /// Returns optional source provenance.
    #[must_use]
    pub const fn source_span(&self) -> Option<&SourceSpan> {
        self.source_span.as_ref()
    }

    /// Returns the module-local runtime instrumentation site.
    #[must_use]
    pub const fn profile_site(&self) -> Option<ProfileSiteId> {
        self.profile_site
    }

    /// Returns deterministic semantic attributes.
    #[must_use]
    pub const fn attributes(&self) -> &BTreeMap<String, Attribute> {
        &self.attributes
    }
}

/// Operation stored in a module arena and exposed through a read-only view.
///
/// Structural changes go through [`crate::ir::IrEditor`] so block membership,
/// result definitions, and inverse use-lists remain synchronized.
#[derive(Clone, Debug)]
pub struct Operation {
    pub(crate) parent: BlockId,
    pub(crate) kind: OperationKind,
    pub(crate) operands: Vec<ValueId>,
    // Position of each operand's inverse edge in `Value::uses`. Keeping this
    // parallel to `operands` makes unlinking a use O(1).
    pub(crate) operand_use_indices: Vec<usize>,
    pub(crate) results: Vec<ValueId>,
    pub(crate) regions: Vec<RegionId>,
    pub(crate) successors: Vec<BlockId>,
    pub(crate) metadata: OperationMetadata,
}

impl Operation {
    /// Returns the containing block.
    #[must_use]
    pub const fn parent(&self) -> BlockId {
        self.parent
    }

    /// Returns the dialect-specific operation payload.
    #[must_use]
    pub const fn kind(&self) -> &OperationKind {
        &self.kind
    }

    /// Returns SSA operands in positional order.
    #[must_use]
    pub fn operands(&self) -> &[ValueId] {
        &self.operands
    }

    /// Returns SSA results in positional order.
    #[must_use]
    pub fn results(&self) -> &[ValueId] {
        &self.results
    }

    /// Returns nested regions in semantic order.
    #[must_use]
    pub fn regions(&self) -> &[RegionId] {
        &self.regions
    }

    /// Returns CFG successor blocks in semantic order.
    #[must_use]
    pub fn successors(&self) -> &[BlockId] {
        &self.successors
    }

    /// Returns attached metadata.
    #[must_use]
    pub const fn metadata(&self) -> &OperationMetadata {
        &self.metadata
    }

    /// Returns conservative side-effect facts.
    #[must_use]
    pub const fn effects(&self) -> EffectSet {
        self.kind.effects()
    }

    /// Reports whether this operation terminates its block.
    #[must_use]
    pub const fn is_terminator(&self) -> bool {
        self.kind.is_terminator()
    }
}

/// Detached operation description consumed by [`crate::ir::IrEditor`].
///
/// A specification describes only the operation shell. Insertion establishes
/// local arena and def-use consistency; [`crate::ir::verify_module`] validates
/// whole-module dominance, region shape, and dialect-specific constraints.
#[derive(Clone, Debug)]
pub struct OperationSpec {
    pub(crate) kind: OperationKind,
    pub(crate) operands: Vec<ValueId>,
    pub(crate) result_types: Vec<Type>,
    pub(crate) successors: Vec<BlockId>,
    pub(crate) metadata: OperationMetadata,
}

impl OperationSpec {
    /// Creates a detached operation with no operands, results, or successors.
    #[must_use]
    pub fn new(kind: impl Into<OperationKind>) -> Self {
        Self {
            kind: kind.into(),
            operands: Vec::new(),
            result_types: Vec::new(),
            successors: Vec::new(),
            metadata: OperationMetadata::default(),
        }
    }

    /// Sets positional SSA operands.
    #[must_use]
    pub fn with_operands(mut self, operands: impl Into<Vec<ValueId>>) -> Self {
        self.operands = operands.into();
        self
    }

    /// Appends one SSA result type.
    #[must_use]
    pub fn with_result(mut self, result: Type) -> Self {
        self.result_types.push(result);
        self
    }

    /// Replaces all SSA result types.
    #[must_use]
    pub fn with_results(mut self, results: impl Into<Vec<Type>>) -> Self {
        self.result_types = results.into();
        self
    }

    /// Sets CFG successor blocks.
    #[must_use]
    pub fn with_successors(mut self, successors: impl Into<Vec<BlockId>>) -> Self {
        self.successors = successors.into();
        self
    }

    /// Attaches diagnostic source provenance.
    #[must_use]
    pub fn with_source_span(mut self, source_span: SourceSpan) -> Self {
        self.metadata.source_span = Some(source_span);
        self
    }

    /// Attaches a stable runtime profile site.
    #[must_use]
    pub fn with_profile_site(mut self, profile_site: ProfileSiteId) -> Self {
        self.metadata.profile_site = Some(profile_site);
        self
    }

    /// Adds or replaces a semantic operation attribute.
    #[must_use]
    pub fn with_attribute(mut self, key: impl Into<String>, value: Attribute) -> Self {
        self.metadata.attributes.insert(key.into(), value);
        self
    }
}

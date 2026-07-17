//! Multi-level SSA query representation and its integrity tools.
//!
//! Relational operations produce typed relation values. Row expressions live
//! in nested regions whose block arguments represent input columns. This keeps
//! SQL scoping explicit while providing the def-use and dominance properties
//! expected by compiler optimization passes.
//!
//! # Core invariants
//!
//! - Every [`Value`] has exactly one [`ValueDefinition`].
//! - Each operation operand and inverse [`ValueUse`] point to one another, which
//!   permits constant-time unlinking during rewrites.
//! - Block order establishes local dominance; nested regions may capture only
//!   values visible at their owning operation.
//! - Every block ends in one terminator, and CFG successors remain within the
//!   same region.
//! - Logical expression-region arguments match input schemas, and yielded values
//!   match the owning operation's contract.
//!
//! [`IrEditor`] maintains local arena and def-use consistency. It intentionally
//! permits partially assembled IR, so callers must run [`verify_module`] before
//! optimization, fingerprinting, serialization, or code generation.

mod entity;
mod model;
mod mutation;
mod support;
mod validation;

pub use entity::{BlockId, OperationId, ProfileSiteId, RegionId, SchemaId, ValueId};
pub use model::{
    Attribute, BinaryOperator, Block, EffectSet, ExtensionOp, Field, FloatBits, FunctionRef,
    JoinKind, Literal, LogicalOp, Module, NullOrder, Operation, OperationKind, OperationMetadata,
    OperationSpec, Region, RegionParent, ScalarKind, ScalarOp, ScalarType, Schema, SetOperator,
    SortDirection, SortKey, SourceSpan, SqlType, TableRef, TerminatorOp, TimeZone, Type,
    UnaryOperator, Value, ValueDefinition, ValueUse, Volatility, WindowFrame, WindowFrameBound,
    WindowFrameExclusion, WindowFrameUnit, WindowSpec,
};
pub use mutation::{EditError, IrEditor};
pub use support::{
    AttachmentError, FingerprintError, StructuralFingerprint, WalkOrder, collect_operations,
    structural_fingerprint, walk_operations,
};
pub use validation::{VerificationError, VerificationLocation, verify_module};

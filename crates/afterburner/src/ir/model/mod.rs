//! Core SSA containers, SQL types, and built-in operation dialects.

mod module;
mod operation;
mod types;

use super::entity::{Arena, BlockId, OperationId, ProfileSiteId, RegionId, SchemaId, ValueId};

pub use module::{Block, Module, Region, RegionParent, Value, ValueDefinition, ValueUse};
pub use operation::{
    Attribute, BinaryOperator, EffectSet, ExtensionOp, JoinKind, LogicalOp, NullOrder, Operation,
    OperationKind, OperationMetadata, OperationSpec, ScalarOp, SetOperator, SortDirection, SortKey,
    TerminatorOp, UnaryOperator,
};
pub use types::{
    Field, FloatBits, FunctionRef, Literal, ScalarKind, ScalarType, Schema, SourceSpan, SqlType,
    TableRef, TimeZone, Type, Volatility,
};

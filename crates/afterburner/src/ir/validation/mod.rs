//! Whole-module structural, SSA, control-flow, and dialect verification.

mod verifier;

use super::{
    BinaryOperator, Block, BlockId, JoinKind, Literal, LogicalOp, Module, OperationId,
    OperationKind, ProfileSiteId, RegionId, RegionParent, ScalarOp, ScalarType, Schema, SchemaId,
    SqlType, TerminatorOp, Type, UnaryOperator, Value, ValueDefinition, ValueId, ValueUse,
    WindowFrame, WindowFrameBound, WindowFrameUnit, WindowSpec,
};

pub use verifier::{VerificationError, VerificationLocation, verify_module};

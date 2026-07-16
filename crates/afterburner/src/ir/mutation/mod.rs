//! Controlled IR construction and local SSA mutation.
//!
//! Edits preserve arena membership, definitions, and inverse use-lists. Global
//! properties such as dominance and dialect signatures remain the verifier's
//! responsibility so multi-step rewrites can be assembled incrementally.

mod editor;

use super::{
    Attribute, Block, BlockId, Module, Operation, OperationId, OperationSpec, ProfileSiteId,
    Region, RegionId, RegionParent, Schema, SchemaId, SourceSpan, Type, Value, ValueDefinition,
    ValueId, ValueUse,
};

pub use editor::{EditError, IrEditor};

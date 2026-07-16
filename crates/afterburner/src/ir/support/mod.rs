//! Transient native attachments, structural fingerprinting, and IR traversal.

mod attachments;
mod fingerprint;
mod walk;

use super::{
    Attribute, BinaryOperator, BlockId, ExtensionOp, FunctionRef, JoinKind, Literal, LogicalOp,
    Module, NullOrder, OperationId, OperationKind, RegionId, ScalarOp, SchemaId, SetOperator,
    SortDirection, SortKey, SqlType, TerminatorOp, TimeZone, Type, UnaryOperator, ValueId,
    VerificationError, Volatility, verify_module,
};

pub use attachments::AttachmentError;
pub(crate) use attachments::OperationAttachments;
pub use fingerprint::{FingerprintError, StructuralFingerprint, structural_fingerprint};
pub use walk::{WalkOrder, collect_operations, walk_operations};

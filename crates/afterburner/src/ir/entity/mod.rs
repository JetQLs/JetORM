//! Strongly typed handles backed by module-local generational arenas.
//!
//! Handles are cheap to copy but meaningful only in their originating module.
//! Reusing an erased slot increments its generation, causing stale lookups to
//! fail instead of silently targeting a different entity.

mod arena;
mod id;

pub(crate) use arena::Arena;
pub use id::{BlockId, OperationId, ProfileSiteId, RegionId, SchemaId, ValueId};

use std::{
    any::{Any, TypeId},
    collections::HashMap,
    error::Error,
    fmt,
    sync::Arc,
};

use super::OperationId;

type ErasedAttachment = dyn Any + Send + Sync;

/// Error returned when accessing transient data for an unknown operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentError {
    /// The operation handle is stale or belongs to another module.
    UnknownOperation(OperationId),
}

impl fmt::Display for AttachmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOperation(operation) => write!(formatter, "unknown operation {operation}"),
        }
    }
}

impl Error for AttachmentError {}

/// Type-erased, non-semantic data indexed by operation and Rust type.
///
/// Values use `Arc` so cloning a module can preserve native attachments without
/// requiring an object-safe clone trait. The public module API validates handles
/// before delegating to this store.
#[derive(Clone, Default)]
pub(crate) struct OperationAttachments {
    entries: HashMap<OperationId, HashMap<TypeId, Arc<ErasedAttachment>>>,
}

impl OperationAttachments {
    pub(crate) fn get<T>(&self, operation: OperationId) -> Option<&T>
    where
        T: Any + Send + Sync,
    {
        self.entries
            .get(&operation)?
            .get(&TypeId::of::<T>())?
            .as_ref()
            .downcast_ref::<T>()
    }

    pub(crate) fn insert<T>(&mut self, operation: OperationId, attachment: T) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        let previous = self
            .entries
            .entry(operation)
            .or_default()
            .insert(TypeId::of::<T>(), Arc::new(attachment));
        previous.map(|value| match value.downcast::<T>() {
            Ok(value) => value,
            Err(_) => unreachable!("attachment TypeId and stored value type disagree"),
        })
    }

    pub(crate) fn remove<T>(&mut self, operation: OperationId) -> Option<Arc<T>>
    where
        T: Any + Send + Sync,
    {
        let (removed, empty) = {
            let attachments = self.entries.get_mut(&operation)?;
            let removed = attachments.remove(&TypeId::of::<T>());
            (removed, attachments.is_empty())
        };
        if empty {
            self.entries.remove(&operation);
        }
        removed.map(|value| match value.downcast::<T>() {
            Ok(value) => value,
            Err(_) => unreachable!("attachment TypeId and stored value type disagree"),
        })
    }

    pub(crate) fn clear_operation(&mut self, operation: OperationId) -> usize {
        self.entries
            .remove(&operation)
            .map_or(0, |attachments| attachments.len())
    }

    fn attachment_count(&self) -> usize {
        self.entries.values().map(HashMap::len).sum()
    }
}

impl fmt::Debug for OperationAttachments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationAttachments")
            .field("operations", &self.entries.len())
            .field("attachments", &self.attachment_count())
            .finish_non_exhaustive()
    }
}

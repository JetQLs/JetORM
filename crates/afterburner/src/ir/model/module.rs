use std::{any::Any, sync::Arc};

use crate::ir::support::{AttachmentError, OperationAttachments};

use super::{Arena, BlockId, Operation, OperationId, RegionId, Schema, SchemaId, Type, ValueId};

/// Structural owner of a region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegionParent {
    /// The module's single top-level region.
    Module,
    /// An operation-owned nested region.
    Operation(OperationId),
}

/// Ordered block container owned by the module or an operation.
#[derive(Clone, Debug)]
pub struct Region {
    pub(crate) parent: RegionParent,
    pub(crate) blocks: Vec<BlockId>,
}

impl Region {
    /// Returns the structural owner of this region.
    #[must_use]
    pub const fn parent(&self) -> RegionParent {
        self.parent
    }

    /// Returns blocks in stored region order.
    ///
    /// Control-flow successors, rather than this slice alone, determine runtime
    /// execution order when a region contains multiple blocks.
    #[must_use]
    pub fn blocks(&self) -> &[BlockId] {
        &self.blocks
    }
}

/// Basic block containing SSA arguments and program-ordered operations.
#[derive(Clone, Debug)]
pub struct Block {
    pub(crate) parent: RegionId,
    pub(crate) arguments: Vec<ValueId>,
    pub(crate) operations: Vec<OperationId>,
}

impl Block {
    /// Returns the region containing this block.
    #[must_use]
    pub const fn parent(&self) -> RegionId {
        self.parent
    }

    /// Returns SSA block arguments in positional order.
    #[must_use]
    pub fn arguments(&self) -> &[ValueId] {
        &self.arguments
    }

    /// Returns operations in program order.
    ///
    /// Within the block, an operation result dominates only later operations.
    #[must_use]
    pub fn operations(&self) -> &[OperationId] {
        &self.operations
    }
}

/// Unique defining site of an SSA value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueDefinition {
    /// Value introduced as a basic-block argument.
    BlockArgument {
        /// Defining block.
        block: BlockId,
        /// Zero-based argument position.
        index: u32,
    },
    /// Value produced by an operation result.
    OperationResult {
        /// Defining operation.
        operation: OperationId,
        /// Zero-based result position.
        index: u32,
    },
}

/// Inverse edge from a value to one consuming operation operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ValueUse {
    user: OperationId,
    operand_index: u32,
}

impl ValueUse {
    /// Creates an inverse use-list entry.
    #[must_use]
    pub const fn new(user: OperationId, operand_index: u32) -> Self {
        Self {
            user,
            operand_index,
        }
    }

    /// Returns the consuming operation.
    #[must_use]
    pub const fn user(self) -> OperationId {
        self.user
    }

    /// Returns the zero-based operand position in the consuming operation.
    #[must_use]
    pub const fn operand_index(self) -> u32 {
        self.operand_index
    }
}

/// Typed SSA value with one definition and an inverse use-list.
#[derive(Clone, Debug)]
pub struct Value {
    pub(crate) ty: Type,
    pub(crate) definition: ValueDefinition,
    pub(crate) uses: Vec<ValueUse>,
}

impl Value {
    /// Returns the statically known value type.
    #[must_use]
    pub const fn ty(&self) -> &Type {
        &self.ty
    }

    /// Returns the unique SSA definition.
    #[must_use]
    pub const fn definition(&self) -> ValueDefinition {
        self.definition
    }

    /// Returns every operand position that consumes this value.
    ///
    /// Use-list order is not semantic and may change after an O(1) unlink.
    #[must_use]
    pub fn uses(&self) -> &[ValueUse] {
        &self.uses
    }
}

/// Ownership boundary for one complete AfterBurner IR unit.
///
/// A module owns every schema, region, block, operation, value, and transient
/// native attachment referenced by its typed handles. Handles are generational:
/// erasing and reusing an arena slot invalidates the old handle instead of
/// redirecting it to a new entity.
///
/// Construction is incremental, so a [`Module`] is not necessarily valid until
/// it passes [`crate::ir::verify_module`].
#[derive(Clone, Debug)]
pub struct Module {
    pub(crate) schemas: Arena<SchemaId, Schema>,
    pub(crate) regions: Arena<RegionId, Region>,
    pub(crate) blocks: Arena<BlockId, Block>,
    pub(crate) operations: Arena<OperationId, Operation>,
    pub(crate) values: Arena<ValueId, Value>,
    pub(crate) attachments: OperationAttachments,
    root_region: RegionId,
    root_block: BlockId,
    pub(crate) revision: u64,
}

impl Module {
    /// Creates a module with an empty root region and entry block.
    ///
    /// The entry block has no terminator, so the new module does not pass
    /// verification until the caller completes it.
    #[must_use]
    pub fn new() -> Self {
        let mut regions = Arena::new();
        let mut blocks = Arena::new();
        let root_region = regions.insert(Region {
            parent: RegionParent::Module,
            blocks: Vec::new(),
        });
        let root_block = blocks.insert(Block {
            parent: root_region,
            arguments: Vec::new(),
            operations: Vec::new(),
        });
        regions
            .get_mut(root_region)
            .expect("newly inserted root region exists")
            .blocks
            .push(root_block);

        Self {
            schemas: Arena::new(),
            regions,
            blocks,
            operations: Arena::new(),
            values: Arena::new(),
            attachments: OperationAttachments::default(),
            root_region,
            root_block,
            revision: 0,
        }
    }

    /// Returns the module-owned top-level region.
    #[must_use]
    pub const fn root_region(&self) -> RegionId {
        self.root_region
    }

    /// Returns the entry block of the top-level region.
    #[must_use]
    pub const fn root_block(&self) -> BlockId {
        self.root_block
    }

    /// Returns the monotonic edit revision used to invalidate cached analyses.
    ///
    /// The counter may wrap; consumers must compare revisions for equality rather
    /// than infer ordering or elapsed edit counts.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns a transient native attachment by its concrete Rust type.
    ///
    /// Attachments are excluded from query semantics, structural fingerprints,
    /// verification, and the module revision. A cloned module shares attachment
    /// instances through [`Arc`]. Use an interior-mutable attachment type when
    /// multiple owners must update the same native object.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::UnknownOperation`] for a stale operation.
    pub fn attachment<T>(&self, operation: OperationId) -> Result<Option<&T>, AttachmentError>
    where
        T: Any + Send + Sync,
    {
        if self.operations.get(operation).is_none() {
            return Err(AttachmentError::UnknownOperation(operation));
        }
        Ok(self.attachments.get::<T>(operation))
    }

    /// Adds or replaces a transient native attachment for an operation.
    ///
    /// One attachment of each concrete Rust type may be stored per operation.
    /// Define lightweight newtypes when one underlying type serves multiple
    /// analysis roles. The previous value is returned when it is replaced.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::UnknownOperation`] for a stale operation.
    pub fn insert_attachment<T>(
        &mut self,
        operation: OperationId,
        attachment: T,
    ) -> Result<Option<Arc<T>>, AttachmentError>
    where
        T: Any + Send + Sync,
    {
        if self.operations.get(operation).is_none() {
            return Err(AttachmentError::UnknownOperation(operation));
        }
        Ok(self.attachments.insert(operation, attachment))
    }

    /// Removes one transient attachment selected by concrete Rust type.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::UnknownOperation`] for a stale operation.
    pub fn remove_attachment<T>(
        &mut self,
        operation: OperationId,
    ) -> Result<Option<Arc<T>>, AttachmentError>
    where
        T: Any + Send + Sync,
    {
        if self.operations.get(operation).is_none() {
            return Err(AttachmentError::UnknownOperation(operation));
        }
        Ok(self.attachments.remove::<T>(operation))
    }

    /// Removes every transient native attachment for one operation.
    ///
    /// Returns the number of removed attachments.
    ///
    /// # Errors
    ///
    /// Returns [`AttachmentError::UnknownOperation`] for a stale operation.
    pub fn clear_attachments(&mut self, operation: OperationId) -> Result<usize, AttachmentError> {
        if self.operations.get(operation).is_none() {
            return Err(AttachmentError::UnknownOperation(operation));
        }
        Ok(self.attachments.clear_operation(operation))
    }

    pub(crate) fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// Looks up an interned schema and rejects stale generations.
    #[must_use]
    pub fn schema(&self, id: SchemaId) -> Option<&Schema> {
        self.schemas.get(id)
    }

    /// Looks up a region and rejects stale generations.
    #[must_use]
    pub fn region(&self, id: RegionId) -> Option<&Region> {
        self.regions.get(id)
    }

    /// Looks up a block and rejects stale generations.
    #[must_use]
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.blocks.get(id)
    }

    /// Looks up an operation and rejects stale generations.
    #[must_use]
    pub fn operation(&self, id: OperationId) -> Option<&Operation> {
        self.operations.get(id)
    }

    /// Looks up an SSA value and rejects stale generations.
    #[must_use]
    pub fn value(&self, id: ValueId) -> Option<&Value> {
        self.values.get(id)
    }

    /// Iterates live schema handles in storage order.
    pub fn schema_ids(&self) -> impl Iterator<Item = SchemaId> + '_ {
        self.schemas.iter().map(|(id, _)| id)
    }

    /// Iterates live region handles in storage order.
    pub fn region_ids(&self) -> impl Iterator<Item = RegionId> + '_ {
        self.regions.iter().map(|(id, _)| id)
    }

    /// Iterates live block handles in storage order.
    pub fn block_ids(&self) -> impl Iterator<Item = BlockId> + '_ {
        self.blocks.iter().map(|(id, _)| id)
    }

    /// Iterates live operation handles in storage order.
    pub fn operation_ids(&self) -> impl Iterator<Item = OperationId> + '_ {
        self.operations.iter().map(|(id, _)| id)
    }

    /// Iterates live SSA value handles in storage order.
    pub fn value_ids(&self) -> impl Iterator<Item = ValueId> + '_ {
        self.values.iter().map(|(id, _)| id)
    }

    /// Borrows the module's controlled construction and mutation interface.
    #[must_use]
    pub fn editor(&mut self) -> crate::ir::IrEditor<'_> {
        crate::ir::IrEditor::new(self)
    }
}

impl Default for Module {
    fn default() -> Self {
        Self::new()
    }
}

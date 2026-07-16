use std::{collections::HashSet, error::Error, fmt};

use super::{
    Attribute, Block, BlockId, Module, Operation, OperationId, OperationSpec, ProfileSiteId,
    Region, RegionId, RegionParent, Schema, SchemaId, SourceSpan, Type, Value, ValueDefinition,
    ValueId, ValueUse,
};

/// Error returned when an edit cannot preserve core arena or SSA invariants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditError {
    /// A block handle is stale or foreign to the module.
    UnknownBlock(BlockId),
    /// An operation handle is stale or foreign to the module.
    UnknownOperation(OperationId),
    /// A region handle is stale or foreign to the module.
    UnknownRegion(RegionId),
    /// A value handle is stale or foreign to the module.
    UnknownValue(ValueId),
    /// A CFG successor handle is stale or foreign to the module.
    UnknownSuccessor(BlockId),
    /// An operand position does not exist.
    OperandOutOfBounds {
        /// Operation being edited.
        operation: OperationId,
        /// Requested zero-based operand position.
        index: usize,
    },
    /// A result position does not exist.
    ResultOutOfBounds {
        /// Operation being queried.
        operation: OperationId,
        /// Requested zero-based result position.
        index: usize,
    },
    /// A block-argument position does not exist.
    BlockArgumentOutOfBounds {
        /// Block being queried.
        block: BlockId,
        /// Requested zero-based argument position.
        index: usize,
    },
    /// A replacement would violate exact SSA type equality.
    TypeMismatch {
        /// Type required by the existing use.
        expected: Type,
        /// Type supplied by the replacement value.
        actual: Type,
    },
    /// A value defined by an erase subtree still has an external consumer.
    ValueStillUsed(ValueId),
    /// Stored inverse use data disagrees with operation operands.
    CorruptUseList(ValueId),
    /// An operation-owned region tree has inconsistent ownership or membership.
    CorruptOperationTree(OperationId),
}

impl fmt::Display for EditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBlock(block) => write!(formatter, "unknown block {block}"),
            Self::UnknownOperation(operation) => {
                write!(formatter, "unknown operation {operation}")
            }
            Self::UnknownRegion(region) => write!(formatter, "unknown region {region}"),
            Self::UnknownValue(value) => write!(formatter, "unknown value {value}"),
            Self::UnknownSuccessor(block) => write!(formatter, "unknown successor {block}"),
            Self::OperandOutOfBounds { operation, index } => {
                write!(formatter, "operand {index} is outside {operation}")
            }
            Self::ResultOutOfBounds { operation, index } => {
                write!(formatter, "result {index} is outside {operation}")
            }
            Self::BlockArgumentOutOfBounds { block, index } => {
                write!(formatter, "argument {index} is outside {block}")
            }
            Self::TypeMismatch { expected, actual } => {
                write!(
                    formatter,
                    "SSA replacement type mismatch: expected {expected:?}, got {actual:?}"
                )
            }
            Self::ValueStillUsed(value) => write!(formatter, "cannot erase used value {value}"),
            Self::CorruptUseList(value) => write!(formatter, "inconsistent use-list for {value}"),
            Self::CorruptOperationTree(operation) => {
                write!(formatter, "inconsistent ownership tree below {operation}")
            }
        }
    }
}

impl Error for EditError {}

/// Exclusive mutation surface for module structure and SSA edges.
///
/// Edits keep arena membership, result definitions, and inverse use-lists in
/// local agreement. They intentionally permit intermediate IR that is incomplete
/// or violates global constraints such as dominance and dialect region shape.
/// Run [`crate::ir::verify_module`] after a rewrite transaction and before the IR
/// crosses an optimizer, fingerprint, or lowering boundary.
pub struct IrEditor<'module> {
    module: &'module mut Module,
}

struct ErasePlan {
    root_parent: BlockId,
    root_position: usize,
    /// Owned operations in post-order, with the requested root last.
    operations: Vec<OperationId>,
    regions: Vec<RegionId>,
    blocks: Vec<BlockId>,
    values: Vec<ValueId>,
    operation_set: HashSet<OperationId>,
}

impl<'module> IrEditor<'module> {
    pub(crate) const fn new(module: &'module mut Module) -> Self {
        Self { module }
    }

    /// Returns a read-only view of the module under construction.
    #[must_use]
    pub const fn module(&self) -> &Module {
        self.module
    }

    /// Reuses an equal row schema or inserts a new schema arena entry.
    #[must_use]
    pub fn intern_schema(&mut self, schema: Schema) -> SchemaId {
        if let Some((id, _)) = self
            .module
            .schemas
            .iter()
            .find(|(_, existing)| *existing == &schema)
        {
            return id;
        }
        let id = self.module.schemas.insert(schema);
        self.module.bump_revision();
        id
    }

    /// Appends an operation and creates its results and inverse uses atomically.
    ///
    /// This method validates referenced handles, but it does not require the new
    /// operation to satisfy dominance, termination, or dialect-specific shape
    /// rules. That allows clients to assemble a valid rewrite incrementally.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent, an operand, or a successor is stale.
    pub fn append_operation(
        &mut self,
        block: BlockId,
        spec: OperationSpec,
    ) -> Result<OperationId, EditError> {
        if self.module.blocks.get(block).is_none() {
            return Err(EditError::UnknownBlock(block));
        }
        for operand in &spec.operands {
            if self.module.values.get(*operand).is_none() {
                return Err(EditError::UnknownValue(*operand));
            }
        }
        for successor in &spec.successors {
            if self.module.blocks.get(*successor).is_none() {
                return Err(EditError::UnknownSuccessor(*successor));
            }
        }

        let operation = self.module.operations.insert(Operation {
            parent: block,
            kind: spec.kind,
            operands: spec.operands,
            operand_use_indices: Vec::new(),
            results: Vec::new(),
            regions: Vec::new(),
            successors: spec.successors,
            metadata: spec.metadata,
        });

        let result_types = spec.result_types;
        let mut results = Vec::with_capacity(result_types.len());
        for (index, ty) in result_types.into_iter().enumerate() {
            let value = self.module.values.insert(Value {
                ty,
                definition: ValueDefinition::OperationResult {
                    operation,
                    index: index as u32,
                },
                uses: Vec::new(),
            });
            results.push(value);
        }
        self.module
            .operations
            .get_mut(operation)
            .expect("newly inserted operation exists")
            .results = results;

        let operands = self
            .module
            .operations
            .get(operation)
            .expect("newly inserted operation exists")
            .operands
            .clone();
        let mut operand_use_indices = Vec::with_capacity(operands.len());
        for (operand_index, operand) in operands.into_iter().enumerate() {
            let value = self
                .module
                .values
                .get_mut(operand)
                .expect("operands were validated");
            operand_use_indices.push(value.uses.len());
            value
                .uses
                .push(ValueUse::new(operation, operand_index as u32));
        }
        self.module
            .operations
            .get_mut(operation)
            .expect("newly inserted operation exists")
            .operand_use_indices = operand_use_indices;
        self.module
            .blocks
            .get_mut(block)
            .expect("parent block was validated")
            .operations
            .push(operation);
        self.module.bump_revision();
        Ok(operation)
    }

    /// Inserts an operation immediately before an existing operation.
    ///
    /// The inserted operation is created through the same def-use maintenance
    /// path as [`Self::append_operation`]. The caller remains responsible for
    /// choosing operands that dominate the new position; whole-module verification
    /// checks that requirement after the rewrite.
    ///
    /// # Errors
    ///
    /// Returns an error when the anchor or any referenced operand is stale.
    pub fn insert_operation_before(
        &mut self,
        anchor: OperationId,
        spec: OperationSpec,
    ) -> Result<OperationId, EditError> {
        let block_id = self
            .module
            .operations
            .get(anchor)
            .ok_or(EditError::UnknownOperation(anchor))?
            .parent;
        let anchor_position = self
            .module
            .blocks
            .get(block_id)
            .ok_or(EditError::UnknownBlock(block_id))?
            .operations
            .iter()
            .position(|candidate| *candidate == anchor)
            .ok_or(EditError::UnknownOperation(anchor))?;
        // Reuse append_operation as the single def-use construction path, then
        // move only the block-order entry. SSA handles and use-lists are position
        // independent, so no other structure needs to be rewritten.
        let inserted = self.append_operation(block_id, spec)?;
        let block = self
            .module
            .blocks
            .get_mut(block_id)
            .expect("append_operation validated the block");
        let inserted_position = block
            .operations
            .iter()
            .position(|candidate| *candidate == inserted)
            .expect("append_operation attached the new operation");
        block.operations.remove(inserted_position);
        block.operations.insert(anchor_position, inserted);
        self.module.bump_revision();
        Ok(inserted)
    }

    /// Adds a nested region to an existing operation.
    ///
    /// The region is incomplete until at least one terminated block is appended
    /// and will fail whole-module verification in the meantime.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::UnknownOperation`] for a stale owner.
    pub fn add_region(&mut self, owner: OperationId) -> Result<RegionId, EditError> {
        if self.module.operations.get(owner).is_none() {
            return Err(EditError::UnknownOperation(owner));
        }
        let region = self.module.regions.insert(Region {
            parent: RegionParent::Operation(owner),
            blocks: Vec::new(),
        });
        self.module
            .operations
            .get_mut(owner)
            .expect("owner was validated")
            .regions
            .push(region);
        self.module.bump_revision();
        Ok(region)
    }

    /// Appends a block and creates typed SSA block arguments.
    ///
    /// The block is intentionally incomplete until a terminator is appended.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::UnknownRegion`] for a stale parent.
    pub fn append_block(
        &mut self,
        region: RegionId,
        argument_types: impl Into<Vec<Type>>,
    ) -> Result<BlockId, EditError> {
        if self.module.regions.get(region).is_none() {
            return Err(EditError::UnknownRegion(region));
        }
        let block = self.module.blocks.insert(Block {
            parent: region,
            arguments: Vec::new(),
            operations: Vec::new(),
        });
        let mut arguments = Vec::new();
        for (index, ty) in argument_types.into().into_iter().enumerate() {
            arguments.push(self.module.values.insert(Value {
                ty,
                definition: ValueDefinition::BlockArgument {
                    block,
                    index: index as u32,
                },
                uses: Vec::new(),
            }));
        }
        self.module
            .blocks
            .get_mut(block)
            .expect("newly inserted block exists")
            .arguments = arguments;
        self.module
            .regions
            .get_mut(region)
            .expect("parent region was validated")
            .blocks
            .push(block);
        self.module.bump_revision();
        Ok(block)
    }

    /// Replaces one operand while updating both involved value use-lists.
    ///
    /// The replacement must have exactly the same type. This local edit does not
    /// prove that the replacement dominates the user; verify the enclosing rewrite
    /// before consuming the module.
    ///
    /// # Errors
    ///
    /// Returns an error for stale handles, an invalid position, mismatched
    /// types, or a previously corrupted inverse use-list.
    pub fn replace_operand(
        &mut self,
        user: OperationId,
        operand_index: usize,
        replacement: ValueId,
    ) -> Result<(), EditError> {
        let replacement_type = self
            .module
            .values
            .get(replacement)
            .ok_or(EditError::UnknownValue(replacement))?
            .ty
            .clone();
        let old = *self
            .module
            .operations
            .get(user)
            .ok_or(EditError::UnknownOperation(user))?
            .operands
            .get(operand_index)
            .ok_or(EditError::OperandOutOfBounds {
                operation: user,
                index: operand_index,
            })?;
        if old == replacement {
            return Ok(());
        }
        let old_type = self
            .module
            .values
            .get(old)
            .ok_or(EditError::UnknownValue(old))?
            .ty
            .clone();
        if old_type != replacement_type {
            return Err(EditError::TypeMismatch {
                expected: old_type,
                actual: replacement_type,
            });
        }

        self.unlink_operand_use(user, operand_index)?;
        let replacement_use_index = {
            let replacement_value = self
                .module
                .values
                .get_mut(replacement)
                .expect("replacement was validated");
            let use_index = replacement_value.uses.len();
            replacement_value
                .uses
                .push(ValueUse::new(user, operand_index as u32));
            use_index
        };
        let user_operation = self
            .module
            .operations
            .get_mut(user)
            .expect("user was validated");
        user_operation.operands[operand_index] = replacement;
        user_operation.operand_use_indices[operand_index] = replacement_use_index;
        self.module.bump_revision();
        Ok(())
    }

    /// Removes one inverse operand edge in constant time.
    ///
    /// `swap_remove` may move the last inverse edge into the removed slot. Its
    /// owning operand backlink is repaired before this method returns.
    fn unlink_operand_use(
        &mut self,
        user: OperationId,
        operand_index: usize,
    ) -> Result<ValueId, EditError> {
        let operation = self
            .module
            .operations
            .get(user)
            .ok_or(EditError::UnknownOperation(user))?;
        let operand =
            *operation
                .operands
                .get(operand_index)
                .ok_or(EditError::OperandOutOfBounds {
                    operation: user,
                    index: operand_index,
                })?;
        let use_index = operation
            .operand_use_indices
            .get(operand_index)
            .copied()
            .ok_or(EditError::CorruptUseList(operand))?;
        let expected = ValueUse::new(user, operand_index as u32);
        let (moved_use, last_use_index) = {
            let value = self
                .module
                .values
                .get(operand)
                .ok_or(EditError::UnknownValue(operand))?;
            if value.uses.get(use_index) != Some(&expected) {
                return Err(EditError::CorruptUseList(operand));
            }
            let last_use_index = value.uses.len() - 1;
            (
                (use_index != last_use_index).then_some(value.uses[last_use_index]),
                last_use_index,
            )
        };

        // Validate the backlink that will be repaired before mutating either
        // side, preserving the editor's all-or-nothing error behavior.
        if let Some(moved) = moved_use {
            let moved_operand_index = moved.operand_index() as usize;
            let moved_user = self
                .module
                .operations
                .get(moved.user())
                .ok_or(EditError::CorruptUseList(operand))?;
            if moved_user.operands.get(moved_operand_index) != Some(&operand)
                || moved_user
                    .operand_use_indices
                    .get(moved_operand_index)
                    .copied()
                    != Some(last_use_index)
            {
                return Err(EditError::CorruptUseList(operand));
            }
        }

        self.module
            .values
            .get_mut(operand)
            .expect("operand and backlink were validated")
            .uses
            .swap_remove(use_index);
        if let Some(moved) = moved_use {
            self.module
                .operations
                .get_mut(moved.user())
                .expect("moved use backlink was validated")
                .operand_use_indices[moved.operand_index() as usize] = use_index;
        }
        Ok(operand)
    }

    /// Replaces all uses of one value with another value of exactly the same type.
    ///
    /// Coercions require an explicit cast operation. Replacement preserves inverse
    /// use-lists, but it may change dominance relationships; callers must verify the
    /// completed rewrite.
    ///
    /// # Errors
    ///
    /// Returns an error for stale handles, unequal types, or corrupt use-lists.
    pub fn replace_all_uses(
        &mut self,
        from: ValueId,
        replacement: ValueId,
    ) -> Result<(), EditError> {
        let from_value = self
            .module
            .values
            .get(from)
            .ok_or(EditError::UnknownValue(from))?;
        let replacement_value = self
            .module
            .values
            .get(replacement)
            .ok_or(EditError::UnknownValue(replacement))?;
        if from_value.ty != replacement_value.ty {
            return Err(EditError::TypeMismatch {
                expected: from_value.ty.clone(),
                actual: replacement_value.ty.clone(),
            });
        }
        let uses = from_value.uses.clone();
        for value_use in uses {
            self.replace_operand(
                value_use.user(),
                value_use.operand_index() as usize,
                replacement,
            )?;
        }
        Ok(())
    }

    /// Erases an operation and every region it transitively owns.
    ///
    /// The complete ownership subtree is validated before mutation begins. Values
    /// defined by the subtree must not have users outside it; internal uses and
    /// captured operands are detached automatically. This preflight makes failure
    /// atomic and lets DCE erase a region-owning operation with one call.
    ///
    /// # Errors
    ///
    /// Returns an error if the operation is stale, a value escapes the erased
    /// subtree, or stored ownership and def-use data is inconsistent.
    pub fn erase_operation(&mut self, operation: OperationId) -> Result<(), EditError> {
        let plan = self.plan_operation_erase(operation)?;

        // Every possible error has been checked. Unlinking may reorder inverse
        // uses, but each swap repairs the moved operand's backlink immediately.
        for owned_operation in &plan.operations {
            let operand_count = self
                .module
                .operations
                .get(*owned_operation)
                .expect("erase plan keeps operations live")
                .operands
                .len();
            for operand_index in 0..operand_count {
                self.unlink_operand_use(*owned_operation, operand_index)
                    .expect("erase preflight validated every inverse use");
            }
        }
        let block = self
            .module
            .blocks
            .get_mut(plan.root_parent)
            .expect("erase preflight validated the root parent block");
        debug_assert_eq!(block.operations[plan.root_position], operation);
        block.operations.remove(plan.root_position);

        for value in plan.values {
            debug_assert!(
                self.module
                    .values
                    .get(value)
                    .is_some_and(|stored| stored.uses.is_empty())
            );
            self.module.values.remove(value);
        }
        // Operations are removed in post-order, then their now-unreachable
        // structural containers are retired deepest-first.
        for owned_operation in plan.operations {
            self.module.attachments.clear_operation(owned_operation);
            self.module.operations.remove(owned_operation);
        }
        for block in plan.blocks.into_iter().rev() {
            self.module.blocks.remove(block);
        }
        for region in plan.regions.into_iter().rev() {
            self.module.regions.remove(region);
        }
        self.module.bump_revision();
        Ok(())
    }

    fn plan_operation_erase(&self, root: OperationId) -> Result<ErasePlan, EditError> {
        let root_operation = self
            .module
            .operations
            .get(root)
            .ok_or(EditError::UnknownOperation(root))?;
        let root_parent = root_operation.parent;
        let root_block = self
            .module
            .blocks
            .get(root_parent)
            .ok_or(EditError::UnknownBlock(root_parent))?;
        let mut root_positions = root_block
            .operations
            .iter()
            .enumerate()
            .filter_map(|(index, candidate)| (*candidate == root).then_some(index));
        let root_position = root_positions
            .next()
            .ok_or(EditError::CorruptOperationTree(root))?;
        if root_positions.next().is_some() {
            return Err(EditError::CorruptOperationTree(root));
        }

        let mut plan = ErasePlan {
            root_parent,
            root_position,
            operations: Vec::new(),
            regions: Vec::new(),
            blocks: Vec::new(),
            values: Vec::new(),
            operation_set: HashSet::new(),
        };
        let mut region_set = HashSet::new();
        let mut block_set = HashSet::new();
        let mut value_set = HashSet::new();
        let mut stack = vec![(root, false)];

        // An explicit enter/exit stack avoids tying supported IR nesting depth to
        // the Rust call stack while still producing operation post-order.
        while let Some((operation_id, expanded)) = stack.pop() {
            if expanded {
                plan.operations.push(operation_id);
                continue;
            }
            if !plan.operation_set.insert(operation_id) {
                return Err(EditError::CorruptOperationTree(operation_id));
            }
            let stored = self
                .module
                .operations
                .get(operation_id)
                .ok_or(EditError::UnknownOperation(operation_id))?;
            if stored.operands.len() != stored.operand_use_indices.len() {
                return Err(EditError::CorruptOperationTree(operation_id));
            }
            for (operand_index, operand) in stored.operands.iter().copied().enumerate() {
                let value = self
                    .module
                    .values
                    .get(operand)
                    .ok_or(EditError::UnknownValue(operand))?;
                let expected = ValueUse::new(operation_id, operand_index as u32);
                let use_index = stored.operand_use_indices[operand_index];
                if value.uses.get(use_index) != Some(&expected) {
                    return Err(EditError::CorruptUseList(operand));
                }
            }
            for (index, result) in stored.results.iter().copied().enumerate() {
                if !value_set.insert(result) {
                    return Err(EditError::CorruptOperationTree(operation_id));
                }
                let value = self
                    .module
                    .values
                    .get(result)
                    .ok_or(EditError::UnknownValue(result))?;
                if value.definition
                    != (ValueDefinition::OperationResult {
                        operation: operation_id,
                        index: index as u32,
                    })
                {
                    return Err(EditError::CorruptOperationTree(operation_id));
                }
                plan.values.push(result);
            }

            let mut children = Vec::new();
            for region_id in &stored.regions {
                if !region_set.insert(*region_id) {
                    return Err(EditError::CorruptOperationTree(operation_id));
                }
                let region = self
                    .module
                    .regions
                    .get(*region_id)
                    .ok_or(EditError::UnknownRegion(*region_id))?;
                if region.parent != RegionParent::Operation(operation_id) {
                    return Err(EditError::CorruptOperationTree(operation_id));
                }
                plan.regions.push(*region_id);
                for block_id in &region.blocks {
                    if !block_set.insert(*block_id) {
                        return Err(EditError::CorruptOperationTree(operation_id));
                    }
                    let block = self
                        .module
                        .blocks
                        .get(*block_id)
                        .ok_or(EditError::UnknownBlock(*block_id))?;
                    if block.parent != *region_id {
                        return Err(EditError::CorruptOperationTree(operation_id));
                    }
                    plan.blocks.push(*block_id);
                    for (index, argument) in block.arguments.iter().copied().enumerate() {
                        if !value_set.insert(argument) {
                            return Err(EditError::CorruptOperationTree(operation_id));
                        }
                        let value = self
                            .module
                            .values
                            .get(argument)
                            .ok_or(EditError::UnknownValue(argument))?;
                        if value.definition
                            != (ValueDefinition::BlockArgument {
                                block: *block_id,
                                index: index as u32,
                            })
                        {
                            return Err(EditError::CorruptOperationTree(operation_id));
                        }
                        plan.values.push(argument);
                    }
                    for child in &block.operations {
                        let child_operation = self
                            .module
                            .operations
                            .get(*child)
                            .ok_or(EditError::UnknownOperation(*child))?;
                        if child_operation.parent != *block_id {
                            return Err(EditError::CorruptOperationTree(operation_id));
                        }
                        children.push(*child);
                    }
                }
            }
            stack.push((operation_id, true));
            for child in children.into_iter().rev() {
                stack.push((child, false));
            }
        }

        let mut touched_values = value_set;
        for operation_id in &plan.operations {
            let stored = self
                .module
                .operations
                .get(*operation_id)
                .expect("collected operation remains live during preflight");
            touched_values.extend(stored.operands.iter().copied());
        }
        for value_id in touched_values {
            self.validate_use_list(value_id)?;
        }
        for value_id in &plan.values {
            let value = self
                .module
                .values
                .get(*value_id)
                .ok_or(EditError::UnknownValue(*value_id))?;
            if value
                .uses
                .iter()
                .any(|value_use| !plan.operation_set.contains(&value_use.user()))
            {
                return Err(EditError::ValueStillUsed(*value_id));
            }
        }
        Ok(plan)
    }

    fn validate_use_list(&self, value_id: ValueId) -> Result<(), EditError> {
        let value = self
            .module
            .values
            .get(value_id)
            .ok_or(EditError::UnknownValue(value_id))?;
        for (use_index, value_use) in value.uses.iter().enumerate() {
            let user = self
                .module
                .operations
                .get(value_use.user())
                .ok_or(EditError::CorruptUseList(value_id))?;
            let operand_index = value_use.operand_index() as usize;
            if user.operands.get(operand_index) != Some(&value_id)
                || user.operand_use_indices.get(operand_index).copied() != Some(use_index)
            {
                return Err(EditError::CorruptUseList(value_id));
            }
        }
        Ok(())
    }

    /// Replaces runtime profile-site metadata without changing query semantics.
    ///
    /// Profile-site ids must be unique within a module; the verifier reports
    /// duplicates after a rewrite transaction.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::UnknownOperation`] for a stale operation.
    pub fn set_profile_site(
        &mut self,
        operation: OperationId,
        profile_site: Option<ProfileSiteId>,
    ) -> Result<(), EditError> {
        self.module
            .operations
            .get_mut(operation)
            .ok_or(EditError::UnknownOperation(operation))?
            .metadata
            .profile_site = profile_site;
        self.module.bump_revision();
        Ok(())
    }

    /// Replaces diagnostic source provenance without changing semantics.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::UnknownOperation`] for a stale operation.
    pub fn set_source_span(
        &mut self,
        operation: OperationId,
        source_span: Option<SourceSpan>,
    ) -> Result<(), EditError> {
        self.module
            .operations
            .get_mut(operation)
            .ok_or(EditError::UnknownOperation(operation))?
            .metadata
            .source_span = source_span;
        self.module.bump_revision();
        Ok(())
    }

    /// Adds or replaces a semantic operation attribute.
    ///
    /// The previous attribute is returned when the key already existed.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::UnknownOperation`] for a stale operation.
    pub fn set_attribute(
        &mut self,
        operation: OperationId,
        key: impl Into<String>,
        value: Attribute,
    ) -> Result<Option<Attribute>, EditError> {
        let previous = self
            .module
            .operations
            .get_mut(operation)
            .ok_or(EditError::UnknownOperation(operation))?
            .metadata
            .attributes
            .insert(key.into(), value);
        self.module.bump_revision();
        Ok(previous)
    }

    /// Returns one SSA result by position.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale operation or invalid result position.
    pub fn result(&self, operation: OperationId, index: usize) -> Result<ValueId, EditError> {
        self.module
            .operations
            .get(operation)
            .ok_or(EditError::UnknownOperation(operation))?
            .results
            .get(index)
            .copied()
            .ok_or(EditError::ResultOutOfBounds { operation, index })
    }

    /// Returns one block argument by position.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale block or invalid argument position.
    pub fn block_argument(&self, block: BlockId, index: usize) -> Result<ValueId, EditError> {
        self.module
            .blocks
            .get(block)
            .ok_or(EditError::UnknownBlock(block))?
            .arguments
            .get(index)
            .copied()
            .ok_or(EditError::BlockArgumentOutOfBounds { block, index })
    }
}

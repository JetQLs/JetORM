use std::collections::{HashMap, HashSet};

use super::{
    BinaryOperator, BlockId, Literal, LogicalOp, Module, OperationId, OperationKind, ProfileSiteId,
    RegionId, RegionParent, ScalarOp, SchemaId, SqlType, TerminatorOp, Type, UnaryOperator,
    ValueDefinition, ValueId, ValueUse, WindowFrameBound, WindowFrameUnit, WindowSpec,
};

/// Arena entity associated with one verifier diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VerificationLocation {
    /// Module-wide invariant.
    Module,
    /// Interned row schema.
    Schema(SchemaId),
    /// Operation-owned or module-owned region.
    Region(RegionId),
    /// Basic block.
    Block(BlockId),
    /// SSA operation.
    Operation(OperationId),
    /// SSA value.
    Value(ValueId),
}

/// One independently actionable IR invariant violation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationError {
    location: VerificationLocation,
    message: String,
}

impl VerificationError {
    /// Constructs a verifier diagnostic for one entity and invariant.
    #[must_use]
    pub fn new(location: VerificationLocation, message: impl Into<String>) -> Self {
        Self {
            location,
            message: message.into(),
        }
    }

    /// Returns the entity associated with this diagnostic.
    #[must_use]
    pub const fn location(&self) -> VerificationLocation {
        self.location
    }

    /// Returns the human-readable invariant failure.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Validates the complete IR contract of a module.
///
/// Verification covers root ownership, arena membership, block termination,
/// def-use symmetry, nested-region dominance, types, CFG successors, profile-site
/// uniqueness, and built-in dialect signatures. Independent failures accumulate
/// in one run so malformed rewrites can be diagnosed without a fail-fast loop.
///
/// [`crate::ir::IrEditor`] permits incomplete intermediate states. A module must
/// pass this function before optimization, fingerprinting, or lowering.
///
/// # Errors
///
/// Returns every detected invariant violation when the module is invalid.
pub fn verify_module(module: &Module) -> Result<(), Vec<VerificationError>> {
    let mut verifier = Verifier {
        module,
        errors: Vec::new(),
        operation_positions: HashMap::new(),
        profile_sites: HashMap::new(),
    };
    verifier.verify_roots();
    verifier.index_operation_positions();
    verifier.verify_schemas();
    verifier.verify_regions();
    verifier.verify_blocks();
    verifier.verify_operations();
    verifier.verify_values();
    if verifier.errors.is_empty() {
        Ok(())
    } else {
        Err(verifier.errors)
    }
}

struct Verifier<'module> {
    module: &'module Module,
    errors: Vec<VerificationError>,
    operation_positions: HashMap<OperationId, (BlockId, usize)>,
    profile_sites: HashMap<ProfileSiteId, OperationId>,
}

impl Verifier<'_> {
    fn error(&mut self, location: VerificationLocation, message: impl Into<String>) {
        // Keep diagnostics independent: a stale edge should not hide unrelated
        // shape or type failures elsewhere in the module.
        self.errors.push(VerificationError::new(location, message));
    }

    fn verify_roots(&mut self) {
        let root_region = self.module.root_region();
        match self.module.region(root_region) {
            Some(region) if region.parent() == RegionParent::Module => {}
            Some(_) => self.error(
                VerificationLocation::Region(root_region),
                "root region must be module-owned",
            ),
            None => self.error(VerificationLocation::Module, "root region handle is stale"),
        }
        let root_block = self.module.root_block();
        match self.module.block(root_block) {
            Some(block) if block.parent() == root_region => {}
            Some(_) => self.error(
                VerificationLocation::Block(root_block),
                "root block must belong to the root region",
            ),
            None => self.error(VerificationLocation::Module, "root block handle is stale"),
        }
    }

    fn index_operation_positions(&mut self) {
        // Dominance within a block is program order, and the same index also
        // exposes duplicate block membership before deeper verification begins.
        for block_id in self.module.block_ids() {
            let Some(block) = self.module.block(block_id) else {
                continue;
            };
            for (index, operation) in block.operations().iter().copied().enumerate() {
                if self
                    .operation_positions
                    .insert(operation, (block_id, index))
                    .is_some()
                {
                    self.error(
                        VerificationLocation::Operation(operation),
                        "operation appears in more than one block position",
                    );
                }
            }
        }
    }

    fn verify_schemas(&mut self) {
        for schema_id in self.module.schema_ids() {
            let Some(schema) = self.module.schema(schema_id) else {
                continue;
            };
            let mut names = HashSet::new();
            for field in schema.fields() {
                if field.name().is_empty() {
                    self.error(
                        VerificationLocation::Schema(schema_id),
                        "schema field name must not be empty",
                    );
                }
                if !names.insert(field.name()) {
                    self.error(
                        VerificationLocation::Schema(schema_id),
                        format!("duplicate schema field {:?}", field.name()),
                    );
                }
                self.verify_field_type(schema_id, field.ty());
            }
        }
    }

    fn verify_field_type(&mut self, schema: SchemaId, ty: &Type) {
        match ty {
            Type::Scalar(scalar) => match scalar.kind() {
                SqlType::Integer { bits, .. } if !matches!(bits, 8 | 16 | 32 | 64 | 128) => {
                    self.error(
                        VerificationLocation::Schema(schema),
                        format!("integer width {bits} is unsupported"),
                    );
                }
                SqlType::Float { bits } if !matches!(bits, 16 | 32 | 64 | 128) => {
                    self.error(
                        VerificationLocation::Schema(schema),
                        format!("float width {bits} is unsupported"),
                    );
                }
                SqlType::Decimal { precision, scale }
                    if invalid_decimal(*precision, *scale) =>
                {
                    self.error(
                        VerificationLocation::Schema(schema),
                        "decimal precision must be positive and cover the scale",
                    );
                }
                SqlType::Time { precision } | SqlType::Timestamp { precision, .. }
                    if *precision > 9 =>
                {
                    self.error(
                        VerificationLocation::Schema(schema),
                        "time precision must be at most 9",
                    );
                }
                SqlType::Custom(name) if name.is_empty() => self.error(
                    VerificationLocation::Schema(schema),
                    "custom scalar type name must not be empty",
                ),
                SqlType::Array { element } => {
                    if let Some(message) = scalar_type_error(element) {
                        self.error(VerificationLocation::Schema(schema), message);
                    }
                }
                SqlType::Boolean
                | SqlType::Integer { .. }
                | SqlType::Float { .. }
                | SqlType::Decimal { .. }
                | SqlType::Utf8
                | SqlType::Binary
                | SqlType::Date
                | SqlType::Time { .. }
                | SqlType::Timestamp { .. }
                | SqlType::Interval
                | SqlType::Uuid
                | SqlType::Json
                | SqlType::Custom(_) => {}
            },
            Type::Tuple(elements) => {
                for element in elements {
                    self.verify_field_type(schema, element);
                }
            }
            Type::Relation(_) => self.error(
                VerificationLocation::Schema(schema),
                "row fields cannot contain relation values",
            ),
            Type::Unit => self.error(
                VerificationLocation::Schema(schema),
                "row fields cannot have unit type",
            ),
        }
    }

    fn verify_regions(&mut self) {
        for region_id in self.module.region_ids() {
            let Some(region) = self.module.region(region_id) else {
                continue;
            };
            if region.blocks().is_empty() {
                self.error(
                    VerificationLocation::Region(region_id),
                    "region must contain at least one block",
                );
            }
            let mut blocks = HashSet::new();
            for block_id in region.blocks() {
                if !blocks.insert(*block_id) {
                    self.error(
                        VerificationLocation::Region(region_id),
                        format!("block {block_id} occurs more than once"),
                    );
                }
                match self.module.block(*block_id) {
                    Some(block) if block.parent() == region_id => {}
                    Some(_) => self.error(
                        VerificationLocation::Block(*block_id),
                        "block parent disagrees with region membership",
                    ),
                    None => self.error(
                        VerificationLocation::Region(region_id),
                        format!("region references stale block {block_id}"),
                    ),
                }
            }
            match region.parent() {
                RegionParent::Module if region_id == self.module.root_region() => {}
                RegionParent::Module => self.error(
                    VerificationLocation::Region(region_id),
                    "only the root region may be module-owned",
                ),
                RegionParent::Operation(owner) => match self.module.operation(owner) {
                    Some(operation) if operation.regions().contains(&region_id) => {}
                    Some(_) => self.error(
                        VerificationLocation::Region(region_id),
                        "owning operation does not reference nested region",
                    ),
                    None => self.error(
                        VerificationLocation::Region(region_id),
                        format!("region owner {owner} is stale"),
                    ),
                },
            }
        }
    }

    fn verify_blocks(&mut self) {
        for block_id in self.module.block_ids() {
            let Some(block) = self.module.block(block_id) else {
                continue;
            };
            match self.module.region(block.parent()) {
                Some(region) if region.blocks().contains(&block_id) => {}
                Some(_) => self.error(
                    VerificationLocation::Block(block_id),
                    "parent region does not reference block",
                ),
                None => self.error(
                    VerificationLocation::Block(block_id),
                    "block parent region is stale",
                ),
            }
            let mut arguments = HashSet::new();
            for (index, argument) in block.arguments().iter().copied().enumerate() {
                if !arguments.insert(argument) {
                    self.error(
                        VerificationLocation::Block(block_id),
                        format!("block argument {argument} occurs more than once"),
                    );
                }
                match self.module.value(argument) {
                    Some(value)
                        if value.definition()
                            == (ValueDefinition::BlockArgument {
                                block: block_id,
                                index: index as u32,
                            }) => {}
                    Some(_) => self.error(
                        VerificationLocation::Value(argument),
                        "block argument definition disagrees with its position",
                    ),
                    None => self.error(
                        VerificationLocation::Block(block_id),
                        format!("block references stale argument {argument}"),
                    ),
                }
            }
            if block.operations().is_empty() {
                self.error(
                    VerificationLocation::Block(block_id),
                    "block must end in a terminator",
                );
                continue;
            }
            for (index, operation_id) in block.operations().iter().copied().enumerate() {
                let Some(operation) = self.module.operation(operation_id) else {
                    self.error(
                        VerificationLocation::Block(block_id),
                        format!("block references stale operation {operation_id}"),
                    );
                    continue;
                };
                if operation.parent() != block_id {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "operation parent disagrees with block membership",
                    );
                }
                let is_last = index + 1 == block.operations().len();
                if operation.is_terminator() != is_last {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        if is_last {
                            "last operation in a block must be a terminator"
                        } else {
                            "terminator must be the final operation in its block"
                        },
                    );
                }
            }
        }
    }

    fn verify_operations(&mut self) {
        for operation_id in self.module.operation_ids() {
            let Some(operation) = self.module.operation(operation_id) else {
                continue;
            };
            if !self.operation_positions.contains_key(&operation_id) {
                self.error(
                    VerificationLocation::Operation(operation_id),
                    "operation is not attached to a block",
                );
            }
            if operation.operands().len() != operation.operand_use_indices.len() {
                self.error(
                    VerificationLocation::Operation(operation_id),
                    "operand and inverse-use backlink counts must match",
                );
            }
            for (operand_index, operand) in operation.operands().iter().copied().enumerate() {
                let Some(value) = self.module.value(operand) else {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        format!("operand {operand_index} references stale value {operand}"),
                    );
                    continue;
                };
                let expected_use = ValueUse::new(operation_id, operand_index as u32);
                let backlink = operation.operand_use_indices.get(operand_index).copied();
                if backlink.and_then(|index| value.uses().get(index)) != Some(&expected_use) {
                    self.error(
                        VerificationLocation::Value(operand),
                        format!(
                            "inverse-use backlink for {operation_id} operand {operand_index} is invalid"
                        ),
                    );
                }
                if !self.value_dominates_use(operand, operation_id) {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        format!(
                            "operand {operand_index} value {operand} does not dominate its use"
                        ),
                    );
                }
            }
            for (result_index, result) in operation.results().iter().copied().enumerate() {
                match self.module.value(result) {
                    Some(value)
                        if value.definition()
                            == (ValueDefinition::OperationResult {
                                operation: operation_id,
                                index: result_index as u32,
                            }) => {}
                    Some(_) => self.error(
                        VerificationLocation::Value(result),
                        "result definition disagrees with operation result position",
                    ),
                    None => self.error(
                        VerificationLocation::Operation(operation_id),
                        format!("operation references stale result {result}"),
                    ),
                }
            }
            for region in operation.regions() {
                match self.module.region(*region) {
                    Some(nested) if nested.parent() == RegionParent::Operation(operation_id) => {}
                    Some(_) => self.error(
                        VerificationLocation::Region(*region),
                        "nested region parent disagrees with owning operation",
                    ),
                    None => self.error(
                        VerificationLocation::Operation(operation_id),
                        format!("operation references stale region {region}"),
                    ),
                }
            }
            for successor in operation.successors() {
                let parent_region = self
                    .module
                    .block(operation.parent())
                    .map(super::Block::parent);
                match self.module.block(*successor) {
                    Some(block) if Some(block.parent()) == parent_region => {}
                    Some(_) => self.error(
                        VerificationLocation::Operation(operation_id),
                        format!("successor {successor} must belong to the same region"),
                    ),
                    None => self.error(
                        VerificationLocation::Operation(operation_id),
                        format!("operation references stale successor {successor}"),
                    ),
                }
            }
            if !operation.successors().is_empty() && !operation.is_terminator() {
                self.error(
                    VerificationLocation::Operation(operation_id),
                    "only a terminator may carry CFG successors",
                );
            }
            if let Some(span) = operation.metadata().source_span()
                && span.start() > span.end()
            {
                self.error(
                    VerificationLocation::Operation(operation_id),
                    "source span start must not exceed its end",
                );
            }
            if let Some(site) = operation.metadata().profile_site()
                && let Some(previous) = self.profile_sites.insert(site, operation_id)
            {
                // Profile records address sites, so aliases would merge runtime
                // observations from semantically unrelated operations.
                self.error(
                    VerificationLocation::Operation(operation_id),
                    format!("profile site {site} is already assigned to {previous}"),
                );
            }
            self.verify_operation_signature(operation_id);
        }
    }

    fn verify_values(&mut self) {
        for value_id in self.module.value_ids() {
            let Some(value) = self.module.value(value_id) else {
                continue;
            };
            self.verify_value_type(value_id, value.ty());
            match value.definition() {
                ValueDefinition::BlockArgument { block, index } => match self.module.block(block) {
                    Some(owner) if owner.arguments().get(index as usize) == Some(&value_id) => {}
                    Some(_) => self.error(
                        VerificationLocation::Value(value_id),
                        "block does not contain value at its declared argument index",
                    ),
                    None => self.error(
                        VerificationLocation::Value(value_id),
                        "value definition references stale block",
                    ),
                },
                ValueDefinition::OperationResult { operation, index } => {
                    match self.module.operation(operation) {
                        Some(owner) if owner.results().get(index as usize) == Some(&value_id) => {}
                        Some(_) => self.error(
                            VerificationLocation::Value(value_id),
                            "operation does not contain value at its declared result index",
                        ),
                        None => self.error(
                            VerificationLocation::Value(value_id),
                            "value definition references stale operation",
                        ),
                    }
                }
            }
            let mut unique_uses = HashSet::new();
            for (use_index, value_use) in value.uses().iter().enumerate() {
                if !unique_uses.insert(*value_use) {
                    self.error(
                        VerificationLocation::Value(value_id),
                        "value use-list contains a duplicate entry",
                    );
                }
                match self.module.operation(value_use.user()) {
                    Some(user)
                        if user.operands().get(value_use.operand_index() as usize)
                            == Some(&value_id)
                            && user
                                .operand_use_indices
                                .get(value_use.operand_index() as usize)
                                .copied()
                                == Some(use_index) => {}
                    Some(_) => self.error(
                        VerificationLocation::Value(value_id),
                        "inverse use and operand backlink do not point to each other",
                    ),
                    None => self.error(
                        VerificationLocation::Value(value_id),
                        "use-list references stale operation",
                    ),
                }
            }
        }
    }

    fn verify_value_type(&mut self, value: ValueId, ty: &Type) {
        match ty {
            Type::Scalar(scalar) => {
                if let Some(message) = scalar_type_error(scalar.kind()) {
                    self.error(VerificationLocation::Value(value), message);
                }
            }
            Type::Tuple(elements) => {
                for element in elements {
                    self.verify_value_type(value, element);
                }
            }
            Type::Relation(schema) if self.module.schema(*schema).is_none() => self.error(
                VerificationLocation::Value(value),
                format!("relation type references stale schema {schema}"),
            ),
            Type::Relation(_) | Type::Unit => {}
        }
    }

    fn value_dominates_use(&self, value_id: ValueId, user: OperationId) -> bool {
        // A nested region may capture values visible at its owning operation. Walk
        // outward through owner operations until the definition block is reached,
        // then apply same-block program order. Direct SSA edges across sibling CFG
        // blocks are intentionally rejected; block arguments carry those values.
        let Some(value) = self.module.value(value_id) else {
            return false;
        };
        let definition_block = match value.definition() {
            ValueDefinition::BlockArgument { block, .. } => block,
            ValueDefinition::OperationResult { operation, .. } => {
                let Some(definition) = self.module.operation(operation) else {
                    return false;
                };
                definition.parent()
            }
        };
        let mut current_user = user;
        loop {
            let Some(current_operation) = self.module.operation(current_user) else {
                return false;
            };
            let user_block = current_operation.parent();
            if definition_block == user_block {
                return match value.definition() {
                    ValueDefinition::BlockArgument { .. } => true,
                    ValueDefinition::OperationResult { operation, .. } => {
                        let Some((_, definition_index)) = self.operation_positions.get(&operation)
                        else {
                            return false;
                        };
                        let Some((_, use_index)) = self.operation_positions.get(&current_user)
                        else {
                            return false;
                        };
                        definition_index < use_index
                    }
                };
            }
            let Some(block) = self.module.block(user_block) else {
                return false;
            };
            let Some(region) = self.module.region(block.parent()) else {
                return false;
            };
            match region.parent() {
                RegionParent::Operation(owner) => current_user = owner,
                RegionParent::Module => return false,
            }
        }
    }

    fn verify_operation_signature(&mut self, operation_id: OperationId) {
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        let kind = operation.kind().clone();
        match kind {
            OperationKind::Logical(logical) => {
                if !operation.successors().is_empty() {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "logical operation cannot have CFG successors",
                    );
                }
                self.verify_logical(operation_id, &logical);
            }
            OperationKind::Scalar(scalar) => {
                if !operation.regions().is_empty() || !operation.successors().is_empty() {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "scalar operation cannot own regions or CFG successors",
                    );
                }
                self.verify_scalar(operation_id, &scalar);
            }
            OperationKind::Terminator(terminator) => {
                if !operation.results().is_empty() || !operation.regions().is_empty() {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "terminator cannot produce results or own regions",
                    );
                }
                if !operation.successors().is_empty() {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "built-in terminator cannot have CFG successors",
                    );
                }
                let in_root = self
                    .module
                    .block(operation.parent())
                    .is_some_and(|block| block.parent() == self.module.root_region());
                match terminator {
                    TerminatorOp::Yield if in_root => self.error(
                        VerificationLocation::Operation(operation_id),
                        "yield is valid only inside an operation-owned region",
                    ),
                    TerminatorOp::QueryReturn if !in_root => self.error(
                        VerificationLocation::Operation(operation_id),
                        "query return is valid only in the module root block",
                    ),
                    TerminatorOp::QueryReturn => {
                        if operation.operands().len() != 1
                            || self
                                .operand_type(operation_id, 0)
                                .and_then(Type::as_relation)
                                .is_none()
                        {
                            self.error(
                                VerificationLocation::Operation(operation_id),
                                "query return requires exactly one relation operand",
                            );
                        }
                    }
                    TerminatorOp::Yield => {}
                }
            }
            OperationKind::Extension(extension) => {
                if extension.dialect().is_empty() || extension.name().is_empty() {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "extension dialect and operation names must not be empty",
                    );
                }
            }
        }
    }

    fn verify_logical(&mut self, operation_id: OperationId, logical: &LogicalOp) {
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        match logical {
            LogicalOp::Scan { columns, .. } => {
                self.expect_shape(operation_id, 0, 1, 0);
                if let Some(schema) = self.result_schema(operation_id, 0)
                    && self.module.schema(schema).map(super::Schema::len) != Some(columns.len())
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "scan column count must match its result schema",
                    );
                }
            }
            LogicalOp::Values { rows } => {
                self.expect_shape(operation_id, 0, 1, 0);
                if let Some(schema) = self.result_schema(operation_id, 0) {
                    let width = self.module.schema(schema).map_or(0, super::Schema::len);
                    if rows.iter().any(|row| row.len() != width) {
                        self.error(
                            VerificationLocation::Operation(operation_id),
                            "every values row must match its result schema width",
                        );
                    }
                }
            }
            LogicalOp::Empty => self.expect_shape(operation_id, 0, 1, 0),
            LogicalOp::Filter => {
                self.expect_shape(operation_id, 1, 1, 1);
                self.expect_same_relation_io(operation_id);
                if let Some(input) = self.operand_schema(operation_id, 0) {
                    self.verify_expression_region(operation_id, input, YieldExpectation::Boolean);
                }
            }
            LogicalOp::Project => {
                self.expect_shape(operation_id, 1, 1, 1);
                if let (Some(input), Some(output)) = (
                    self.operand_schema(operation_id, 0),
                    self.result_schema(operation_id, 0),
                ) {
                    self.verify_expression_region(
                        operation_id,
                        input,
                        YieldExpectation::Schema(output),
                    );
                }
            }
            LogicalOp::Join {
                kind,
                has_condition,
            } => {
                self.expect_shape(operation_id, 2, 1, usize::from(*has_condition));
                if *kind == super::JoinKind::Cross && *has_condition {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "cross join cannot carry a condition region",
                    );
                }
                if *has_condition
                    && let (Some(left), Some(right)) = (
                        self.operand_schema(operation_id, 0),
                        self.operand_schema(operation_id, 1),
                    )
                {
                    self.verify_join_region(operation_id, left, right);
                }
                if self.result_schema(operation_id, 0).is_none() {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "join must produce one relation",
                    );
                }
                self.verify_join_result_schema(operation_id, *kind);
            }
            LogicalOp::Aggregate { group_keys } => {
                self.expect_shape(operation_id, 1, 1, 1);
                if let (Some(input), Some(output)) = (
                    self.operand_schema(operation_id, 0),
                    self.result_schema(operation_id, 0),
                ) {
                    self.verify_aggregate_region(operation_id, input, output, *group_keys);
                }
            }
            LogicalOp::Window => {
                self.expect_shape(operation_id, 1, 1, 1);
                if let (Some(input), Some(output)) = (
                    self.operand_schema(operation_id, 0),
                    self.result_schema(operation_id, 0),
                ) {
                    self.verify_expression_region(
                        operation_id,
                        input,
                        YieldExpectation::Schema(output),
                    );
                }
            }
            LogicalOp::Sort { keys } => {
                self.expect_shape(operation_id, 1, 1, usize::from(!keys.is_empty()));
                self.expect_same_relation_io(operation_id);
                if !keys.is_empty()
                    && let Some(input) = self.operand_schema(operation_id, 0)
                {
                    self.verify_expression_region(
                        operation_id,
                        input,
                        YieldExpectation::Count(keys.len()),
                    );
                }
            }
            LogicalOp::Limit {
                has_offset,
                has_fetch,
            } => {
                let counts = usize::from(*has_offset) + usize::from(*has_fetch);
                self.expect_shape(operation_id, 1 + counts, 1, 0);
                self.expect_same_relation_io(operation_id);
                if counts == 0 {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "limit must specify an offset, fetch count, or both",
                    );
                }
                for index in 1..=counts {
                    let valid = self
                        .operand_type(operation_id, index)
                        .and_then(Type::as_scalar)
                        .is_some_and(|scalar| {
                            matches!(scalar.kind(), SqlType::Integer { .. })
                                && !scalar.is_nullable()
                        });
                    if !valid {
                        self.error(
                            VerificationLocation::Operation(operation_id),
                            "limit count operands must be non-nullable integers",
                        );
                    }
                }
            }
            LogicalOp::Distinct => {
                self.expect_shape(operation_id, 1, 1, 0);
                self.expect_same_relation_io(operation_id);
            }
            LogicalOp::Set { .. } => {
                if operation.operands().len() < 2
                    || operation.results().len() != 1
                    || !operation.regions().is_empty()
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "set operation requires at least two operands, one result, and no regions",
                    );
                }
                let expected = self.result_type(operation_id, 0).cloned();
                if expected.is_none()
                    || expected.as_ref().and_then(Type::as_relation).is_none()
                    || operation.operands().iter().any(|operand| {
                        self.module.value(*operand).map(super::Value::ty) != expected.as_ref()
                    })
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "set operands and result must have one identical relation type",
                    );
                }
            }
        }
    }

    fn verify_scalar(&mut self, operation_id: OperationId, scalar: &ScalarOp) {
        let operand_count = self
            .module
            .operation(operation_id)
            .map_or(0, |operation| operation.operands().len());
        let result_count = self
            .module
            .operation(operation_id)
            .map_or(0, |operation| operation.results().len());
        if result_count != 1 {
            self.error(
                VerificationLocation::Operation(operation_id),
                "scalar operation must produce exactly one result",
            );
        } else if self
            .result_type(operation_id, 0)
            .and_then(Type::as_scalar)
            .is_none()
        {
            self.error(
                VerificationLocation::Operation(operation_id),
                "scalar operation result must have a scalar SQL type",
            );
        }
        match scalar {
            ScalarOp::Literal(literal) => {
                if operand_count != 0 {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "literal cannot have operands",
                    );
                }
                if let Some(ty) = self.result_type(operation_id, 0)
                    && !literal_matches_type(literal, ty)
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "literal payload is incompatible with its result type",
                    );
                }
            }
            ScalarOp::Parameter { .. } => {
                if operand_count != 0 {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "parameter cannot have operands",
                    );
                }
            }
            ScalarOp::Unary(operator) => {
                if operand_count != 1 {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "unary operation requires one operand",
                    );
                }
                match operator {
                    UnaryOperator::Not => {
                        if !self.operand_is_boolean(operation_id, 0)
                            || !self.result_is_boolean(operation_id, 0)
                        {
                            self.error(
                                VerificationLocation::Operation(operation_id),
                                "logical not requires a boolean operand and result",
                            );
                        }
                    }
                    UnaryOperator::IsNull | UnaryOperator::IsNotNull => {
                        let result = self.result_type(operation_id, 0).and_then(Type::as_scalar);
                        if result.is_none_or(|scalar| !scalar.is_boolean() || scalar.is_nullable())
                        {
                            self.error(
                                VerificationLocation::Operation(operation_id),
                                "null test must produce a non-nullable boolean",
                            );
                        }
                    }
                    UnaryOperator::Negate => {
                        if self.operand_type(operation_id, 0) != self.result_type(operation_id, 0) {
                            self.error(
                                VerificationLocation::Operation(operation_id),
                                "negation operand and result types must match",
                            );
                        }
                    }
                }
            }
            ScalarOp::Binary(operator) => {
                if operand_count != 2 {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "binary operation requires two operands",
                    );
                }
                let left = self.operand_type(operation_id, 0).cloned();
                let right = self.operand_type(operation_id, 1).cloned();
                let result = self.result_type(operation_id, 0).cloned();
                if left.is_none()
                    || left.as_ref().and_then(Type::as_scalar).is_none()
                    || right.as_ref().and_then(Type::as_scalar).is_none()
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "binary operation requires scalar operands",
                    );
                }
                if *operator == BinaryOperator::InArray {
                    // Membership pairs a scalar with an array of its kind
                    // rather than two operands of one type.
                    let element_matches = match (
                        left.as_ref().and_then(Type::as_scalar),
                        right.as_ref().and_then(Type::as_scalar),
                    ) {
                        (Some(scalar), Some(array)) => match array.kind() {
                            SqlType::Array { element } => **element == *scalar.kind(),
                            _ => false,
                        },
                        _ => false,
                    };
                    if !element_matches {
                        self.error(
                            VerificationLocation::Operation(operation_id),
                            "in-array requires an array operand of the scalar operand's kind",
                        );
                    }
                } else if left != right {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "binary operand types must match after explicit coercion",
                    );
                }
                if binary_returns_boolean(*operator) && !self.result_is_boolean(operation_id, 0) {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "predicate binary operation must produce a boolean",
                    );
                }
                if !binary_returns_boolean(*operator) && left != result {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "value-producing binary result type must match its operands",
                    );
                }
                if matches!(operator, BinaryOperator::And | BinaryOperator::Or)
                    && !self.operand_is_boolean(operation_id, 0)
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "logical binary operation requires boolean operands",
                    );
                }
                if *operator == BinaryOperator::IsDistinctFrom
                    && self
                        .result_type(operation_id, 0)
                        .and_then(Type::as_scalar)
                        .is_none_or(super::ScalarType::is_nullable)
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "is distinct from must produce a non-nullable boolean",
                    );
                }
            }
            ScalarOp::Cast { to } => {
                if operand_count != 1 || self.result_type(operation_id, 0) != Some(to) {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "cast requires one operand and a result matching its target type",
                    );
                }
            }
            ScalarOp::Case { arms } => {
                let expected = (*arms as usize).saturating_mul(2).saturating_add(1);
                if operand_count != expected {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        format!("case with {arms} arms requires {expected} operands"),
                    );
                }
                let result_type = self.result_type(operation_id, 0).cloned();
                for arm in 0..*arms as usize {
                    if !self.operand_is_boolean(operation_id, arm * 2) {
                        self.error(
                            VerificationLocation::Operation(operation_id),
                            format!("case condition {arm} must be boolean"),
                        );
                    }
                    if self.operand_type(operation_id, arm * 2 + 1) != result_type.as_ref() {
                        self.error(
                            VerificationLocation::Operation(operation_id),
                            format!("case value {arm} must match the result type"),
                        );
                    }
                }
                if expected > 0
                    && self.operand_type(operation_id, expected - 1) != result_type.as_ref()
                {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "case else value must match the result type",
                    );
                }
            }
            ScalarOp::Call { .. } => {
                self.verify_scalar_call_operands(operation_id, "function");
            }
            ScalarOp::AggregateCall { .. } => {
                if !matches!(
                    self.enclosing_logical(operation_id),
                    Some(LogicalOp::Aggregate { .. })
                ) {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "aggregate calls are valid only inside an aggregate region",
                    );
                }
                self.verify_scalar_call_operands(operation_id, "aggregate");
                self.verify_no_nested_aggregate_or_window_calls(operation_id, "aggregate");
            }
            ScalarOp::WindowCall {
                argument_count,
                window,
                ..
            } => {
                if !matches!(
                    self.enclosing_logical(operation_id),
                    Some(LogicalOp::Window)
                ) {
                    self.error(
                        VerificationLocation::Operation(operation_id),
                        "window calls are valid only inside a window region",
                    );
                }
                self.verify_scalar_call_operands(operation_id, "window");
                self.verify_no_nested_aggregate_or_window_calls(operation_id, "window");
                self.verify_window_spec(operation_id, *argument_count, window);
            }
        }
    }

    fn enclosing_logical(&self, operation_id: OperationId) -> Option<&LogicalOp> {
        let operation = self.module.operation(operation_id)?;
        let block = self.module.block(operation.parent())?;
        let region = self.module.region(block.parent())?;
        let RegionParent::Operation(owner) = region.parent() else {
            return None;
        };
        let owner = self.module.operation(owner)?;
        let OperationKind::Logical(logical) = owner.kind() else {
            return None;
        };
        Some(logical)
    }

    fn verify_no_nested_aggregate_or_window_calls(
        &mut self,
        operation_id: OperationId,
        call_kind: &str,
    ) {
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        let block = operation.parent();
        let empty_groups = HashSet::new();
        let mut contains_special_call = false;
        for operand in operation.operands() {
            let mut memo = HashMap::new();
            let dependencies =
                self.aggregate_dependencies(block, *operand, &empty_groups, &mut memo);
            contains_special_call |=
                dependencies.contains_aggregate || dependencies.contains_window;
        }
        if contains_special_call {
            self.error(
                VerificationLocation::Operation(operation_id),
                format!("{call_kind} call operands cannot contain aggregate or window calls"),
            );
        }
    }

    fn verify_scalar_call_operands(&mut self, operation_id: OperationId, call_kind: &str) {
        let has_non_scalar = self
            .module
            .operation(operation_id)
            .is_some_and(|operation| {
                operation.operands().iter().any(|operand| {
                    self.module
                        .value(*operand)
                        .is_none_or(|value| value.ty().as_scalar().is_none())
                })
            });
        if has_non_scalar {
            self.error(
                VerificationLocation::Operation(operation_id),
                format!("{call_kind} call operands must be scalar"),
            );
        }
    }

    fn verify_window_spec(
        &mut self,
        operation_id: OperationId,
        argument_count: u32,
        window: &WindowSpec,
    ) {
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        let Ok(partition_count) = usize::try_from(window.partition_key_count()) else {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window partition-key count does not fit this target",
            );
            return;
        };
        let Ok(argument_count) = usize::try_from(argument_count) else {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window function-argument count does not fit this target",
            );
            return;
        };
        let order_count = window.order_keys().len();
        let offset_count = window.frame().map_or(0, |frame| frame.offset_count());
        let Some(expected_count) = argument_count
            .checked_add(partition_count)
            .and_then(|count| count.checked_add(order_count))
            .and_then(|count| count.checked_add(offset_count))
        else {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window operand layout overflows this target",
            );
            return;
        };
        if expected_count != operation.operands().len() {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window operand count must equal its function, partition, order, and frame layout",
            );
            return;
        }

        let metadata_start = argument_count;
        let offset_start = metadata_start + partition_count + order_count;
        if operation.operands()[metadata_start..]
            .iter()
            .any(|operand| {
                self.module
                    .value(*operand)
                    .is_none_or(|value| value.ty().as_scalar().is_none())
            })
        {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window partition, order, and frame-offset operands must be scalar",
            );
        }

        if let Some(frame) = window.frame() {
            self.verify_window_frame(operation_id, window, *frame);
            let empty_groups = HashSet::new();
            for offset in &operation.operands()[offset_start..] {
                let mut memo = HashMap::new();
                let dependencies = self.aggregate_dependencies(
                    operation.parent(),
                    *offset,
                    &empty_groups,
                    &mut memo,
                );
                if dependencies.unaggregated_row
                    || dependencies.contains_aggregate
                    || dependencies.contains_window
                {
                    self.error(
                        VerificationLocation::Value(*offset),
                        "window frame offsets cannot reference rows, aggregates, or windows",
                    );
                }
                if matches!(
                    frame.unit(),
                    WindowFrameUnit::Rows | WindowFrameUnit::Groups
                ) && self
                    .module
                    .value(*offset)
                    .and_then(|value| value.ty().as_scalar())
                    .is_some_and(|scalar| !matches!(scalar.kind(), SqlType::Integer { .. }))
                {
                    self.error(
                        VerificationLocation::Value(*offset),
                        "ROWS and GROUPS frame offsets must have an integer type",
                    );
                }
            }
        }
    }

    fn verify_window_frame(
        &mut self,
        operation_id: OperationId,
        window: &WindowSpec,
        frame: super::WindowFrame,
    ) {
        if frame.start() == WindowFrameBound::UnboundedFollowing {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window frame start cannot be UNBOUNDED FOLLOWING",
            );
        }
        if frame.end() == Some(WindowFrameBound::UnboundedPreceding) {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window frame end cannot be UNBOUNDED PRECEDING",
            );
        }
        let effective_end = frame.end().unwrap_or(WindowFrameBound::CurrentRow);
        if window_bound_rank(effective_end) < window_bound_rank(frame.start()) {
            self.error(
                VerificationLocation::Operation(operation_id),
                "window frame end cannot precede its start category",
            );
        }
        let has_offset = frame.offset_count() != 0;
        if frame.unit() == WindowFrameUnit::Range && has_offset && window.order_keys().len() != 1 {
            self.error(
                VerificationLocation::Operation(operation_id),
                "RANGE frames with offsets require exactly one ordering key",
            );
        }
        if frame.unit() == WindowFrameUnit::Groups && window.order_keys().is_empty() {
            self.error(
                VerificationLocation::Operation(operation_id),
                "GROUPS frames require at least one ordering key",
            );
        }
    }

    fn aggregate_dependencies(
        &self,
        block: BlockId,
        value_id: ValueId,
        group_keys: &HashSet<ValueId>,
        memo: &mut HashMap<ValueId, AggregateDependencies>,
    ) -> AggregateDependencies {
        if group_keys.contains(&value_id) {
            return AggregateDependencies::default();
        }
        if let Some(dependencies) = memo.get(&value_id) {
            return *dependencies;
        }
        // Break malformed cycles. The ordinary dominance verifier reports the
        // structural error; this semantic walk must remain total on invalid IR.
        memo.insert(value_id, AggregateDependencies::default());
        let dependencies = match self.module.value(value_id).map(super::Value::definition) {
            Some(ValueDefinition::BlockArgument {
                block: definition, ..
            }) => AggregateDependencies {
                unaggregated_row: definition == block,
                ..AggregateDependencies::default()
            },
            Some(ValueDefinition::OperationResult { operation, .. }) => {
                let Some(defining) = self.module.operation(operation) else {
                    return AggregateDependencies::default();
                };
                match defining.kind() {
                    OperationKind::Scalar(ScalarOp::AggregateCall { .. }) => {
                        AggregateDependencies {
                            contains_aggregate: true,
                            ..AggregateDependencies::default()
                        }
                    }
                    OperationKind::Scalar(ScalarOp::WindowCall { .. }) => AggregateDependencies {
                        contains_window: true,
                        ..AggregateDependencies::default()
                    },
                    OperationKind::Scalar(_) => {
                        let mut dependencies = AggregateDependencies::default();
                        for operand in defining.operands() {
                            dependencies = dependencies.union(
                                self.aggregate_dependencies(block, *operand, group_keys, memo),
                            );
                        }
                        dependencies
                    }
                    OperationKind::Logical(_)
                    | OperationKind::Terminator(_)
                    | OperationKind::Extension(_) => AggregateDependencies::default(),
                }
            }
            None => AggregateDependencies::default(),
        };
        memo.insert(value_id, dependencies);
        dependencies
    }

    fn expect_shape(
        &mut self,
        operation_id: OperationId,
        operands: usize,
        results: usize,
        regions: usize,
    ) {
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        if operation.operands().len() != operands
            || operation.results().len() != results
            || operation.regions().len() != regions
        {
            self.error(
                VerificationLocation::Operation(operation_id),
                format!(
                    "operation requires {operands} operands, {results} results, and {regions} regions"
                ),
            );
        }
    }

    fn expect_same_relation_io(&mut self, operation_id: OperationId) {
        let operand = self.operand_type(operation_id, 0).cloned();
        let result = self.result_type(operation_id, 0).cloned();
        if operand.is_none()
            || operand != result
            || operand.as_ref().and_then(Type::as_relation).is_none()
        {
            self.error(
                VerificationLocation::Operation(operation_id),
                "operation input and result must have the same relation type",
            );
        }
    }

    fn verify_expression_region(
        &mut self,
        operation_id: OperationId,
        input: SchemaId,
        expectation: YieldExpectation,
    ) {
        // Relational expression regions model a row lambda: one block receives
        // input fields as block arguments and yields the computed scalar values.
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        let Some(region_id) = operation.regions().first().copied() else {
            return;
        };
        let Some(region) = self.module.region(region_id) else {
            return;
        };
        if region.blocks().len() != 1 {
            self.error(
                VerificationLocation::Region(region_id),
                "logical expression region must contain exactly one block",
            );
            return;
        }
        let block_id = region.blocks()[0];
        let Some(block) = self.module.block(block_id) else {
            return;
        };
        let expected_arguments = self
            .module
            .schema(input)
            .map(super::Schema::field_types)
            .unwrap_or_default();
        let actual_arguments: Vec<Type> = block
            .arguments()
            .iter()
            .filter_map(|argument| self.module.value(*argument).map(|value| value.ty().clone()))
            .collect();
        if actual_arguments != expected_arguments {
            self.error(
                VerificationLocation::Block(block_id),
                "expression region arguments must match input row fields",
            );
        }
        self.verify_yield(block_id, expectation);
    }

    fn verify_aggregate_region(
        &mut self,
        operation_id: OperationId,
        input: SchemaId,
        output: SchemaId,
        group_keys: u32,
    ) {
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        let Some(region_id) = operation.regions().first().copied() else {
            return;
        };
        let Some(region) = self.module.region(region_id) else {
            return;
        };
        if region.blocks().len() != 1 {
            self.error(
                VerificationLocation::Region(region_id),
                "aggregate expression region must contain exactly one block",
            );
            return;
        }
        let block_id = region.blocks()[0];
        let Some(block) = self.module.block(block_id) else {
            return;
        };
        let expected_arguments = self
            .module
            .schema(input)
            .map(super::Schema::field_types)
            .unwrap_or_default();
        let actual_arguments: Vec<Type> = block
            .arguments()
            .iter()
            .filter_map(|argument| self.module.value(*argument).map(|value| value.ty().clone()))
            .collect();
        if actual_arguments != expected_arguments {
            self.error(
                VerificationLocation::Block(block_id),
                "aggregate region arguments must match input row fields",
            );
        }

        let Some(terminator_id) = block.operations().last().copied() else {
            return;
        };
        let Some(terminator) = self.module.operation(terminator_id) else {
            return;
        };
        if terminator.kind() != &OperationKind::Terminator(TerminatorOp::Yield) {
            self.error(
                VerificationLocation::Operation(terminator_id),
                "aggregate expression region must end in yield",
            );
            return;
        }
        let Ok(group_count) = usize::try_from(group_keys) else {
            self.error(
                VerificationLocation::Operation(operation_id),
                "aggregate group-key count does not fit this target",
            );
            return;
        };
        let output_types = self
            .module
            .schema(output)
            .map(super::Schema::field_types)
            .unwrap_or_default();
        let Some(expected_count) = group_count.checked_add(output_types.len()) else {
            self.error(
                VerificationLocation::Operation(operation_id),
                "aggregate yield count overflows this target",
            );
            return;
        };
        if terminator.operands().len() != expected_count {
            self.error(
                VerificationLocation::Operation(terminator_id),
                format!(
                    "aggregate region must yield {group_count} group keys followed by {} output values",
                    output_types.len()
                ),
            );
            return;
        }
        let (group_values, output_values) = terminator.operands().split_at(group_count);
        let yielded_output_types: Vec<Type> = output_values
            .iter()
            .filter_map(|value| self.module.value(*value).map(|stored| stored.ty().clone()))
            .collect();
        if yielded_output_types != output_types {
            self.error(
                VerificationLocation::Operation(terminator_id),
                "aggregate output values must match the result relation schema",
            );
        }

        let no_groups = HashSet::new();
        for group in group_values {
            if self
                .module
                .value(*group)
                .is_none_or(|value| value.ty().as_scalar().is_none())
            {
                self.error(
                    VerificationLocation::Value(*group),
                    "aggregate group keys must be scalar",
                );
            }
            let mut memo = HashMap::new();
            let dependencies = self.aggregate_dependencies(block_id, *group, &no_groups, &mut memo);
            if dependencies.contains_aggregate || dependencies.contains_window {
                self.error(
                    VerificationLocation::Value(*group),
                    "aggregate group keys cannot contain aggregate or window calls",
                );
            }
        }

        let group_set: HashSet<ValueId> = group_values.iter().copied().collect();
        for output_value in output_values {
            let mut memo = HashMap::new();
            let dependencies =
                self.aggregate_dependencies(block_id, *output_value, &group_set, &mut memo);
            if dependencies.contains_window {
                self.error(
                    VerificationLocation::Value(*output_value),
                    "aggregate outputs cannot contain window calls",
                );
            }
            if dependencies.unaggregated_row {
                self.error(
                    VerificationLocation::Value(*output_value),
                    "aggregate outputs may reference rows only through explicit group keys or aggregate calls",
                );
            }
        }
    }

    fn verify_join_region(&mut self, operation_id: OperationId, left: SchemaId, right: SchemaId) {
        // A join predicate is the same row-lambda convention with left fields
        // followed by right fields, and exactly one Boolean yield.
        let Some(operation) = self.module.operation(operation_id) else {
            return;
        };
        let Some(region_id) = operation.regions().first().copied() else {
            return;
        };
        let Some(region) = self.module.region(region_id) else {
            return;
        };
        if region.blocks().len() != 1 {
            self.error(
                VerificationLocation::Region(region_id),
                "join condition region must contain exactly one block",
            );
            return;
        }
        let block_id = region.blocks()[0];
        let Some(block) = self.module.block(block_id) else {
            return;
        };
        let mut expected = self
            .module
            .schema(left)
            .map(super::Schema::field_types)
            .unwrap_or_default();
        expected.extend(
            self.module
                .schema(right)
                .map(super::Schema::field_types)
                .unwrap_or_default(),
        );
        let actual: Vec<Type> = block
            .arguments()
            .iter()
            .filter_map(|argument| self.module.value(*argument).map(|value| value.ty().clone()))
            .collect();
        if actual != expected {
            self.error(
                VerificationLocation::Block(block_id),
                "join region arguments must concatenate left and right row fields",
            );
        }
        self.verify_yield(block_id, YieldExpectation::Boolean);
    }

    fn verify_join_result_schema(&mut self, operation_id: OperationId, kind: super::JoinKind) {
        let (Some(left_id), Some(right_id), Some(output_id)) = (
            self.operand_schema(operation_id, 0),
            self.operand_schema(operation_id, 1),
            self.result_schema(operation_id, 0),
        ) else {
            return;
        };
        let (Some(left), Some(right), Some(output)) = (
            self.module.schema(left_id),
            self.module.schema(right_id),
            self.module.schema(output_id),
        ) else {
            return;
        };

        let left_types = left.field_types();
        let right_types = right.field_types();
        let nullable = |types: &[Type]| -> Option<Vec<Type>> {
            types
                .iter()
                .map(|ty| match ty {
                    Type::Scalar(scalar) => Some(Type::Scalar(scalar.with_nullability(true))),
                    Type::Tuple(_) | Type::Relation(_) | Type::Unit => None,
                })
                .collect()
        };
        let expected = match kind {
            super::JoinKind::Inner | super::JoinKind::Cross => {
                let mut expected = left_types;
                expected.extend(right_types);
                Some(expected)
            }
            super::JoinKind::Left => nullable(&right_types).map(|right| {
                let mut expected = left_types;
                expected.extend(right);
                expected
            }),
            super::JoinKind::Right => nullable(&left_types).map(|mut left| {
                left.extend(right_types);
                left
            }),
            super::JoinKind::Full => nullable(&left_types).and_then(|mut left| {
                nullable(&right_types).map(|right| {
                    left.extend(right);
                    left
                })
            }),
            super::JoinKind::Semi | super::JoinKind::Anti => Some(left_types),
        };
        let Some(expected) = expected else {
            self.error(
                VerificationLocation::Operation(operation_id),
                "outer joins can null-extend only scalar row fields",
            );
            return;
        };
        if output.field_types() != expected {
            self.error(
                VerificationLocation::Operation(operation_id),
                "join result fields must positionally match its null-extended input rows",
            );
        }
    }

    fn verify_yield(&mut self, block_id: BlockId, expectation: YieldExpectation) {
        let Some(block) = self.module.block(block_id) else {
            return;
        };
        let Some(terminator_id) = block.operations().last().copied() else {
            return;
        };
        let Some(terminator) = self.module.operation(terminator_id) else {
            return;
        };
        if terminator.kind() != &OperationKind::Terminator(TerminatorOp::Yield) {
            self.error(
                VerificationLocation::Operation(terminator_id),
                "logical expression region must end in yield",
            );
            return;
        }
        let yielded: Vec<Type> = terminator
            .operands()
            .iter()
            .filter_map(|value| self.module.value(*value).map(|stored| stored.ty().clone()))
            .collect();
        match expectation {
            YieldExpectation::Boolean
                if yielded.len() != 1
                    || yielded[0]
                        .as_scalar()
                        .is_none_or(|scalar| !scalar.is_boolean()) =>
            {
                self.error(
                    VerificationLocation::Operation(terminator_id),
                    "region must yield exactly one boolean predicate",
                );
            }
            YieldExpectation::Schema(schema) => {
                let expected = self
                    .module
                    .schema(schema)
                    .map(super::Schema::field_types)
                    .unwrap_or_default();
                if yielded != expected {
                    self.error(
                        VerificationLocation::Operation(terminator_id),
                        "yielded values must match the output relation schema",
                    );
                }
            }
            YieldExpectation::Count(count) if yielded.len() != count => self.error(
                VerificationLocation::Operation(terminator_id),
                format!("region must yield exactly {count} values"),
            ),
            YieldExpectation::Boolean | YieldExpectation::Count(_) => {}
        }
    }

    fn operand_type(&self, operation: OperationId, index: usize) -> Option<&Type> {
        self.module
            .operation(operation)?
            .operands()
            .get(index)
            .and_then(|value| self.module.value(*value))
            .map(super::Value::ty)
    }

    fn result_type(&self, operation: OperationId, index: usize) -> Option<&Type> {
        self.module
            .operation(operation)?
            .results()
            .get(index)
            .and_then(|value| self.module.value(*value))
            .map(super::Value::ty)
    }

    fn operand_schema(&self, operation: OperationId, index: usize) -> Option<SchemaId> {
        self.operand_type(operation, index)
            .and_then(Type::as_relation)
    }

    fn result_schema(&self, operation: OperationId, index: usize) -> Option<SchemaId> {
        self.result_type(operation, index)
            .and_then(Type::as_relation)
    }

    fn operand_is_boolean(&self, operation: OperationId, index: usize) -> bool {
        self.operand_type(operation, index)
            .and_then(Type::as_scalar)
            .is_some_and(super::ScalarType::is_boolean)
    }

    fn result_is_boolean(&self, operation: OperationId, index: usize) -> bool {
        self.result_type(operation, index)
            .and_then(Type::as_scalar)
            .is_some_and(super::ScalarType::is_boolean)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AggregateDependencies {
    contains_aggregate: bool,
    contains_window: bool,
    unaggregated_row: bool,
}

impl AggregateDependencies {
    const fn union(self, other: Self) -> Self {
        Self {
            contains_aggregate: self.contains_aggregate || other.contains_aggregate,
            contains_window: self.contains_window || other.contains_window,
            unaggregated_row: self.unaggregated_row || other.unaggregated_row,
        }
    }
}

#[derive(Clone, Copy)]
enum YieldExpectation {
    Boolean,
    Schema(SchemaId),
    Count(usize),
}

const fn window_bound_rank(bound: WindowFrameBound) -> u8 {
    match bound {
        WindowFrameBound::UnboundedPreceding => 0,
        WindowFrameBound::Preceding => 1,
        WindowFrameBound::CurrentRow => 2,
        WindowFrameBound::Following => 3,
        WindowFrameBound::UnboundedFollowing => 4,
    }
}

fn binary_returns_boolean(operator: BinaryOperator) -> bool {
    matches!(
        operator,
        BinaryOperator::Equal
            | BinaryOperator::NotEqual
            | BinaryOperator::LessThan
            | BinaryOperator::LessThanOrEqual
            | BinaryOperator::GreaterThan
            | BinaryOperator::GreaterThanOrEqual
            | BinaryOperator::And
            | BinaryOperator::Or
            | BinaryOperator::Like
            | BinaryOperator::CaseInsensitiveLike
            | BinaryOperator::IsDistinctFrom
            | BinaryOperator::InArray
    )
}

fn literal_matches_type(literal: &Literal, ty: &Type) -> bool {
    let Some(scalar) = ty.as_scalar() else {
        return false;
    };
    match literal {
        Literal::Null => scalar.is_nullable(),
        Literal::Boolean(_) => matches!(scalar.kind(), SqlType::Boolean),
        Literal::Integer(_) => matches!(
            scalar.kind(),
            SqlType::Integer { signed: true, .. } | SqlType::Decimal { .. }
        ),
        Literal::Unsigned(_) => matches!(
            scalar.kind(),
            SqlType::Integer { signed: false, .. } | SqlType::Decimal { .. }
        ),
        Literal::Float(_) => matches!(scalar.kind(), SqlType::Float { .. }),
        Literal::Decimal { .. } => matches!(scalar.kind(), SqlType::Decimal { .. }),
        Literal::String(_) => matches!(scalar.kind(), SqlType::Utf8),
        Literal::Bytes(_) => matches!(scalar.kind(), SqlType::Binary),
        Literal::Date(_) => matches!(scalar.kind(), SqlType::Date),
        Literal::Time(_) => matches!(scalar.kind(), SqlType::Time { .. }),
        Literal::Timestamp(_) => matches!(scalar.kind(), SqlType::Timestamp { .. }),
        Literal::Interval { .. } => matches!(scalar.kind(), SqlType::Interval),
        Literal::Uuid(_) => matches!(scalar.kind(), SqlType::Uuid),
        Literal::Json(_) => matches!(scalar.kind(), SqlType::Json),
    }
}

/// Rejects impossible decimal declarations while admitting the
/// unconstrained form.
///
/// Precision zero with scale zero denotes a decimal with no declared
/// precision — arbitrary exact numerics, as SQL's bare `NUMERIC` — so only
/// a zero precision paired with a nonzero scale, or a scale wider than a
/// declared precision, is an error.
const fn invalid_decimal(precision: u16, scale: i16) -> bool {
    if precision == 0 {
        return scale != 0;
    }
    scale.unsigned_abs() > precision
}

fn scalar_type_error(ty: &SqlType) -> Option<String> {
    match ty {
        SqlType::Integer { bits, .. } if !matches!(bits, 8 | 16 | 32 | 64 | 128) => {
            Some(format!("integer width {bits} is unsupported"))
        }
        SqlType::Float { bits } if !matches!(bits, 16 | 32 | 64 | 128) => {
            Some(format!("float width {bits} is unsupported"))
        }
        SqlType::Decimal { precision, scale } if invalid_decimal(*precision, *scale) => {
            Some("decimal precision must be positive and cover the scale".into())
        }
        SqlType::Time { precision } | SqlType::Timestamp { precision, .. } if *precision > 9 => {
            Some("time precision must be at most 9".into())
        }
        SqlType::Custom(name) if name.is_empty() => {
            Some("custom scalar type name must not be empty".into())
        }
        SqlType::Array { element } => scalar_type_error(element),
        SqlType::Boolean
        | SqlType::Integer { .. }
        | SqlType::Float { .. }
        | SqlType::Decimal { .. }
        | SqlType::Utf8
        | SqlType::Binary
        | SqlType::Date
        | SqlType::Time { .. }
        | SqlType::Timestamp { .. }
        | SqlType::Interval
        | SqlType::Uuid
        | SqlType::Json
        | SqlType::Custom(_) => None,
    }
}

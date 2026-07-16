use super::{Module, OperationId, RegionId};

/// Traversal order for recursive operation walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WalkOrder {
    /// Visits an operation before operations in its nested regions.
    PreOrder,
    /// Visits nested regions before their owning operation.
    PostOrder,
}

/// Walks every operation reachable through one region hierarchy.
///
/// Returns `false` when the root region handle is stale. Missing nested entities
/// are skipped so diagnostic tooling can still inspect partially malformed IR;
/// callers that require valid structure should run [`crate::ir::verify_module`].
pub fn walk_operations(
    module: &Module,
    region: RegionId,
    order: WalkOrder,
    mut visitor: impl FnMut(OperationId),
) -> bool {
    walk_region(module, region, order, &mut visitor)
}

/// Captures an operation-id worklist before mutation begins.
///
/// The returned ids are generational. If an edit erases a collected operation,
/// subsequent lookup fails instead of accidentally addressing a reused arena slot.
/// Returns `None` when the supplied region handle is stale.
#[must_use]
pub fn collect_operations(
    module: &Module,
    region: RegionId,
    order: WalkOrder,
) -> Option<Vec<OperationId>> {
    let mut operations = Vec::new();
    walk_operations(module, region, order, |operation| {
        operations.push(operation)
    })
    .then_some(operations)
}

fn walk_region(
    module: &Module,
    region_id: RegionId,
    order: WalkOrder,
    visitor: &mut impl FnMut(OperationId),
) -> bool {
    let Some(region) = module.region(region_id) else {
        return false;
    };
    for block_id in region.blocks() {
        let Some(block) = module.block(*block_id) else {
            continue;
        };
        for operation_id in block.operations() {
            let Some(operation) = module.operation(*operation_id) else {
                continue;
            };
            if order == WalkOrder::PreOrder {
                visitor(*operation_id);
            }
            for nested_region in operation.regions() {
                walk_region(module, *nested_region, order, visitor);
            }
            if order == WalkOrder::PostOrder {
                visitor(*operation_id);
            }
        }
    }
    true
}

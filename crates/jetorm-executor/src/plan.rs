use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use afterburner::ir::{Module, StructuralFingerprint, structural_fingerprint};
use jetorm_dialect::{Dialect, Postgres, Statement};

use crate::error::ExecuteError;

/// Cache of rendered statements keyed by IR structural fingerprint.
///
/// Queries that differ only in bound values lower to structurally identical
/// IR and therefore share one fingerprint, so SQL rendering — and, once the
/// optimizer lands, its passes — run once per query shape instead of once
/// per execution. The cache is shared by a [`crate::Database`] and every
/// transaction started from it.
#[derive(Debug, Default)]
pub struct PlanCache {
    statements: Mutex<HashMap<StructuralFingerprint, Arc<Statement>>>,
}

impl PlanCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Returns the cached statement for a verified module, rendering on miss.
    pub(crate) fn statement(&self, module: &Module) -> Result<Arc<Statement>, ExecuteError> {
        let fingerprint = structural_fingerprint(module)?;
        if let Some(statement) = self
            .statements
            .lock()
            .expect("plan cache lock is never poisoned")
            .get(&fingerprint)
        {
            return Ok(Arc::clone(statement));
        }
        // Rendering happens outside the lock; a concurrent miss renders the
        // same statement twice and the entries are equal, which is harmless.
        let statement = Arc::new(Postgres.render_query(module)?);
        self.statements
            .lock()
            .expect("plan cache lock is never poisoned")
            .insert(fingerprint, Arc::clone(&statement));
        Ok(statement)
    }
}

#[cfg(test)]
mod tests {
    use afterburner::afterburner;
    use jetorm_entity::{Column, ColumnMeta, ColumnType, DecodeError, Entity, Model, TableMeta};
    use jetorm_query::{ColumnExt, EntityQuery};

    use super::PlanCache;

    #[derive(Clone, Copy, Debug)]
    struct ItemEntity;

    #[derive(Clone, Debug)]
    struct Item;

    impl Entity for ItemEntity {
        type Model = Item;
        const TABLE: TableMeta = TableMeta::new("items");
        const COLUMNS: &'static [ColumnMeta] =
            &[ColumnMeta::new("id", "id", ColumnType::Int64).primary_key()];
        const PRIMARY_KEY: &'static [usize] = &[0];
    }

    impl Model for Item {
        type Entity = ItemEntity;

        fn into_values(self) -> Vec<jetorm_entity::Value> {
            Vec::new()
        }

        fn from_values(_values: Vec<jetorm_entity::Value>) -> Result<Self, DecodeError> {
            Ok(Self)
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct Id;

    impl Column for Id {
        type Entity = ItemEntity;
        type Rust = i64;
        const INDEX: usize = 0;
        const NULLABLE: bool = false;
    }

    #[test]
    fn identical_shapes_share_one_rendered_statement() {
        let cache = PlanCache::new();
        let first = afterburner!(ItemEntity::find().filter(Id.gt(1)).limit(10))
            .expect("first query lowers");
        let second = afterburner!(ItemEntity::find().filter(Id.gt(999)).limit(10))
            .expect("second query lowers");

        let first_statement = cache.statement(&first).expect("first render succeeds");
        let second_statement = cache.statement(&second).expect("second lookup succeeds");
        assert!(
            std::sync::Arc::ptr_eq(&first_statement, &second_statement),
            "same query shape must hit the cached statement"
        );
    }

    #[test]
    fn different_shapes_render_distinct_statements() {
        let cache = PlanCache::new();
        let filtered =
            afterburner!(ItemEntity::find().filter(Id.gt(1))).expect("filtered query lowers");
        let bare = afterburner!(ItemEntity::find()).expect("bare query lowers");

        let filtered_statement = cache
            .statement(&filtered)
            .expect("filtered render succeeds");
        let bare_statement = cache.statement(&bare).expect("bare render succeeds");
        assert_ne!(filtered_statement.sql(), bare_statement.sql());
    }
}

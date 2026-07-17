use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use afterburner::IntoAfterBurnerIr;
use jetorm_dialect::{Dialect, Postgres, Statement};
use jetorm_entity::Entity;
use jetorm_query::{QueryShape, Select};

use crate::error::ExecuteError;

/// Cache of rendered statements keyed by value-independent query shape.
///
/// Queries that differ only in bound values share one [`QueryShape`], so a
/// long-lived process pays for lowering, verification, and SQL rendering once
/// per query shape rather than once per execution. The cache is shared by a
/// [`crate::Database`] and every transaction started from it.
///
/// Keying on the shape rather than on the lowered module's structural
/// fingerprint is what makes a hit cheap: the shape is read straight off the
/// builder, so a hit skips IR construction entirely. The trade-off is that
/// two differently built queries that would lower to identical IR occupy two
/// entries; the IR fingerprint remains the right identity for profile-guided
/// optimization, which must survive optimizer rewrites.
#[derive(Debug, Default)]
pub struct PlanCache {
    statements: Mutex<HashMap<QueryShape, Arc<Statement>>>,
}

impl PlanCache {
    /// Creates an empty cache.
    ///
    /// [`crate::Database`] owns one internally; standalone construction is
    /// for tooling and benchmarks that drive the pipeline without a
    /// connection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the statement for one query, rendering it on a cache miss.
    ///
    /// The query is only lowered and verified when its shape is not already
    /// known.
    ///
    /// # Errors
    ///
    /// Returns an error when the query cannot be lowered, fails IR
    /// verification, or cannot be rendered as SQL.
    pub fn statement<E>(&self, query: &Select<E>) -> Result<Arc<Statement>, ExecuteError>
    where
        E: Entity,
    {
        let shape = query.shape();
        if let Some(statement) = self.lock().get(&shape) {
            return Ok(Arc::clone(statement));
        }

        // Render outside the lock. A concurrent miss renders the same shape
        // twice and the entries are equal, which is harmless.
        let statement = Arc::new(Self::render(query)?);
        self.lock().insert(shape, Arc::clone(&statement));
        Ok(statement)
    }

    /// Lowers, verifies, and renders one query without consulting the cache.
    fn render<E>(query: &Select<E>) -> Result<Statement, ExecuteError>
    where
        E: Entity,
    {
        // Cloning the builder keeps `statement` a read-only view of the
        // caller's query; the clone is a small AST plus its bind table, and
        // it only happens on a miss.
        let module = query
            .clone()
            .into_afterburner_ir()
            .map_err(ExecuteError::lowering)?;
        // `render_query` verifies the module, so lowering deliberately does
        // not verify it a second time.
        Ok(Postgres.render_query(&module)?)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<QueryShape, Arc<Statement>>> {
        self.statements
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
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
        let first = cache
            .statement(&ItemEntity::find().filter(Id.gt(1)).limit(10))
            .expect("first render succeeds");
        let second = cache
            .statement(&ItemEntity::find().filter(Id.gt(999)).limit(10))
            .expect("second lookup succeeds");
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "same query shape must hit the cached statement"
        );
    }

    #[test]
    fn different_shapes_render_distinct_statements() {
        let cache = PlanCache::new();
        let filtered = cache
            .statement(&ItemEntity::find().filter(Id.gt(1)))
            .expect("filtered render succeeds");
        let bare = cache
            .statement(&ItemEntity::find())
            .expect("bare render succeeds");
        assert_ne!(filtered.sql(), bare.sql());
    }

    #[test]
    fn structural_facts_partition_the_cache_and_bound_values_do_not() {
        // Everything the frontend lowers structurally must key a distinct
        // entry, or a cache hit would return SQL for a different query.
        // Bound values — including row counts — must NOT partition it, or a
        // paginated walk would mint one entry per page.
        let cache = PlanCache::new();
        let base = cache
            .statement(&ItemEntity::find().limit(10))
            .expect("base renders");
        let other_limit = cache
            .statement(&ItemEntity::find().limit(11))
            .expect("other limit renders");
        assert!(
            std::sync::Arc::ptr_eq(&base, &other_limit),
            "row counts are bound values; every page must share one statement"
        );

        let with_offset = cache
            .statement(&ItemEntity::find().limit(10).offset(5))
            .expect("offset renders");
        let distinct = cache
            .statement(&ItemEntity::find().limit(10).distinct())
            .expect("distinct renders");
        let ascending = cache
            .statement(&ItemEntity::find().limit(10).order_by(Id.asc()))
            .expect("ascending renders");
        let descending = cache
            .statement(&ItemEntity::find().limit(10).order_by(Id.desc()))
            .expect("descending renders");
        let nulls_first = cache
            .statement(
                &ItemEntity::find()
                    .limit(10)
                    .order_by(Id.asc().nulls_first()),
            )
            .expect("null order renders");

        for (label, other) in [
            ("offset presence", &with_offset),
            ("distinct", &distinct),
            ("order", &ascending),
            ("null order", &nulls_first),
        ] {
            assert_ne!(
                base.sql(),
                other.sql(),
                "{label} must produce a distinct statement"
            );
        }
        assert_ne!(
            ascending.sql(),
            descending.sql(),
            "sort direction must produce a distinct statement"
        );
    }

    #[test]
    fn a_cache_hit_reuses_the_statement_for_new_bind_values() {
        let cache = PlanCache::new();
        let first = ItemEntity::find().filter(Id.eq(1));
        let second = ItemEntity::find().filter(Id.eq(2));
        assert_eq!(first.shape(), second.shape());

        let statement = cache.statement(&second).expect("second query resolves");
        // The cached statement is parameterized, so the differing values ride
        // in the bind table rather than in the SQL text.
        assert!(statement.sql().contains("$1"));
        assert_eq!(statement.bind_order(), [0]);
        assert_eq!(first.binds().len(), 1);
        assert_eq!(second.binds().len(), 1);
    }
}

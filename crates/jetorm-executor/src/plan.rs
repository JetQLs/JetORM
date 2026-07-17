use std::sync::Arc;

use afterburner::IntoAfterBurnerIr;
use jetorm_dialect::{Dialect, Postgres, Statement};
use jetorm_entity::Entity;
use jetorm_query::{QueryShape, Select};

use crate::error::ExecuteError;

/// Number of statements retained before the least-recently-used are evicted.
///
/// Statements are a few hundred bytes each, so the default bound costs
/// megabytes at most while making the cache immune to unbounded growth from
/// dynamically generated query shapes.
const DEFAULT_CAPACITY: u64 = 10_000;

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
///
/// The cache is safe to hammer from many threads: storage is sharded rather
/// than guarded by one lock, entries are bounded with least-recently-used
/// eviction, and every query execution touches it exactly once.
#[derive(Debug)]
pub struct PlanCache {
    statements: moka::sync::Cache<QueryShape, Arc<Statement>>,
}

impl PlanCache {
    /// Creates an empty cache with the default capacity.
    ///
    /// [`crate::Database`] owns one internally; standalone construction is
    /// for tooling and benchmarks that drive the pipeline without a
    /// connection.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// Creates an empty cache retaining at most `capacity` statements.
    #[must_use]
    pub fn with_capacity(capacity: u64) -> Self {
        Self {
            statements: moka::sync::Cache::builder().max_capacity(capacity).build(),
        }
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
        if let Some(statement) = self.statements.get(&shape) {
            return Ok(statement);
        }

        // Concurrent misses of one shape may render it more than once; the
        // renders are equal and the last insert wins, which is harmless and
        // cheaper than holding a rendering slot across the cache.
        let statement = Arc::new(Self::render(query)?);
        self.statements.insert(shape, Arc::clone(&statement));
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
}

impl Default for PlanCache {
    fn default() -> Self {
        Self::new()
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
        type Field = i64;
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

    #[test]
    fn capacity_bounds_the_cache_with_lru_eviction() {
        // A dynamically generated stream of shapes must not grow the cache
        // without limit; the least recently used statements go first. Each
        // predicate depth is a structurally distinct shape — bound values
        // deliberately cannot create new shapes.
        let cache = PlanCache::with_capacity(2);
        for depth in 1..=16 {
            let mut query = ItemEntity::find();
            for _ in 0..depth {
                query = query.filter(Id.gt(0));
            }
            cache.statement(&query).expect("statement renders");
        }
        cache.statements.run_pending_tasks();
        assert!(
            cache.statements.entry_count() <= 2,
            "cache exceeded its capacity: {} entries",
            cache.statements.entry_count()
        );
    }

    /// Diagnostic for hit-path scaling under thread contention.
    ///
    /// The original `Mutex<HashMap>` cache degraded ~4x per-op at 8 threads
    /// because read-only hits serialized on one lock. Sharded storage must
    /// keep per-op cost roughly flat; the bound here is deliberately loose
    /// so scheduler noise cannot flake it. Run explicitly:
    /// `cargo test -p jetorm-executor plan_cache_scaling -- --ignored --nocapture`
    #[test]
    #[ignore = "timing diagnostic; run explicitly with --nocapture"]
    fn plan_cache_scaling_diagnostic() {
        fn per_op_nanos(cache: &std::sync::Arc<PlanCache>, threads: usize) -> f64 {
            const OPS: usize = 50_000;
            let started = std::time::Instant::now();
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let cache = std::sync::Arc::clone(cache);
                    std::thread::spawn(move || {
                        for value in 0..OPS {
                            let statement = cache
                                .statement(&ItemEntity::find().filter(Id.gt(value as i64)))
                                .expect("hit resolves");
                            std::hint::black_box(statement);
                        }
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("worker thread completes");
            }
            started.elapsed().as_nanos() as f64 / (threads * OPS) as f64
        }

        let cache = std::sync::Arc::new(PlanCache::new());
        cache
            .statement(&ItemEntity::find().filter(Id.gt(0)))
            .expect("seed renders");

        let single = per_op_nanos(&cache, 1);
        let eight = per_op_nanos(&cache, 8);
        eprintln!("plan cache hit: 1 thread {single:.0} ns/op, 8 threads {eight:.0} ns/op");
        assert!(
            eight < single * 3.0,
            "hit path degraded {}x under 8 threads; reads are serializing",
            eight / single
        );
    }

    #[test]
    fn concurrent_hits_share_one_statement() {
        // Executions from many threads must resolve one shape to one
        // statement without corrupting the cache or serializing incorrectly.
        let cache = std::sync::Arc::new(PlanCache::new());
        let seed = cache
            .statement(&ItemEntity::find().filter(Id.gt(0)))
            .expect("seed renders");

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let cache = std::sync::Arc::clone(&cache);
                let seed = std::sync::Arc::clone(&seed);
                std::thread::spawn(move || {
                    for value in 0..1_000 {
                        let statement = cache
                            .statement(&ItemEntity::find().filter(Id.gt(value)))
                            .expect("hit resolves");
                        assert!(
                            std::sync::Arc::ptr_eq(&statement, &seed),
                            "every thread must observe the one cached statement"
                        );
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("worker thread completes");
        }
    }
}

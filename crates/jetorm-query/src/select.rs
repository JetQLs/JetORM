use std::any::TypeId;
use std::hash::{BuildHasher, Hash, Hasher, RandomState};
use std::sync::{Arc, LazyLock};
use std::{fmt, marker::PhantomData};

use afterburner::ir::BinaryOperator;
use jetorm_entity::{Column, Entity, SingleKeyEntity, Value};

use crate::expr::{Expr, OrderKey, Predicate, SortKeySpec, normalize};

/// Value-independent identity of a query.
///
/// Two queries share a shape exactly when they lower to the same IR, so a
/// shape is a sound key for caching anything derived from that IR — rendered
/// SQL above all. Bound values are excluded by construction: they live in the
/// query's bind table, never in its expression tree.
///
/// This is the frontend counterpart to
/// [`afterburner::ir::structural_fingerprint`]. A shape is cheaper — it is
/// read straight off the builder, with no lowering, verification, or module
/// walk — but it only recognizes queries that were *built* alike. The IR
/// fingerprint additionally recognizes differently built queries that lower
/// or optimize to identical IR.
/// Reading a shape is cheap by design: the predicate tree is shared through
/// an [`Arc`] rather than cloned, and the hash over the whole shape is
/// computed once at construction. Plan caches probe with that precomputed
/// hash and fall back to deep equality only on a hash match, so a cache hit
/// never walks the tree twice.
#[derive(Clone, Debug)]
pub struct QueryShape {
    /// Hash over every field below, fixed at construction.
    hash: u64,
    // The entity fixes the table identity and column metadata that lowering
    // reads, so equal shapes over equal entities lower identically.
    entity: TypeId,
    filter: Option<Arc<Predicate>>,
    order: Vec<SortKeySpec>,
    // Row counts are bound values, so only their presence is structural:
    // every page of a paginated query shares one shape.
    has_offset: bool,
    has_fetch: bool,
    distinct: bool,
    projection: Option<Vec<usize>>,
    /// Whether the query collapses to a single `count(*)` row. A count and a
    /// select over the same builder must never share a cached statement.
    count: bool,
}

impl PartialEq for QueryShape {
    fn eq(&self, other: &Self) -> bool {
        // The precomputed hash rejects almost every mismatch before any
        // tree walk; equal hashes still require real equality, because a
        // colliding shape returning another query's statement would execute
        // the wrong SQL.
        self.hash == other.hash
            && self.entity == other.entity
            && self.order == other.order
            && self.has_offset == other.has_offset
            && self.has_fetch == other.has_fetch
            && self.distinct == other.distinct
            && self.projection == other.projection
            && self.count == other.count
            && match (&self.filter, &other.filter) {
                (None, None) => true,
                (Some(left), Some(right)) => Arc::ptr_eq(left, right) || left == right,
                _ => false,
            }
    }
}

impl Eq for QueryShape {}

impl Hash for QueryShape {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

/// Process-wide hash seed shared by every shape, which is exactly a plan
/// cache's scope.
fn shape_seed() -> &'static RandomState {
    static SEED: LazyLock<RandomState> = LazyLock::new(RandomState::new);
    &SEED
}

/// Typed `SELECT` builder over one entity.
///
/// The builder stores a backend-independent logical AST plus a positional
/// bind table; it constructs no IR and renders no SQL itself. Lowering into
/// AfterBurner IR happens through the builder's
/// [`afterburner::IntoAfterBurnerIr`] implementation, typically via the
/// [`afterburner::afterburner!`] entry point.
///
/// Predicates and ordering keys are bound to `E` at the type level, so a
/// column marker belonging to another entity is a compile error rather than
/// a query that silently reads the wrong column.
#[derive(Clone)]
pub struct Select<E>
where
    E: Entity,
{
    pub(crate) filter: Option<Arc<Predicate>>,
    pub(crate) binds: Vec<Value>,
    pub(crate) order: Vec<SortKeySpec>,
    pub(crate) offset: Option<u64>,
    pub(crate) fetch: Option<u64>,
    pub(crate) distinct: bool,
    /// Column positions to project, in output order; `None` fetches the
    /// full row. Set through [`Select::select`].
    pub(crate) projection: Option<Vec<usize>>,
    entity: PhantomData<fn() -> E>,
}

impl<E> Select<E>
where
    E: Entity,
{
    /// Creates a select over every row and column of the entity.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            filter: None,
            binds: Vec::new(),
            order: Vec::new(),
            offset: None,
            fetch: None,
            distinct: false,
            projection: None,
            entity: PhantomData,
        }
    }

    /// Restricts rows to those satisfying the predicate.
    ///
    /// Successive calls combine with SQL `AND`. Values captured by the
    /// predicate are appended to the positional bind table in expression
    /// pre-order, left operands before right operands.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<E, bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.binds);
        self.filter = Some(Arc::new(match self.filter.take() {
            Some(existing) => Predicate::Binary {
                // The builder usually holds the only reference, so combining
                // moves the existing tree; a shape taken earlier keeps its
                // own copy alive and forces one clone here instead.
                left: Box::new(
                    Arc::try_unwrap(existing).unwrap_or_else(|shared| (*shared).clone()),
                ),
                op: BinaryOperator::And,
                right: Box::new(normalized),
            },
            None => normalized,
        }));
        self
    }

    /// Appends one ordering key; earlier keys take precedence.
    #[must_use]
    pub fn order_by(mut self, key: OrderKey<E>) -> Self {
        self.order.push(key.spec);
        self
    }

    /// Restricts the number of emitted rows.
    #[must_use]
    pub const fn limit(mut self, fetch: u64) -> Self {
        self.fetch = Some(fetch);
        self
    }

    /// Skips rows before emission begins.
    #[must_use]
    pub const fn offset(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// Removes duplicate rows from the result.
    #[must_use]
    pub const fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    /// Returns captured values in positional bind order.
    ///
    /// Position `n` in this vector corresponds to the IR parameter with bind
    /// position `n` produced by lowering: predicate values in capture order,
    /// then the offset, then the row limit. Executors can therefore bind
    /// values without re-walking the query.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        let mut binds = self.binds.clone();
        Self::push_count_binds(&mut binds, self.offset, self.fetch);
        binds
    }

    /// Consumes the query and returns its captured values in bind order.
    ///
    /// Executors use this to hand values to the driver without copying the
    /// predicate values a second time.
    #[must_use]
    pub fn into_binds(self) -> Vec<Value> {
        let mut binds = self.binds;
        Self::push_count_binds(&mut binds, self.offset, self.fetch);
        binds
    }

    fn push_count_binds(binds: &mut Vec<Value>, offset: Option<u64>, fetch: Option<u64>) {
        for count in [offset, fetch].into_iter().flatten() {
            // Row counts beyond i64::MAX have no meaning to the database;
            // saturating keeps the builder API infallible.
            binds.push(Value::Int64(i64::try_from(count).unwrap_or(i64::MAX)));
        }
    }

    /// Returns the projected column positions, when a projection is set.
    #[must_use]
    pub fn projection(&self) -> Option<&[usize]> {
        self.projection.as_deref()
    }

    /// Returns this query's value-independent shape.
    ///
    /// Reading a shape shares the predicate tree instead of cloning it and
    /// never lowers the query, so callers can resolve a cached plan before
    /// paying for IR construction.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        let entity = TypeId::of::<E>();
        let has_offset = self.offset.is_some();
        let has_fetch = self.fetch.is_some();

        // One walk at construction; every later probe reuses the digest.
        let mut hasher = shape_seed().build_hasher();
        entity.hash(&mut hasher);
        self.filter.hash(&mut hasher);
        self.order.hash(&mut hasher);
        (has_offset, has_fetch, self.distinct, false).hash(&mut hasher);
        self.projection.hash(&mut hasher);

        QueryShape {
            hash: hasher.finish(),
            entity,
            filter: self.filter.clone(),
            order: self.order.clone(),
            has_offset,
            has_fetch,
            distinct: self.distinct,
            projection: self.projection.clone(),
            count: false,
        }
    }

    /// Converts the query into a row count over the same rows.
    ///
    /// The count sees exactly the rows this select would return: the filter,
    /// `DISTINCT`, and any row limit or offset carry over. Ordering does not —
    /// no ordering can change how many rows there are — so counts differing
    /// only in `order_by` share one statement.
    #[must_use]
    pub fn into_count(self) -> CountQuery<E> {
        CountQuery {
            filter: self.filter,
            binds: self.binds,
            offset: self.offset,
            fetch: self.fetch,
            distinct: self.distinct,
            entity: PhantomData,
        }
    }
}

/// Typed `SELECT count(*)` over the rows a [`Select`] would return.
///
/// Built through [`Select::into_count`]; executors expose it as a `count`
/// method on the select itself. The query keeps the source's bind table, so
/// a filtered count binds its values exactly like the filtered select.
#[derive(Clone)]
pub struct CountQuery<E>
where
    E: Entity,
{
    pub(crate) filter: Option<Arc<Predicate>>,
    pub(crate) binds: Vec<Value>,
    pub(crate) offset: Option<u64>,
    pub(crate) fetch: Option<u64>,
    pub(crate) distinct: bool,
    entity: PhantomData<fn() -> E>,
}

impl<E> CountQuery<E>
where
    E: Entity,
{
    /// Returns captured values in positional bind order.
    ///
    /// Positions match [`Select::binds`] for the source query: predicate
    /// values in capture order, then the offset, then the row limit.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        let mut binds = self.binds.clone();
        Select::<E>::push_count_binds(&mut binds, self.offset, self.fetch);
        binds
    }

    /// Consumes the query and returns its captured values in bind order.
    #[must_use]
    pub fn into_binds(self) -> Vec<Value> {
        let mut binds = self.binds;
        Select::<E>::push_count_binds(&mut binds, self.offset, self.fetch);
        binds
    }

    /// Returns this query's value-independent shape.
    ///
    /// A count never shares a shape with a select — the two lower to
    /// different IR — so the shape carries the aggregate as a structural
    /// fact alongside the source query's own identity.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        let entity = TypeId::of::<E>();
        let has_offset = self.offset.is_some();
        let has_fetch = self.fetch.is_some();

        let mut hasher = shape_seed().build_hasher();
        entity.hash(&mut hasher);
        self.filter.hash(&mut hasher);
        // A count carries no ordering; hash the same field count as a
        // select so the streams stay aligned.
        Vec::<SortKeySpec>::new().hash(&mut hasher);
        (has_offset, has_fetch, self.distinct, true).hash(&mut hasher);
        None::<Vec<usize>>.hash(&mut hasher);

        QueryShape {
            hash: hasher.finish(),
            entity,
            filter: self.filter.clone(),
            order: Vec::new(),
            has_offset,
            has_fetch,
            distinct: self.distinct,
            projection: None,
            count: true,
        }
    }
}

impl<E> fmt::Debug for CountQuery<E>
where
    E: Entity,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CountQuery")
            .field("table", &E::TABLE.name())
            .field("filter", &self.filter)
            .field("binds", &self.binds)
            .field("offset", &self.offset)
            .field("fetch", &self.fetch)
            .field("distinct", &self.distinct)
            .finish()
    }
}

impl<E> Default for Select<E>
where
    E: Entity,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<E> fmt::Debug for Select<E>
where
    E: Entity,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Select")
            .field("table", &E::TABLE.name())
            .field("filter", &self.filter)
            .field("binds", &self.binds)
            .field("order", &self.order)
            .field("offset", &self.offset)
            .field("fetch", &self.fetch)
            .field("distinct", &self.distinct)
            .field("projection", &self.projection)
            .finish()
    }
}

/// Queries a plan cache can key and lower.
///
/// The contract pairs the two halves a statement cache needs: a
/// value-independent [`QueryShape`] as the key, and IR lowering (through
/// [`afterburner::IntoAfterBurnerIr`] on the clone) to produce the statement
/// on a miss. Both selects and counts satisfy it, so one cache serves every
/// query kind.
pub trait CacheableQuery:
    Clone + afterburner::IntoAfterBurnerIr<Error = crate::LoweringError>
{
    /// Returns the value-independent shape identifying this query.
    fn shape(&self) -> QueryShape;
}

impl<E> CacheableQuery for Select<E>
where
    E: Entity,
{
    fn shape(&self) -> QueryShape {
        Self::shape(self)
    }
}

impl<E> CacheableQuery for CountQuery<E>
where
    E: Entity,
{
    fn shape(&self) -> QueryShape {
        Self::shape(self)
    }
}

/// Query entry points available on every entity marker.
pub trait EntityQuery: Entity {
    /// Starts a typed select over every row and column of this entity.
    #[must_use]
    fn find() -> Select<Self> {
        Select::new()
    }

    /// Starts a select for the row whose primary key equals the value.
    ///
    /// Available on entities with a single-column primary key. The value
    /// converts into the key column's Rust type, so a mismatched type is a
    /// compile error, and it binds as a parameter like every other value.
    #[must_use]
    fn find_by_id(value: impl Into<<Self::PrimaryKeyColumn as Column>::Rust>) -> Select<Self>
    where
        Self: SingleKeyEntity,
    {
        use crate::expr::ColumnExt;
        Self::find().filter(Self::PrimaryKeyColumn::default().eq(value))
    }
}

impl<E> EntityQuery for E where E: Entity {}

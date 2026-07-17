use std::any::TypeId;
use std::{fmt, marker::PhantomData};

use afterburner::ir::BinaryOperator;
use jetorm_entity::{Entity, Value};

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
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QueryShape {
    // The entity fixes the table identity and column metadata that lowering
    // reads, so equal shapes over equal entities lower identically.
    entity: TypeId,
    filter: Option<Predicate>,
    order: Vec<SortKeySpec>,
    // Row counts are bound values, so only their presence is structural:
    // every page of a paginated query shares one shape.
    has_offset: bool,
    has_fetch: bool,
    distinct: bool,
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
    pub(crate) filter: Option<Predicate>,
    pub(crate) binds: Vec<Value>,
    pub(crate) order: Vec<SortKeySpec>,
    pub(crate) offset: Option<u64>,
    pub(crate) fetch: Option<u64>,
    pub(crate) distinct: bool,
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
        self.filter = Some(match self.filter.take() {
            Some(existing) => Predicate::Binary {
                op: BinaryOperator::And,
                left: Box::new(existing),
                right: Box::new(normalized),
            },
            None => normalized,
        });
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

    /// Returns this query's value-independent shape.
    ///
    /// Reading a shape costs one small tree clone and never lowers the query,
    /// so callers can resolve a cached plan before paying for IR construction.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        QueryShape {
            entity: TypeId::of::<E>(),
            filter: self.filter.clone(),
            order: self.order.clone(),
            has_offset: self.offset.is_some(),
            has_fetch: self.fetch.is_some(),
            distinct: self.distinct,
        }
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
            .finish()
    }
}

/// Query entry points available on every entity marker.
pub trait EntityQuery: Entity {
    /// Starts a typed select over every row and column of this entity.
    #[must_use]
    fn find() -> Select<Self> {
        Select::new()
    }
}

impl<E> EntityQuery for E where E: Entity {}

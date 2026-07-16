use std::{fmt, marker::PhantomData};

use afterburner::ir::BinaryOperator;
use jetorm_entity::{Entity, Value};

use crate::expr::{Expr, Node, OrderKey};

/// Typed `SELECT` builder over one entity.
///
/// The builder stores a backend-independent logical AST plus a positional
/// bind table; it constructs no IR and renders no SQL itself. Lowering into
/// AfterBurner IR happens through the builder's
/// [`afterburner::IntoAfterBurnerIr`] implementation, typically via the
/// [`afterburner::afterburner!`] entry point.
#[derive(Clone)]
pub struct Select<E>
where
    E: Entity,
{
    pub(crate) filter: Option<Node>,
    pub(crate) binds: Vec<Value>,
    pub(crate) order: Vec<OrderKey>,
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
    pub fn filter(mut self, predicate: Expr<bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.binds);
        self.filter = Some(match self.filter.take() {
            Some(existing) => Node::Binary {
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
    pub fn order_by(mut self, key: OrderKey) -> Self {
        self.order.push(key);
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
    /// Position `n` in this slice corresponds to the IR parameter with bind
    /// position `n` produced by lowering, so executors can bind values without
    /// re-walking the query.
    #[must_use]
    pub fn binds(&self) -> &[Value] {
        &self.binds
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

/// Replaces captured values with positional binds in expression pre-order.
fn normalize(node: Node, binds: &mut Vec<Value>) -> Node {
    match node {
        Node::Value(value) => {
            let position = binds.len();
            binds.push(value);
            Node::Bind(position)
        }
        Node::Unary { op, operand } => Node::Unary {
            op,
            operand: Box::new(normalize(*operand, binds)),
        },
        Node::Binary { op, left, right } => {
            let left = normalize(*left, binds);
            let right = normalize(*right, binds);
            Node::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
        Node::Column(_) | Node::Bind(_) => node,
    }
}

use std::marker::PhantomData;

use afterburner::ir::{BinaryOperator, NullOrder, SortDirection, UnaryOperator};
use jetorm_entity::{Column, SqlValue, Value};

/// Backend-independent scalar expression node.
///
/// Nodes reference entity columns by position and carry user values inline
/// until [`crate::Select`] normalizes them into positional binds. The node
/// set mirrors the AfterBurner scalar dialect JetORM currently lowers.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    /// Reference to one entity column by position in `Entity::COLUMNS`.
    Column(usize),
    /// Reference to one positional bind assigned during normalization.
    Bind(usize),
    /// User value not yet assigned a bind position.
    Value(Value),
    /// One unary scalar operation.
    Unary {
        op: UnaryOperator,
        operand: Box<Node>,
    },
    /// One binary scalar operation.
    Binary {
        op: BinaryOperator,
        left: Box<Node>,
        right: Box<Node>,
    },
}

/// Typed scalar expression produced by column operators.
///
/// The type parameter tracks the expression's Rust-visible result type, so
/// predicate combinators accept only boolean expressions. Values are captured
/// by the expression and become positional binds when the expression is
/// attached to a query.
#[derive(Debug)]
pub struct Expr<T> {
    pub(crate) node: Node,
    marker: PhantomData<fn() -> T>,
}

impl<T> Expr<T> {
    pub(crate) const fn from_node(node: Node) -> Self {
        Self {
            node,
            marker: PhantomData,
        }
    }
}

impl<T> Clone for Expr<T> {
    fn clone(&self) -> Self {
        Self::from_node(self.node.clone())
    }
}

impl Expr<bool> {
    /// Combines two predicates with SQL three-valued conjunction.
    #[must_use]
    pub fn and(self, other: Self) -> Self {
        Self::from_node(Node::Binary {
            op: BinaryOperator::And,
            left: Box::new(self.node),
            right: Box::new(other.node),
        })
    }

    /// Combines two predicates with SQL three-valued disjunction.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self::from_node(Node::Binary {
            op: BinaryOperator::Or,
            left: Box::new(self.node),
            right: Box::new(other.node),
        })
    }
}

/// Negates a predicate with SQL three-valued logic.
///
/// The standard operator trait keeps both spellings available: `!predicate`
/// and `predicate.not()`.
impl std::ops::Not for Expr<bool> {
    type Output = Self;

    fn not(self) -> Self {
        Self::from_node(Node::Unary {
            op: UnaryOperator::Not,
            operand: Box::new(self.node),
        })
    }
}

/// One `ORDER BY` key over an entity column.
///
/// Created through [`ColumnExt::asc`] and [`ColumnExt::desc`]; null placement
/// defaults to the target database's convention.
#[derive(Clone, Copy, Debug)]
pub struct OrderKey {
    pub(crate) column: usize,
    pub(crate) direction: SortDirection,
    pub(crate) null_order: NullOrder,
}

impl OrderKey {
    const fn new(column: usize, direction: SortDirection) -> Self {
        Self {
            column,
            direction,
            null_order: NullOrder::DialectDefault,
        }
    }

    /// Places SQL `NULL` values before non-null values.
    #[must_use]
    pub const fn nulls_first(mut self) -> Self {
        self.null_order = NullOrder::First;
        self
    }

    /// Places SQL `NULL` values after non-null values.
    #[must_use]
    pub const fn nulls_last(mut self) -> Self {
        self.null_order = NullOrder::Last;
        self
    }
}

/// Comparison and ordering operators available on every column marker.
///
/// Operand values are converted into the column's Rust type first, so a type
/// mismatch is a compile error rather than a runtime failure. Each captured
/// value becomes a positional bind, never an IR literal.
pub trait ColumnExt: Column + Sized {
    /// Builds a SQL equality predicate.
    #[must_use]
    fn eq(self, value: impl Into<Self::Rust>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::Equal, value.into())
    }

    /// Builds a SQL inequality predicate.
    #[must_use]
    fn ne(self, value: impl Into<Self::Rust>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::NotEqual, value.into())
    }

    /// Builds a strict less-than predicate.
    #[must_use]
    fn lt(self, value: impl Into<Self::Rust>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::LessThan, value.into())
    }

    /// Builds an inclusive less-than predicate.
    #[must_use]
    fn le(self, value: impl Into<Self::Rust>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::LessThanOrEqual, value.into())
    }

    /// Builds a strict greater-than predicate.
    #[must_use]
    fn gt(self, value: impl Into<Self::Rust>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::GreaterThan, value.into())
    }

    /// Builds an inclusive greater-than predicate.
    #[must_use]
    fn ge(self, value: impl Into<Self::Rust>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::GreaterThanOrEqual, value.into())
    }

    /// Tests whether the stored value is SQL `NULL`.
    #[must_use]
    fn is_null(self) -> Expr<bool> {
        null_test::<Self>(UnaryOperator::IsNull)
    }

    /// Tests whether the stored value is not SQL `NULL`.
    #[must_use]
    fn is_not_null(self) -> Expr<bool> {
        null_test::<Self>(UnaryOperator::IsNotNull)
    }

    /// Orders by this column with the lowest value first.
    #[must_use]
    fn asc(self) -> OrderKey {
        OrderKey::new(Self::INDEX, SortDirection::Ascending)
    }

    /// Orders by this column with the highest value first.
    #[must_use]
    fn desc(self) -> OrderKey {
        OrderKey::new(Self::INDEX, SortDirection::Descending)
    }
}

impl<C> ColumnExt for C where C: Column {}

/// Pattern operators available on text columns only.
pub trait TextColumnExt: Column<Rust = String> + Sized {
    /// Builds a SQL `LIKE` pattern predicate.
    #[must_use]
    fn like(self, pattern: impl Into<String>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::Like, pattern.into())
    }

    /// Builds a case-insensitive `LIKE` pattern predicate.
    #[must_use]
    fn ilike(self, pattern: impl Into<String>) -> Expr<bool> {
        compare::<Self>(BinaryOperator::CaseInsensitiveLike, pattern.into())
    }
}

impl<C> TextColumnExt for C where C: Column<Rust = String> {}

fn compare<C>(op: BinaryOperator, value: C::Rust) -> Expr<bool>
where
    C: Column,
{
    Expr::from_node(Node::Binary {
        op,
        left: Box::new(Node::Column(C::INDEX)),
        right: Box::new(Node::Value(value.into_value())),
    })
}

fn null_test<C>(op: UnaryOperator) -> Expr<bool>
where
    C: Column,
{
    Expr::from_node(Node::Unary {
        op,
        operand: Box::new(Node::Column(C::INDEX)),
    })
}

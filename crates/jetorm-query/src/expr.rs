use std::marker::PhantomData;

use afterburner::ir::{BinaryOperator, NullOrder, SortDirection, UnaryOperator};
use jetorm_entity::{Column, ColumnType, Entity, SqlValue, Value};

/// Static SQL typing of one expression operand.
///
/// Operand types are captured when the expression is built, from the same
/// column metadata the lowering uses for column references. Carrying them in
/// the tree keeps lowering independent of bound values: the expression alone
/// determines the IR.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OperandType {
    pub(crate) column_type: ColumnType,
    pub(crate) nullable: bool,
    /// Whether the operand is an array of `column_type` elements.
    pub(crate) list: bool,
}

impl OperandType {
    /// Reads the typing of one value type directly.
    pub(crate) fn of_value<T>() -> Self
    where
        T: crate::SqlValueTyping,
    {
        Self {
            column_type: T::COLUMN_TYPE_OF,
            nullable: T::NULLABLE_OF,
            list: false,
        }
    }

    /// Reads the typing of one column from its entity metadata.
    fn of_column<C>() -> Self
    where
        C: Column,
    {
        let meta = C::meta();
        Self {
            column_type: meta.column_type(),
            nullable: meta.is_nullable(),
            list: false,
        }
    }
}

/// Expression tree as built by the column operators.
///
/// User values sit inline here until [`crate::Select::filter`] normalizes
/// them into the query's positional bind table, which yields a [`Predicate`].
#[derive(Clone, Debug)]
pub(crate) enum Node {
    /// Reference to one entity column by position in `Entity::COLUMNS`.
    Column(usize),
    /// User value not yet assigned a bind position.
    Value { value: Value, ty: OperandType },
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

/// Normalized expression tree holding bind positions instead of values.
///
/// A predicate contains no user data by construction, so it is exactly the
/// value-independent shape of an expression: it hashes and compares as that
/// shape, and lowering it needs no access to the bind table.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Predicate {
    /// Reference to one entity column by position in `Entity::COLUMNS`.
    Column(usize),
    /// Reference to one positional bind, with the typing of its operand.
    Bind { position: usize, ty: OperandType },
    /// One unary scalar operation.
    Unary {
        op: UnaryOperator,
        operand: Box<Predicate>,
    },
    /// One binary scalar operation.
    Binary {
        op: BinaryOperator,
        left: Box<Predicate>,
        right: Box<Predicate>,
    },
}

/// Replaces captured values with bind positions in expression pre-order.
pub(crate) fn normalize(node: Node, binds: &mut Vec<Value>) -> Predicate {
    match node {
        Node::Column(index) => Predicate::Column(index),
        Node::Value { value, ty } => {
            let position = binds.len();
            binds.push(value);
            Predicate::Bind { position, ty }
        }
        Node::Unary { op, operand } => Predicate::Unary {
            op,
            operand: Box::new(normalize(*operand, binds)),
        },
        Node::Binary { op, left, right } => {
            let left = normalize(*left, binds);
            let right = normalize(*right, binds);
            Predicate::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            }
        }
    }
}

/// Typed scalar expression produced by column operators.
///
/// The entity parameter binds the expression to the table it reads, so a
/// predicate built from one entity's columns cannot be attached to another
/// entity's query. The value parameter tracks the expression's result type,
/// so predicate combinators accept only boolean expressions.
#[derive(Debug)]
pub struct Expr<E, T> {
    pub(crate) node: Node,
    marker: PhantomData<fn() -> (E, T)>,
}

impl<E, T> Expr<E, T> {
    pub(crate) const fn from_node(node: Node) -> Self {
        Self {
            node,
            marker: PhantomData,
        }
    }
}

impl<E, T> Clone for Expr<E, T> {
    fn clone(&self) -> Self {
        Self::from_node(self.node.clone())
    }
}

impl<E> Expr<E, bool>
where
    E: Entity,
{
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
impl<E> std::ops::Not for Expr<E, bool>
where
    E: Entity,
{
    type Output = Self;

    fn not(self) -> Self {
        Self::from_node(Node::Unary {
            op: UnaryOperator::Not,
            operand: Box::new(self.node),
        })
    }
}

/// Entity-independent ordering facts for one sort key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SortKeySpec {
    pub(crate) column: usize,
    pub(crate) direction: SortDirection,
    pub(crate) null_order: NullOrder,
}

/// One `ORDER BY` key over an entity column.
///
/// The entity parameter binds the key to the table it reads, so a key built
/// from one entity's columns cannot order another entity's query. Created
/// through [`ColumnExt::asc`] and [`ColumnExt::desc`]; null placement
/// defaults to the target database's convention.
#[derive(Debug)]
pub struct OrderKey<E> {
    pub(crate) spec: SortKeySpec,
    marker: PhantomData<fn() -> E>,
}

impl<E> Clone for OrderKey<E> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<E> Copy for OrderKey<E> {}

impl<E> OrderKey<E>
where
    E: Entity,
{
    const fn new(column: usize, direction: SortDirection) -> Self {
        Self {
            spec: SortKeySpec {
                column,
                direction,
                null_order: NullOrder::DialectDefault,
            },
            marker: PhantomData,
        }
    }

    /// Places SQL `NULL` values before non-null values.
    #[must_use]
    pub const fn nulls_first(mut self) -> Self {
        self.spec.null_order = NullOrder::First;
        self
    }

    /// Places SQL `NULL` values after non-null values.
    #[must_use]
    pub const fn nulls_last(mut self) -> Self {
        self.spec.null_order = NullOrder::Last;
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
    fn eq(self, value: impl Into<Self::Rust>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::Equal, value.into())
    }

    /// Builds a SQL inequality predicate.
    #[must_use]
    fn ne(self, value: impl Into<Self::Rust>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::NotEqual, value.into())
    }

    /// Builds a strict less-than predicate.
    #[must_use]
    fn lt(self, value: impl Into<Self::Rust>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::LessThan, value.into())
    }

    /// Builds an inclusive less-than predicate.
    #[must_use]
    fn le(self, value: impl Into<Self::Rust>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::LessThanOrEqual, value.into())
    }

    /// Builds a strict greater-than predicate.
    #[must_use]
    fn gt(self, value: impl Into<Self::Rust>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::GreaterThan, value.into())
    }

    /// Builds an inclusive greater-than predicate.
    #[must_use]
    fn ge(self, value: impl Into<Self::Rust>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::GreaterThanOrEqual, value.into())
    }

    /// Tests whether the stored value is SQL `NULL`.
    #[must_use]
    fn is_null(self) -> Expr<Self::Entity, bool> {
        null_test::<Self>(UnaryOperator::IsNull)
    }

    /// Tests whether the stored value is not SQL `NULL`.
    #[must_use]
    fn is_not_null(self) -> Expr<Self::Entity, bool> {
        null_test::<Self>(UnaryOperator::IsNotNull)
    }

    /// Builds a membership predicate over a list of values.
    ///
    /// The whole list binds as one array parameter, so every list length
    /// shares one query shape and one prepared statement — a paginated
    /// batch loader cannot flood the plan cache. An empty list matches no
    /// rows.
    #[must_use]
    fn is_in<I>(self, values: I) -> Expr<Self::Entity, bool>
    where
        I: IntoIterator,
        I::Item: Into<Self::Rust>,
    {
        let element = <Self::Rust as SqlValue>::COLUMN_TYPE;
        let values: Vec<Value> = values
            .into_iter()
            .map(|value| value.into().into_value())
            .collect();
        Expr::from_node(Node::Binary {
            op: BinaryOperator::InArray,
            left: Box::new(Node::Column(Self::INDEX)),
            right: Box::new(Node::Value {
                value: Value::Array { element, values },
                ty: OperandType {
                    column_type: element,
                    nullable: false,
                    list: true,
                },
            }),
        })
    }

    /// Builds an inclusive range predicate: `low <= column <= high`.
    #[must_use]
    fn between(
        self,
        low: impl Into<Self::Rust>,
        high: impl Into<Self::Rust>,
    ) -> Expr<Self::Entity, bool> {
        self.ge(low).and(self.le(high))
    }

    /// Orders by this column with the lowest value first.
    #[must_use]
    fn asc(self) -> OrderKey<Self::Entity> {
        OrderKey::new(Self::INDEX, SortDirection::Ascending)
    }

    /// Orders by this column with the highest value first.
    #[must_use]
    fn desc(self) -> OrderKey<Self::Entity> {
        OrderKey::new(Self::INDEX, SortDirection::Descending)
    }
}

impl<C> ColumnExt for C where C: Column {}

/// Pattern operators available on text columns only.
///
/// [`TextColumnExt::like`] and [`TextColumnExt::ilike`] treat the argument
/// as a pattern the caller controls. The substring operators treat it as
/// literal text: `%`, `_`, and `\` in the needle are escaped, so matching a
/// discount string like `"50%"` matches those three characters rather than
/// turning user input into a wildcard.
pub trait TextColumnExt: Column<Rust = String> + Sized {
    /// Builds a SQL `LIKE` pattern predicate.
    #[must_use]
    fn like(self, pattern: impl Into<String>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::Like, pattern.into())
    }

    /// Builds a case-insensitive `LIKE` pattern predicate.
    #[must_use]
    fn ilike(self, pattern: impl Into<String>) -> Expr<Self::Entity, bool> {
        compare::<Self>(BinaryOperator::CaseInsensitiveLike, pattern.into())
    }

    /// Matches values containing the needle as literal text.
    #[must_use]
    fn contains(self, needle: impl AsRef<str>) -> Expr<Self::Entity, bool> {
        compare::<Self>(
            BinaryOperator::Like,
            format!("%{}%", escape_like(needle.as_ref())),
        )
    }

    /// Matches values starting with the prefix as literal text.
    #[must_use]
    fn starts_with(self, prefix: impl AsRef<str>) -> Expr<Self::Entity, bool> {
        compare::<Self>(
            BinaryOperator::Like,
            format!("{}%", escape_like(prefix.as_ref())),
        )
    }

    /// Matches values ending with the suffix as literal text.
    #[must_use]
    fn ends_with(self, suffix: impl AsRef<str>) -> Expr<Self::Entity, bool> {
        compare::<Self>(
            BinaryOperator::Like,
            format!("%{}", escape_like(suffix.as_ref())),
        )
    }
}

impl<C> TextColumnExt for C where C: Column<Rust = String> {}

/// Escapes `LIKE` metacharacters so a needle matches itself literally.
///
/// PostgreSQL's default `LIKE` escape character is the backslash, so the
/// escaped needle needs no `ESCAPE` clause.
fn escape_like(needle: &str) -> String {
    let mut escaped = String::with_capacity(needle.len());
    for character in needle.chars() {
        if matches!(character, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Builds the primary-key equality chain of one entity from a key value.
///
/// Values pair positionally with [`jetorm_entity::Entity::PRIMARY_KEY`],
/// each typed by its column's metadata and combined with `AND`.
pub(crate) fn key_equalities<E>(key: E::Key) -> Expr<E, bool>
where
    E: jetorm_entity::KeyedEntity,
{
    let values = E::key_values(key);
    debug_assert_eq!(
        values.len(),
        E::PRIMARY_KEY.len(),
        "the derive emits one key value per primary-key column"
    );
    let mut chain: Option<Node> = None;
    for (index, value) in E::PRIMARY_KEY.iter().zip(values) {
        let meta = &E::COLUMNS[*index];
        let equality = Node::Binary {
            op: BinaryOperator::Equal,
            left: Box::new(Node::Column(*index)),
            right: Box::new(Node::Value {
                value,
                ty: OperandType {
                    column_type: meta.column_type(),
                    nullable: meta.is_nullable(),
                    list: false,
                },
            }),
        };
        chain = Some(match chain {
            Some(existing) => Node::Binary {
                op: BinaryOperator::And,
                left: Box::new(existing),
                right: Box::new(equality),
            },
            None => equality,
        });
    }
    Expr::from_node(chain.expect("KeyedEntity guarantees at least one key column"))
}

fn compare<C>(op: BinaryOperator, value: C::Rust) -> Expr<C::Entity, bool>
where
    C: Column,
{
    Expr::from_node(Node::Binary {
        op,
        left: Box::new(Node::Column(C::INDEX)),
        right: Box::new(Node::Value {
            value: value.into_value(),
            // The operand adopts the column's exact typing, so a comparison
            // against a column never needs a widening cast during lowering.
            ty: OperandType::of_column::<C>(),
        }),
    })
}

fn null_test<C>(op: UnaryOperator) -> Expr<C::Entity, bool>
where
    C: Column,
{
    Expr::from_node(Node::Unary {
        op,
        operand: Box::new(Node::Column(C::INDEX)),
    })
}

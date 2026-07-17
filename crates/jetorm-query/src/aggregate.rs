use std::marker::PhantomData;
use std::sync::Arc;

use afterburner::ir::BinaryOperator;
use jetorm_entity::{Column, ColumnType, DecodeError, Entity, SqlValue, Value};

use crate::expr::{Expr, Node, OperandType, Predicate, normalize};
use crate::projection::ColumnList;
use crate::select::{QueryShape, Select};

/// Aggregate function identity, part of a grouped query's shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AggregateFunction {
    /// `count(*)` — rows per group.
    Count,
    /// `sum` over one column.
    Sum,
    /// `avg` over one column.
    Avg,
    /// `min` over one column.
    Min,
    /// `max` over one column.
    Max,
}

impl AggregateFunction {
    /// Returns the SQL function name.
    #[must_use]
    pub const fn sql_name(self) -> &'static str {
        match self {
            Self::Count => "count",
            Self::Sum => "sum",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
        }
    }
}

/// One aggregate of a grouped query, as value-independent structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AggregateSpec {
    /// Function applied per group.
    pub function: AggregateFunction,
    /// Column position the function consumes; `None` counts rows.
    pub column: Option<usize>,
    /// Output column type of the aggregate.
    pub column_type: ColumnType,
    /// Whether the output can be SQL `NULL`.
    pub nullable: bool,
}

/// One typed aggregate expression: the function plus its Rust result type.
///
/// Built by [`count_rows`], [`sum`], [`avg`], [`min`], and [`max`]. The
/// result type carries the database's own promotion rules at compile time —
/// summing 32-bit integers yields a 64-bit total, averaging integers yields
/// an exact decimal — and aggregating a nullable column yields an `Option`,
/// since a group whose values are all `NULL` aggregates to `NULL`.
#[derive(Clone, Copy, Debug)]
pub struct Aggregate<E, T> {
    function: AggregateFunction,
    column: Option<usize>,
    entity: PhantomData<fn() -> E>,
    output: PhantomData<fn() -> T>,
}

/// Counts the rows of each group; never `NULL`.
#[must_use]
pub fn count_rows<E>() -> Aggregate<E, i64>
where
    E: Entity,
{
    Aggregate {
        function: AggregateFunction::Count,
        column: None,
        entity: PhantomData,
        output: PhantomData,
    }
}

/// Sums one column per group, at the width the database really returns.
#[must_use]
pub fn sum<C>(column: C) -> Aggregate<C::Entity, <C::Field as Summable>::Sum>
where
    C: Column,
    C::Field: Summable,
{
    let _ = column;
    Aggregate {
        function: AggregateFunction::Sum,
        column: Some(C::INDEX),
        entity: PhantomData,
        output: PhantomData,
    }
}

/// Averages one column per group, at the type the database really returns.
#[must_use]
pub fn avg<C>(column: C) -> Aggregate<C::Entity, <C::Field as Averageable>::Avg>
where
    C: Column,
    C::Field: Averageable,
{
    let _ = column;
    Aggregate {
        function: AggregateFunction::Avg,
        column: Some(C::INDEX),
        entity: PhantomData,
        output: PhantomData,
    }
}

/// Takes the smallest value of one column per group.
#[must_use]
pub fn min<C>(column: C) -> Aggregate<C::Entity, C::Field>
where
    C: Column,
    C::Field: Comparable,
{
    let _ = column;
    Aggregate {
        function: AggregateFunction::Min,
        column: Some(C::INDEX),
        entity: PhantomData,
        output: PhantomData,
    }
}

/// Takes the largest value of one column per group.
#[must_use]
pub fn max<C>(column: C) -> Aggregate<C::Entity, C::Field>
where
    C: Column,
    C::Field: Comparable,
{
    let _ = column;
    Aggregate {
        function: AggregateFunction::Max,
        column: Some(C::INDEX),
        entity: PhantomData,
        output: PhantomData,
    }
}

/// Types `sum` accepts, with the database's own result promotion.
///
/// PostgreSQL widens as it accumulates: 16- and 32-bit integers sum to
/// 64 bits, 64-bit integers sum to `numeric`, floats and decimals keep
/// their type. Carrying that rule here means the decoded Rust type is the
/// wire type, never a lossy re-narrowing.
pub trait Summable: SqlValue {
    /// The sum's Rust type.
    type Sum: SqlValue;
}

impl Summable for i16 {
    type Sum = i64;
}
impl Summable for i32 {
    type Sum = i64;
}
impl Summable for i64 {
    type Sum = rust_decimal::Decimal;
}
impl Summable for f32 {
    type Sum = f32;
}
impl Summable for f64 {
    type Sum = f64;
}
impl Summable for rust_decimal::Decimal {
    type Sum = rust_decimal::Decimal;
}
/// A nullable column can aggregate to `NULL`, so its sum is optional.
impl<T> Summable for Option<T>
where
    T: Summable,
    Option<T>: SqlValue,
    Option<T::Sum>: SqlValue,
{
    type Sum = Option<T::Sum>;
}

/// Types `avg` accepts, with the database's own result promotion:
/// integers average to `numeric`, floats to 64-bit floats.
pub trait Averageable: SqlValue {
    /// The average's Rust type.
    type Avg: SqlValue;
}

impl Averageable for i16 {
    type Avg = rust_decimal::Decimal;
}
impl Averageable for i32 {
    type Avg = rust_decimal::Decimal;
}
impl Averageable for i64 {
    type Avg = rust_decimal::Decimal;
}
impl Averageable for f32 {
    type Avg = f64;
}
impl Averageable for f64 {
    type Avg = f64;
}
impl Averageable for rust_decimal::Decimal {
    type Avg = rust_decimal::Decimal;
}
/// A nullable column can aggregate to `NULL`, so its average is optional.
impl<T> Averageable for Option<T>
where
    T: Averageable,
    Option<T>: SqlValue,
    Option<T::Avg>: SqlValue,
{
    type Avg = Option<T::Avg>;
}

/// Types with a total SQL ordering, accepted by `min` and `max`.
pub trait Comparable: SqlValue {}

impl Comparable for i16 {}
impl Comparable for i32 {}
impl Comparable for i64 {}
impl Comparable for f32 {}
impl Comparable for f64 {}
impl Comparable for rust_decimal::Decimal {}
impl Comparable for String {}
impl Comparable for chrono::NaiveDate {}
impl Comparable for chrono::NaiveTime {}
impl Comparable for chrono::NaiveDateTime {}
impl Comparable for chrono::DateTime<chrono::Utc> {}
impl<T> Comparable for Option<T>
where
    T: Comparable,
    Option<T>: SqlValue,
{
}

/// A typed reference to one aggregate of the grouped output row.
///
/// Handed to [`GroupedSelect::having`]'s builder, positioned where the
/// aggregate sits in the output; comparisons produce `HAVING` predicates
/// typed at the aggregate's own promoted type.
#[derive(Clone, Copy, Debug)]
pub struct AggregateRef<T> {
    position: usize,
    output: PhantomData<fn() -> T>,
}

/// Comparison operands of one aggregate result type.
///
/// The operand is always the non-null type: comparing a nullable aggregate
/// against SQL `NULL` with `=` would silently match nothing under
/// three-valued logic, so `eq(None)` must not compile —
/// [`AggregateRef::is_null`] is that question's correct spelling.
pub trait AggregateComparand: SqlValue {
    /// The value type comparisons accept.
    type Operand: SqlValue + Into<Self>;
}

macro_rules! impl_aggregate_comparand {
    ($($ty:ty),+ $(,)?) => {
        $(impl AggregateComparand for $ty {
            type Operand = $ty;
        })+
    };
}

impl_aggregate_comparand!(
    i16,
    i32,
    i64,
    f32,
    f64,
    rust_decimal::Decimal,
    String,
    chrono::NaiveDate,
    chrono::NaiveTime,
    chrono::NaiveDateTime,
    chrono::DateTime<chrono::Utc>,
);

impl<U> AggregateComparand for Option<U>
where
    U: AggregateComparand<Operand = U>,
    Option<U>: SqlValue,
{
    type Operand = U;
}

impl<T> AggregateRef<T>
where
    T: AggregateComparand,
{
    fn compare(self, op: BinaryOperator, value: impl Into<T::Operand>) -> HavingExpr {
        HavingExpr {
            node: Node::Binary {
                op,
                left: Box::new(Node::Column(self.position)),
                right: Box::new(Node::Value {
                    value: value.into().into_value(),
                    ty: OperandType::of_value::<T::Operand>(),
                }),
            },
        }
    }

    /// Requires the aggregate to equal the value.
    pub fn eq(self, value: impl Into<T::Operand>) -> HavingExpr {
        self.compare(BinaryOperator::Equal, value)
    }

    /// Requires the aggregate to differ from the value.
    pub fn ne(self, value: impl Into<T::Operand>) -> HavingExpr {
        self.compare(BinaryOperator::NotEqual, value)
    }

    /// Requires the aggregate to be below the value.
    pub fn lt(self, value: impl Into<T::Operand>) -> HavingExpr {
        self.compare(BinaryOperator::LessThan, value)
    }

    /// Requires the aggregate to be at most the value.
    pub fn le(self, value: impl Into<T::Operand>) -> HavingExpr {
        self.compare(BinaryOperator::LessThanOrEqual, value)
    }

    /// Requires the aggregate to exceed the value.
    pub fn gt(self, value: impl Into<T::Operand>) -> HavingExpr {
        self.compare(BinaryOperator::GreaterThan, value)
    }

    /// Requires the aggregate to be at least the value.
    pub fn ge(self, value: impl Into<T::Operand>) -> HavingExpr {
        self.compare(BinaryOperator::GreaterThanOrEqual, value)
    }
}

impl<U> AggregateRef<Option<U>>
where
    Option<U>: SqlValue,
{
    fn null_test(self, op: afterburner::ir::UnaryOperator) -> HavingExpr {
        HavingExpr {
            node: Node::Unary {
                op,
                operand: Box::new(Node::Column(self.position)),
            },
        }
    }

    /// Requires the aggregate to be SQL `NULL` — an all-`NULL` group.
    pub fn is_null(self) -> HavingExpr {
        self.null_test(afterburner::ir::UnaryOperator::IsNull)
    }

    /// Requires the aggregate to be non-`NULL`.
    pub fn is_not_null(self) -> HavingExpr {
        self.null_test(afterburner::ir::UnaryOperator::IsNotNull)
    }
}

/// A Boolean predicate over the grouped output row — SQL's `HAVING`.
#[derive(Debug)]
#[must_use = "a having expression does nothing until passed to having()"]
pub struct HavingExpr {
    pub(crate) node: Node,
}

impl HavingExpr {
    /// Requires both conditions.
    pub fn and(self, other: Self) -> Self {
        Self {
            node: Node::Binary {
                op: BinaryOperator::And,
                left: Box::new(self.node),
                right: Box::new(other.node),
            },
        }
    }

    /// Requires either condition.
    pub fn or(self, other: Self) -> Self {
        Self {
            node: Node::Binary {
                op: BinaryOperator::Or,
                left: Box::new(self.node),
                right: Box::new(other.node),
            },
        }
    }
}

/// A typed list of aggregates, one to four per query.
pub trait AggregateList<E>
where
    E: Entity,
{
    /// Rust type one aggregate row decodes into.
    type Row;

    /// Typed references to each aggregate, for `HAVING` builders.
    type Refs;

    /// Builds the references, positioned after `key_width` group keys.
    fn refs(key_width: usize) -> Self::Refs;

    /// Value-independent structure of each aggregate, in output order.
    fn specs(&self) -> Vec<AggregateSpec>;

    /// Decodes the aggregate segment of one fetched row.
    ///
    /// # Errors
    ///
    /// Returns an error when the width or a payload kind does not match.
    fn decode(values: Vec<Value>) -> Result<Self::Row, DecodeError>;
}

impl<E, T> AggregateList<E> for (Aggregate<E, T>,)
where
    E: Entity,
    T: SqlValue,
{
    type Row = T;

    type Refs = AggregateRef<T>;

    fn refs(key_width: usize) -> Self::Refs {
        AggregateRef {
            position: key_width,
            output: PhantomData,
        }
    }

    fn specs(&self) -> Vec<AggregateSpec> {
        vec![AggregateSpec {
            function: self.0.function,
            column: self.0.column,
            column_type: T::COLUMN_TYPE,
            nullable: T::NULLABLE,
        }]
    }

    fn decode(values: Vec<Value>) -> Result<Self::Row, DecodeError> {
        if values.len() != 1 {
            return Err(DecodeError::ColumnCount {
                expected: 1,
                actual: values.len(),
            });
        }
        T::from_value(values.into_iter().next().expect("width checked above")).map_err(|mismatch| {
            DecodeError::Column {
                name: "aggregate 0",
                mismatch,
            }
        })
    }
}

macro_rules! impl_aggregate_list_for_tuple {
    ($count:literal, $($output:ident $index:tt $name:literal),+) => {
        impl<E, $($output),+> AggregateList<E> for ($(Aggregate<E, $output>,)+)
        where
            E: Entity,
            $($output: SqlValue,)+
        {
            type Row = ($($output,)+);

            type Refs = ($(AggregateRef<$output>,)+);

            fn refs(key_width: usize) -> Self::Refs {
                ($(AggregateRef {
                    position: key_width + $index,
                    output: PhantomData,
                },)+)
            }

            fn specs(&self) -> Vec<AggregateSpec> {
                vec![$(AggregateSpec {
                    function: self.$index.function,
                    column: self.$index.column,
                    column_type: $output::COLUMN_TYPE,
                    nullable: $output::NULLABLE,
                }),+]
            }

            fn decode(values: Vec<Value>) -> Result<Self::Row, DecodeError> {
                if values.len() != $count {
                    return Err(DecodeError::ColumnCount {
                        expected: $count,
                        actual: values.len(),
                    });
                }
                let mut values = values.into_iter();
                Ok(($(
                    $output::from_value(values.next().expect("width checked above"))
                        .map_err(|mismatch| DecodeError::Column {
                            name: $name,
                            mismatch,
                        })?,
                )+))
            }
        }
    };
}

impl_aggregate_list_for_tuple!(2, A 0 "aggregate 0", B 1 "aggregate 1");
impl_aggregate_list_for_tuple!(3, A 0 "aggregate 0", B 1 "aggregate 1", C 2 "aggregate 2");
impl_aggregate_list_for_tuple!(
    4,
    A 0 "aggregate 0",
    B 1 "aggregate 1",
    C 2 "aggregate 2",
    D 3 "aggregate 3"
);

/// A select collapsed into groups, each yielding its keys and aggregates.
///
/// Created by [`Select::group_by`] followed by
/// [`GroupBy::select_agg`]. Filters on the base select apply before
/// grouping — SQL's `WHERE` — and the fetched rows pair each group's key
/// values with its aggregate values.
#[derive(Debug)]
pub struct GroupedSelect<E, K, A>
where
    E: Entity,
{
    pub(crate) select: Select<E>,
    pub(crate) aggregates: Vec<AggregateSpec>,
    pub(crate) order_by_keys: bool,
    /// Predicate over the grouped output row — SQL's `HAVING`.
    pub(crate) having: Option<Arc<Predicate>>,
    keys: PhantomData<fn() -> K>,
    output: PhantomData<fn() -> A>,
}

/// A select with grouping keys chosen, awaiting its aggregates.
#[derive(Debug)]
pub struct GroupBy<E, K>
where
    E: Entity,
{
    select: Select<E>,
    keys: PhantomData<fn() -> K>,
}

/// The key and aggregate parameters are phantom, so cloning never
/// requires them to be cloneable themselves.
impl<E, K, A> Clone for GroupedSelect<E, K, A>
where
    E: Entity,
{
    fn clone(&self) -> Self {
        Self {
            select: self.select.clone(),
            aggregates: self.aggregates.clone(),
            order_by_keys: self.order_by_keys,
            having: self.having.clone(),
            keys: PhantomData,
            output: PhantomData,
        }
    }
}

impl<E, K> Clone for GroupBy<E, K>
where
    E: Entity,
{
    fn clone(&self) -> Self {
        Self {
            select: self.select.clone(),
            keys: PhantomData,
        }
    }
}

impl<E> Select<E>
where
    E: Entity,
{
    /// Groups rows by the given columns.
    ///
    /// The filter set before this call restricts the rows that enter the
    /// groups — SQL's `WHERE`. Restricting on aggregate results (`HAVING`)
    /// is not expressible yet.
    #[must_use]
    pub fn group_by<K>(self, keys: K) -> GroupBy<E, K>
    where
        K: ColumnList<E>,
    {
        let _ = keys;
        GroupBy {
            select: self,
            keys: PhantomData,
        }
    }
}

impl<E, K> GroupBy<E, K>
where
    E: Entity,
    K: ColumnList<E>,
{
    /// Chooses the aggregates computed per group.
    ///
    /// Each fetched row is `(keys, aggregates)`: one group's key values —
    /// a single column decodes bare, a tuple as a tuple — paired with its
    /// aggregate values.
    #[must_use]
    pub fn select_agg<A>(self, aggregates: A) -> GroupedSelect<E, K, A>
    where
        A: AggregateList<E>,
    {
        GroupedSelect {
            select: self.select,
            aggregates: aggregates.specs(),
            order_by_keys: false,
            having: None,
            keys: PhantomData,
            output: PhantomData,
        }
    }
}

impl<E, K, A> GroupedSelect<E, K, A>
where
    E: Entity,
    K: ColumnList<E>,
    A: AggregateList<E>,
{
    /// Restricts grouped rows to those whose source rows satisfy the
    /// predicate — SQL's `WHERE`, applied before grouping.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<E, bool>) -> Self {
        self.select = self.select.filter(predicate);
        self
    }

    /// Restricts fetched groups by their aggregate values — SQL's `HAVING`.
    ///
    /// The builder receives one typed reference per aggregate, in tuple
    /// order, each comparing at the aggregate's own promoted type:
    ///
    /// ```text
    /// .select_agg((count_rows(), sum(order::Quantity)))
    /// .having(|(rows, total)| rows.ge(2).and(total.gt(10)))
    /// ```
    ///
    /// Successive calls combine with SQL `AND`. Values bind after the
    /// filter's values, in call order.
    #[must_use]
    pub fn having(mut self, build: impl FnOnce(A::Refs) -> HavingExpr) -> Self {
        let expression = build(A::refs(K::indexes().len()));
        let normalized = normalize(expression.node, &mut self.select.binds);
        self.having = Some(Arc::new(match self.having.take() {
            Some(existing) => Predicate::Binary {
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

    /// Orders the fetched groups by their key values, ascending.
    ///
    /// Grouped output has no inherent order; this makes it deterministic
    /// for display and pagination.
    #[must_use]
    pub const fn order_by_keys(mut self) -> Self {
        self.order_by_keys = true;
        self
    }

    /// Returns captured values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        self.select.binds()
    }

    /// Consumes the query and returns its captured values in bind order.
    #[must_use]
    pub fn into_binds(self) -> Vec<Value> {
        self.select.into_binds()
    }

    /// Returns the group-key column positions, in output order.
    #[must_use]
    pub fn key_indexes(&self) -> Vec<usize> {
        K::indexes()
    }

    /// Returns each aggregate's value-independent structure.
    #[must_use]
    pub fn aggregate_specs(&self) -> &[AggregateSpec] {
        &self.aggregates
    }

    /// Returns this query's value-independent shape.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        QueryShape::for_grouped(
            &self.select,
            K::indexes(),
            self.aggregates.clone(),
            self.order_by_keys,
            self.having.clone(),
        )
    }

    /// Decodes one fetched row into `(keys, aggregates)`.
    ///
    /// # Errors
    ///
    /// Returns an error when the row does not match the grouped output.
    pub fn decode_row(mut values: Vec<Value>) -> Result<(K::Row, A::Row), DecodeError> {
        let key_width = K::indexes().len();
        if values.len() < key_width {
            return Err(DecodeError::ColumnCount {
                expected: key_width,
                actual: values.len(),
            });
        }
        let aggregate_values = values.split_off(key_width);
        Ok((K::decode(values)?, A::decode(aggregate_values)?))
    }
}

impl<E, K, A> crate::select::CacheableQuery for GroupedSelect<E, K, A>
where
    E: Entity,
    K: ColumnList<E>,
    A: AggregateList<E>,
{
    fn shape(&self) -> QueryShape {
        Self::shape(self)
    }
}

use std::marker::PhantomData;

use jetorm_entity::{Column, DecodeError, Entity, SqlValue, Value};

use crate::expr::{Expr, OrderKey};
use crate::select::{QueryShape, Select};

/// A typed list of columns to project, all belonging to one entity.
///
/// Implemented by tuples of column markers up to eight columns — a single
/// column is the 1-tuple `(user::Id,)`, which decodes to its field type
/// rather than a 1-tuple — and by partial-model structs through
/// `#[derive(JetPartial)]`, which decode to themselves. The list fixes both
/// the projected positions and the Rust row type they decode into, so a
/// projected query is as statically typed as a full-model query: a nullable
/// column comes back as an `Option`, and a wrong tuple arity or element
/// type is a compile error.
///
/// Everything is associated rather than instance-based: a projection is a
/// fact about a type, and row types like partial models have no value in
/// hand when the query is built.
pub trait ColumnList<E>
where
    E: Entity,
{
    /// Rust type one projected row decodes into.
    type Row;

    /// Column positions within `E::COLUMNS`, in output order.
    fn indexes() -> Vec<usize>;

    /// Decodes one row of projected values.
    ///
    /// # Errors
    ///
    /// Returns an error when the width or a payload kind does not match the
    /// projected columns.
    fn decode(values: Vec<Value>) -> Result<Self::Row, DecodeError>;
}

/// A single projected column decodes to its field type, not a 1-tuple.
impl<E, A> ColumnList<E> for (A,)
where
    E: Entity,
    A: Column<Entity = E>,
{
    type Row = A::Field;

    fn indexes() -> Vec<usize> {
        vec![A::INDEX]
    }

    fn decode(values: Vec<Value>) -> Result<Self::Row, DecodeError> {
        if values.len() != 1 {
            return Err(DecodeError::ColumnCount {
                expected: 1,
                actual: values.len(),
            });
        }
        let value = values.into_iter().next().expect("width checked above");
        A::Field::from_value(value).map_err(|mismatch| DecodeError::Column {
            name: A::meta().name(),
            mismatch,
        })
    }
}

macro_rules! impl_column_list_for_tuple {
    ($count:literal, $($column:ident . $index:tt),+) => {
        impl<E, $($column),+> ColumnList<E> for ($($column,)+)
        where
            E: Entity,
            $($column: Column<Entity = E>,)+
        {
            type Row = ($($column::Field,)+);

            fn indexes() -> Vec<usize> {
                vec![$($column::INDEX),+]
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
                    $column::Field::from_value(
                        values.next().expect("width checked above"),
                    )
                    .map_err(|mismatch| DecodeError::Column {
                        name: $column::meta().name(),
                        mismatch,
                    })?,
                )+))
            }
        }
    };
}

impl_column_list_for_tuple!(2, A.0, B.1);
impl_column_list_for_tuple!(3, A.0, B.1, C.2);
impl_column_list_for_tuple!(4, A.0, B.1, C.2, D.3);
impl_column_list_for_tuple!(5, A.0, B.1, C.2, D.3, F.4);
impl_column_list_for_tuple!(6, A.0, B.1, C.2, D.3, F.4, G.5);
impl_column_list_for_tuple!(7, A.0, B.1, C.2, D.3, F.4, G.5, H.6);
impl_column_list_for_tuple!(8, A.0, B.1, C.2, D.3, F.4, G.5, H.6, I.7);

/// A select that fetches chosen columns instead of whole models.
///
/// Created by [`Select::select`]. Everything the builder adds afterwards —
/// filters, ordering, row limits — still addresses the entity's full row:
/// the projection applies last, so ordering by a column that is not
/// projected works exactly as it does in SQL.
#[derive(Clone, Debug)]
pub struct Projected<E, C>
where
    E: Entity,
    C: ColumnList<E>,
{
    select: Select<E>,
    columns: PhantomData<fn() -> C>,
}

impl<E> Select<E>
where
    E: Entity,
{
    /// Restricts the fetched columns to the given list.
    ///
    /// Rows decode into the list's Rust type: one column yields its field
    /// type, a tuple of columns yields a tuple. Combining a projection with
    /// [`Select::distinct`] is not supported yet and fails at lowering.
    #[must_use]
    pub fn select<C>(self, columns: C) -> Projected<E, C>
    where
        C: ColumnList<E>,
    {
        // The value exists purely so tuple projections infer their type
        // from the argument; the projection itself is a fact about `C`.
        let _ = columns;
        self.select_as::<C>()
    }

    /// Restricts the fetched columns to a projection named by type.
    ///
    /// This is how partial models select themselves:
    /// `query.select_as::<UserSummary>()` fetches exactly the columns the
    /// partial declares and decodes each row into it.
    #[must_use]
    pub fn select_as<C>(mut self) -> Projected<E, C>
    where
        C: ColumnList<E>,
    {
        self.projection = Some(C::indexes());
        Projected {
            select: self,
            columns: PhantomData,
        }
    }
}

impl<E, C> Projected<E, C>
where
    E: Entity,
    C: ColumnList<E>,
{
    /// Restricts rows to those satisfying the predicate.
    ///
    /// The predicate addresses the entity's full row, not the projection.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<E, bool>) -> Self {
        self.select = self.select.filter(predicate);
        self
    }

    /// Appends one ordering key; earlier keys take precedence.
    ///
    /// Keys address the entity's full row, so ordering by an unprojected
    /// column works.
    #[must_use]
    pub fn order_by(mut self, key: OrderKey<E>) -> Self {
        self.select = self.select.order_by(key);
        self
    }

    /// Restricts the number of emitted rows.
    #[must_use]
    pub fn limit(mut self, fetch: u64) -> Self {
        self.select = self.select.limit(fetch);
        self
    }

    /// Skips rows before emission begins.
    #[must_use]
    pub fn offset(mut self, offset: u64) -> Self {
        self.select = self.select.offset(offset);
        self
    }

    /// Returns captured values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        self.select.binds()
    }

    /// Returns this query's value-independent shape.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        self.select.shape()
    }

    /// Returns the underlying select, whose lowering carries the projection.
    #[must_use]
    pub fn into_select(self) -> Select<E> {
        self.select
    }

    /// Decodes one fetched row into the projected Rust type.
    ///
    /// # Errors
    ///
    /// Returns an error when the row does not match the projected columns.
    pub fn decode_row(values: Vec<Value>) -> Result<C::Row, DecodeError> {
        C::decode(values)
    }
}

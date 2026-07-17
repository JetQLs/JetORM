use std::any::TypeId;
use std::{fmt, marker::PhantomData};

use jetorm_entity::{Entity, Relation, Value};

use crate::expr::{Expr, OrderKey};
use crate::select::{QueryShape, Select};

/// A select over one entity joined with a related entity's rows.
///
/// Created by [`Select::also`]. Each fetched row pairs a source model with
/// the row the relation reaches, or `None` when the edge is unmatched — the
/// SQL is a single `LEFT JOIN`, so one query fetches both sides.
///
/// For a to-one edge ([`Relation::TO_ONE`]) every source row appears exactly
/// once. Walking a to-many edge — an [`jetorm_entity::Inverse`] — is honest
/// SQL: a source row appears once per matched row, and once with `None` when
/// nothing matches.
///
/// Filters and ordering keys keep addressing the source entity's columns;
/// predicates over the joined entity's columns are not expressible yet.
#[derive(Clone)]
pub struct JoinSelect<R>
where
    R: Relation,
{
    pub(crate) select: Select<R::Source>,
    relation: PhantomData<fn() -> R>,
}

impl<E> Select<E>
where
    E: Entity,
{
    /// Joins each fetched row with the row the relation reaches.
    ///
    /// `find().also::<post::Author>()` fetches `(Post, Option<User>)` pairs
    /// in one query. The join is a `LEFT JOIN`, so a row whose foreign key
    /// is `NULL` — or whose referenced row is gone — pairs with `None`
    /// instead of disappearing from the result.
    #[must_use]
    pub fn also<R>(self) -> JoinSelect<R>
    where
        R: Relation<Source = E>,
    {
        JoinSelect {
            select: self,
            relation: PhantomData,
        }
    }
}

impl<R> JoinSelect<R>
where
    R: Relation,
{
    /// Restricts rows to those satisfying the predicate.
    ///
    /// The predicate addresses the source entity's columns.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<R::Source, bool>) -> Self {
        self.select = self.select.filter(predicate);
        self
    }

    /// Appends one ordering key; earlier keys take precedence.
    ///
    /// Keys address the source entity's columns.
    #[must_use]
    pub fn order_by(mut self, key: OrderKey<R::Source>) -> Self {
        self.select = self.select.order_by(key);
        self
    }

    /// Restricts the number of emitted rows.
    ///
    /// The limit counts joined rows: over a to-one edge that equals source
    /// rows, over a to-many edge each match counts separately.
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
    ///
    /// Positions match [`Select::binds`] for the source query.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        self.select.binds()
    }

    /// Consumes the query and returns its captured values in bind order.
    #[must_use]
    pub fn into_binds(self) -> Vec<Value> {
        self.select.into_binds()
    }

    /// Returns this query's value-independent shape.
    ///
    /// The joined edge is part of the shape: two joins over different
    /// relations — or a join and its plain select — never share a cached
    /// statement.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        QueryShape::for_join(&self.select, TypeId::of::<R>())
    }
}

impl<R> crate::select::CacheableQuery for JoinSelect<R>
where
    R: Relation,
{
    fn shape(&self) -> QueryShape {
        Self::shape(self)
    }
}

impl<R> fmt::Debug for JoinSelect<R>
where
    R: Relation,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JoinSelect")
            .field("relation", &R::NAME)
            .field("select", &self.select)
            .finish()
    }
}

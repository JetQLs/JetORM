use std::any::TypeId;
use std::sync::Arc;
use std::{fmt, marker::PhantomData};

use afterburner::ir::BinaryOperator;
use jetorm_entity::{Entity, Relation, Value};

use crate::expr::{Expr, OrderKey, Predicate, normalize};
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
/// Filters and ordering keys address the source entity's columns through
/// [`JoinSelect::filter`] and [`JoinSelect::order_by`], and the joined
/// entity's columns through [`JoinSelect::filter_related`] and
/// [`JoinSelect::order_by_related`].
#[derive(Clone)]
pub struct JoinSelect<R>
where
    R: Relation,
{
    pub(crate) select: Select<R::Source>,
    /// Predicate over the joined entity's columns, lowered against the
    /// null-extended right side of the join.
    pub(crate) related_filter: Option<Arc<Predicate>>,
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
            related_filter: None,
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

    /// Restricts rows to those satisfying a predicate over the joined
    /// entity's columns.
    ///
    /// The predicate evaluates after null extension, so an unmatched row's
    /// joined columns are `NULL`: `filter_related(user::Name.eq("alice"))`
    /// keeps only rows whose join matched alice, and drops unmatched rows —
    /// exactly as SQL's `WHERE` over a `LEFT JOIN` does. Values bind into
    /// the same positional table as source-side predicates, in call order.
    #[must_use]
    pub fn filter_related(mut self, predicate: Expr<R::Target, bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.select.binds);
        self.related_filter = Some(Arc::new(match self.related_filter.take() {
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

    /// Appends one ordering key over the joined entity's columns.
    ///
    /// Related keys and source keys share one precedence list, in call
    /// order. Unmatched rows sort by `NULL` on related keys.
    #[must_use]
    pub fn order_by_related(mut self, key: OrderKey<R::Target>) -> Self {
        let mut spec = key.spec;
        // The joined row places the target's columns after the source's.
        spec.column += R::Source::COLUMNS.len();
        self.select.order.push(spec);
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
        QueryShape::for_join(&self.select, TypeId::of::<R>(), self.related_filter.clone())
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

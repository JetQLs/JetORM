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
        self.related_filter = Some(merge_and(self.related_filter.take(), normalized));
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

/// A select joined with two related entities, both edges from the same
/// source.
///
/// Created by chaining [`JoinSelect::also`]. Each fetched row carries the
/// source model and both edges' matches — `(Post, Option<User>,
/// Option<User>)` for the author and editor edges — through one statement
/// with two `LEFT JOIN`s. Everything the single join promises holds per
/// edge: null extension keeps unmatched rows, and to-many edges multiply
/// rows honestly.
#[derive(Clone)]
pub struct Join2Select<R1, R2>
where
    R1: Relation,
    R2: Relation<Source = R1::Source>,
{
    pub(crate) select: Select<R1::Source>,
    pub(crate) related_filter: Option<Arc<Predicate>>,
    pub(crate) second_filter: Option<Arc<Predicate>>,
    relations: PhantomData<fn() -> (R1, R2)>,
}

impl<R> JoinSelect<R>
where
    R: Relation,
{
    /// Joins a second relation from the same source entity.
    ///
    /// The dual-edge case in one query: both edges join the source row
    /// independently, so `find().also::<post::Author>()
    /// .also::<post::Editor>()` fetches `(Post, Option<User>,
    /// Option<User>)`.
    #[must_use]
    pub fn also<R2>(self) -> Join2Select<R, R2>
    where
        R2: Relation<Source = R::Source>,
    {
        Join2Select {
            select: self.select,
            related_filter: self.related_filter,
            second_filter: None,
            relations: PhantomData,
        }
    }
}

impl<R1, R2> Join2Select<R1, R2>
where
    R1: Relation,
    R2: Relation<Source = R1::Source>,
{
    /// Restricts rows to those satisfying a predicate over the source
    /// entity's columns.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<R1::Source, bool>) -> Self {
        self.select = self.select.filter(predicate);
        self
    }

    /// Restricts rows by the first joined entity's columns.
    ///
    /// Evaluates after null extension, exactly as
    /// [`JoinSelect::filter_related`] does.
    #[must_use]
    pub fn filter_related1(mut self, predicate: Expr<R1::Target, bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.select.binds);
        self.related_filter = Some(merge_and(self.related_filter.take(), normalized));
        self
    }

    /// Restricts rows by the second joined entity's columns.
    #[must_use]
    pub fn filter_related2(mut self, predicate: Expr<R2::Target, bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.select.binds);
        self.second_filter = Some(merge_and(self.second_filter.take(), normalized));
        self
    }

    /// Appends one ordering key over the source entity's columns.
    #[must_use]
    pub fn order_by(mut self, key: OrderKey<R1::Source>) -> Self {
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

    /// Consumes the query and returns its captured values in bind order.
    #[must_use]
    pub fn into_binds(self) -> Vec<Value> {
        self.select.into_binds()
    }

    /// Returns this query's value-independent shape.
    #[must_use]
    pub fn shape(&self) -> QueryShape {
        QueryShape::for_join2(
            &self.select,
            TypeId::of::<(R1, R2)>(),
            self.related_filter.clone(),
            self.second_filter.clone(),
        )
    }
}

/// Combines an optional existing predicate with a new one under `AND`.
fn merge_and(existing: Option<Arc<Predicate>>, normalized: Predicate) -> Arc<Predicate> {
    Arc::new(match existing {
        Some(previous) => Predicate::Binary {
            left: Box::new(Arc::try_unwrap(previous).unwrap_or_else(|shared| (*shared).clone())),
            op: BinaryOperator::And,
            right: Box::new(normalized),
        },
        None => normalized,
    })
}

impl<R1, R2> crate::select::CacheableQuery for Join2Select<R1, R2>
where
    R1: Relation,
    R2: Relation<Source = R1::Source>,
{
    fn shape(&self) -> QueryShape {
        Self::shape(self)
    }
}

impl<R1, R2> fmt::Debug for Join2Select<R1, R2>
where
    R1: Relation,
    R2: Relation<Source = R1::Source>,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Join2Select")
            .field("relations", &(R1::NAME, R2::NAME))
            .field("select", &self.select)
            .finish()
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

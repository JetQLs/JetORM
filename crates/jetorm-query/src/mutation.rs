//! Typed insert, update, delete, upsert, and returning builders.

use std::sync::Arc;
use std::{fmt, marker::PhantomData};

use afterburner::ir::BinaryOperator;
use jetorm_entity::{Column, Entity, Model, SqlValue, Value};

use crate::LoweringError;
use crate::expr::{Expr, Predicate, normalize};
use crate::select::{CacheableQuery, QueryShape, StatementKind};

/// Typed multi-row insert builder.
#[derive(Clone)]
pub struct Insert<E: Entity> {
    pub(crate) rows: Vec<Vec<Value>>,
    pub(crate) conflict: InsertConflict,
    pub(crate) returning: bool,
    entity: PhantomData<fn() -> E>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum InsertConflict {
    #[default]
    None,
    DoNothing,
    UpdateInserted,
    /// `DO NOTHING` restricted to an explicit column arbiter.
    TargetedIgnore {
        /// Conflict-target column positions within `E::COLUMNS`.
        target: Vec<usize>,
    },
    /// `DO UPDATE` on an explicit column arbiter, assigning the named
    /// columns from the excluded input row.
    TargetedUpdate {
        /// Conflict-target column positions within `E::COLUMNS`.
        target: Vec<usize>,
        /// Assigned column positions within `E::COLUMNS`.
        update: Vec<usize>,
    },
}

impl<E: Entity> Insert<E> {
    pub(crate) fn new(model: E::Model) -> Self {
        Self {
            rows: vec![model.into_values()],
            conflict: InsertConflict::None,
            returning: false,
            entity: PhantomData,
        }
    }

    /// Appends another model to the same `INSERT ... VALUES` statement.
    #[must_use]
    pub fn values(mut self, model: E::Model) -> Self {
        self.rows.push(model.into_values());
        self
    }

    /// Ignores primary-key conflicts.
    ///
    /// When the entity declares no primary key, every conflict is ignored.
    #[must_use]
    pub fn on_conflict_do_nothing(mut self) -> Self {
        self.conflict = InsertConflict::DoNothing;
        self
    }

    /// Updates inserted, non-key, non-auto-incrementing columns when the
    /// primary key conflicts.
    #[must_use]
    pub fn on_conflict_update(mut self) -> Self {
        self.conflict = InsertConflict::UpdateInserted;
        self
    }

    /// Chooses an explicit conflict arbiter from the given columns.
    ///
    /// The columns must be covered by a unique index or constraint on the
    /// live table — the database enforces this when the statement is
    /// prepared. Follow with [`OnConflict::ignore`] or
    /// [`OnConflict::update_columns`] to choose what a conflict does.
    #[must_use]
    pub fn on_conflict<T>(self, _target: T) -> OnConflict<E>
    where
        T: crate::projection::ColumnList<E>,
    {
        OnConflict {
            insert: self,
            target: T::indexes(),
        }
    }

    /// Returns complete entity rows affected by the insert or upsert.
    #[must_use]
    pub fn returning(mut self) -> Returning<Self> {
        self.returning = true;
        Returning::new(self)
    }

    /// Returns captured insert values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        let included = E::COLUMNS
            .iter()
            .enumerate()
            .filter(|(_, column)| !column.is_auto_increment())
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        self.rows
            .iter()
            .flat_map(|row| included.iter().filter_map(|index| row.get(*index).cloned()))
            .collect()
    }

    pub(crate) fn shape(&self) -> QueryShape {
        QueryShape::for_mutation::<E>(
            None,
            StatementKind::Insert {
                rows: self.rows.len(),
                conflict: self.conflict.clone(),
                returning: self.returning,
            },
        )
    }

    pub(crate) fn validate(&self) -> Result<(), LoweringError> {
        for (row, values) in self.rows.iter().enumerate() {
            if values.len() != E::COLUMNS.len() {
                return Err(LoweringError::ModelWidthMismatch {
                    row,
                    expected: E::COLUMNS.len(),
                    actual: values.len(),
                });
            }
            if let Some(column) = E::COLUMNS.iter().zip(values).find_map(|(column, value)| {
                (!column.is_auto_increment() && !column.is_nullable() && value.is_null())
                    .then_some(column)
            }) {
                return Err(LoweringError::NullForRequiredColumn {
                    column: column.name().to_owned(),
                });
            }
        }
        let included = E::COLUMNS
            .iter()
            .filter(|column| !column.is_auto_increment())
            .collect::<Vec<_>>();
        if included.is_empty() {
            return Err(LoweringError::EmptyInsert);
        }
        u32::try_from(self.rows.len()).map_err(|_| LoweringError::CapacityExceeded {
            detail: "insert row count",
        })?;
        let bind_count =
            self.rows
                .len()
                .checked_mul(included.len())
                .ok_or(LoweringError::CapacityExceeded {
                    detail: "insert bind count",
                })?;
        ensure_bind_capacity(bind_count)?;

        match &self.conflict {
            InsertConflict::None
            | InsertConflict::DoNothing
            | InsertConflict::TargetedIgnore { .. } => {}
            InsertConflict::UpdateInserted => {
                if E::PRIMARY_KEY.is_empty() {
                    return Err(LoweringError::MissingPrimaryKey);
                }
                // A generated key never appears in the inserted columns, so
                // arbitrating on it could never conflict — the update arm
                // would be dead code, silently.
                if let Some(column) = E::PRIMARY_KEY
                    .iter()
                    .map(|index| &E::COLUMNS[*index])
                    .find(|column| column.is_auto_increment())
                {
                    return Err(LoweringError::UpsertKeyGenerated {
                        column: column.name().to_owned(),
                    });
                }
                if included.iter().all(|column| column.is_primary_key()) {
                    return Err(LoweringError::EmptyUpsertUpdate);
                }
            }
            InsertConflict::TargetedUpdate { target, update } => {
                if update.is_empty() {
                    return Err(LoweringError::EmptyUpsertUpdate);
                }
                for index in update {
                    let column = &E::COLUMNS[*index];
                    // An excluded auto-increment value is the sequence's
                    // next number, never the caller's data; assigning it
                    // — or a conflict-target column onto itself — asks
                    // for something the statement cannot mean.
                    if column.is_auto_increment() {
                        return Err(LoweringError::UpsertAssignsGenerated {
                            column: column.name().to_owned(),
                        });
                    }
                    if target.contains(index) {
                        return Err(LoweringError::UpsertAssignsTarget {
                            column: column.name().to_owned(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

/// A chosen conflict arbiter awaiting its action.
///
/// Produced by [`Insert::on_conflict`]; finish with [`Self::ignore`] or
/// [`Self::update_columns`].
#[derive(Clone, Debug)]
pub struct OnConflict<E: Entity> {
    insert: Insert<E>,
    target: Vec<usize>,
}

impl<E: Entity> OnConflict<E> {
    /// Skips rows conflicting on the chosen arbiter.
    #[must_use]
    pub fn ignore(mut self) -> Insert<E> {
        self.insert.conflict = InsertConflict::TargetedIgnore {
            target: self.target,
        };
        self.insert
    }

    /// Assigns the given columns from the excluded input row when the
    /// chosen arbiter conflicts.
    #[must_use]
    pub fn update_columns<U>(mut self, _columns: U) -> Insert<E>
    where
        U: crate::projection::ColumnList<E>,
    {
        self.insert.conflict = InsertConflict::TargetedUpdate {
            target: self.target,
            update: U::indexes(),
        };
        self.insert
    }
}

impl<E: Entity> fmt::Debug for Insert<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Insert")
            .field("table", &E::TABLE.name())
            .field("rows", &self.rows.len())
            .field("conflict", &self.conflict)
            .field("returning", &self.returning)
            .finish()
    }
}

impl<E: Entity> CacheableQuery for Insert<E> {
    fn shape(&self) -> QueryShape {
        Insert::shape(self)
    }

    fn validate(&self) -> Result<(), LoweringError> {
        Insert::validate(self)
    }
}

/// Typed update builder with an explicit full-table safety gate.
#[derive(Clone)]
pub struct Update<E: Entity> {
    pub(crate) filter: Option<Arc<Predicate>>,
    pub(crate) assignments: Vec<(usize, usize)>,
    pub(crate) binds: Vec<Value>,
    pub(crate) all_rows: bool,
    pub(crate) returning: bool,
    entity: PhantomData<fn() -> E>,
}

impl<E: Entity> Update<E> {
    pub(crate) const fn new() -> Self {
        Self {
            filter: None,
            assignments: Vec::new(),
            binds: Vec::new(),
            all_rows: false,
            returning: false,
            entity: PhantomData,
        }
    }

    /// Assigns a non-null value to one entity column.
    #[must_use]
    pub fn set<C>(mut self, _column: C, value: impl Into<C::Rust>) -> Self
    where
        C: Column<Entity = E>,
    {
        let position = self.binds.len();
        self.binds.push(value.into().into_value());
        self.assignments.push((C::INDEX, position));
        self
    }

    /// Assigns SQL `NULL` to one entity column.
    ///
    /// Lowering rejects the statement when the selected column is required.
    #[must_use]
    pub fn set_null<C>(mut self, _column: C) -> Self
    where
        C: Column<Entity = E>,
    {
        let position = self.binds.len();
        self.binds.push(Value::Null(C::Rust::COLUMN_TYPE));
        self.assignments.push((C::INDEX, position));
        self
    }

    /// Restricts rows to those satisfying the predicate.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<E, bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.binds);
        self.filter = Some(Arc::new(match self.filter.take() {
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

    /// Explicitly authorizes updating every table row.
    #[must_use]
    pub const fn all_rows(mut self) -> Self {
        self.all_rows = true;
        self
    }

    /// Returns complete entity rows changed by the update.
    #[must_use]
    pub fn returning(mut self) -> Returning<Self> {
        self.returning = true;
        Returning::new(self)
    }

    /// Returns assignment and predicate values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> &[Value] {
        &self.binds
    }

    pub(crate) fn shape(&self) -> QueryShape {
        QueryShape::for_mutation::<E>(
            self.filter.clone(),
            StatementKind::Update {
                assignments: self.assignments.iter().map(|(index, _)| *index).collect(),
                all_rows: self.all_rows,
                returning: self.returning,
            },
        )
    }

    pub(crate) fn validate(&self) -> Result<(), LoweringError> {
        if self.assignments.is_empty() {
            return Err(LoweringError::EmptyUpdate);
        }
        require_bounded(self.filter.is_some(), self.all_rows)?;
        ensure_bind_capacity(self.binds.len())?;

        // This runs on every plan-cache lookup, hits included, so it must
        // not allocate: assignment lists are a handful of columns, where a
        // linear rescan beats a hash set outright.
        for (position_in_list, (index, position)) in self.assignments.iter().enumerate() {
            let column = &E::COLUMNS[*index];
            if self.assignments[..position_in_list]
                .iter()
                .any(|(previous, _)| previous == index)
            {
                return Err(LoweringError::DuplicateAssignment {
                    column: column.name().to_owned(),
                });
            }
            if self.binds[*position].is_null() && !column.is_nullable() {
                return Err(LoweringError::NullForRequiredColumn {
                    column: column.name().to_owned(),
                });
            }
        }
        Ok(())
    }
}

impl<E: Entity> fmt::Debug for Update<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Update")
            .field("table", &E::TABLE.name())
            .field("assignments", &self.assignments)
            .field("filter", &self.filter)
            .field("all_rows", &self.all_rows)
            .field("returning", &self.returning)
            .finish()
    }
}

impl<E: Entity> CacheableQuery for Update<E> {
    fn shape(&self) -> QueryShape {
        Update::shape(self)
    }

    fn validate(&self) -> Result<(), LoweringError> {
        Update::validate(self)
    }
}

/// Typed delete builder with an explicit full-table safety gate.
#[derive(Clone)]
pub struct Delete<E: Entity> {
    pub(crate) filter: Option<Arc<Predicate>>,
    pub(crate) binds: Vec<Value>,
    pub(crate) all_rows: bool,
    pub(crate) returning: bool,
    entity: PhantomData<fn() -> E>,
}

impl<E: Entity> Delete<E> {
    pub(crate) const fn new() -> Self {
        Self {
            filter: None,
            binds: Vec::new(),
            all_rows: false,
            returning: false,
            entity: PhantomData,
        }
    }

    /// Restricts rows to those satisfying the predicate.
    #[must_use]
    pub fn filter(mut self, predicate: Expr<E, bool>) -> Self {
        let normalized = normalize(predicate.node, &mut self.binds);
        self.filter = Some(Arc::new(match self.filter.take() {
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

    /// Explicitly authorizes deleting every table row.
    #[must_use]
    pub const fn all_rows(mut self) -> Self {
        self.all_rows = true;
        self
    }

    /// Returns complete entity rows removed by the delete.
    #[must_use]
    pub fn returning(mut self) -> Returning<Self> {
        self.returning = true;
        Returning::new(self)
    }

    /// Returns captured predicate values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> &[Value] {
        &self.binds
    }

    pub(crate) fn shape(&self) -> QueryShape {
        QueryShape::for_mutation::<E>(
            self.filter.clone(),
            StatementKind::Delete {
                all_rows: self.all_rows,
                returning: self.returning,
            },
        )
    }

    pub(crate) fn validate(&self) -> Result<(), LoweringError> {
        require_bounded(self.filter.is_some(), self.all_rows)?;
        ensure_bind_capacity(self.binds.len())
    }
}

impl<E: Entity> fmt::Debug for Delete<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Delete")
            .field("table", &E::TABLE.name())
            .field("filter", &self.filter)
            .field("all_rows", &self.all_rows)
            .field("returning", &self.returning)
            .finish()
    }
}

impl<E: Entity> CacheableQuery for Delete<E> {
    fn shape(&self) -> QueryShape {
        Delete::shape(self)
    }

    fn validate(&self) -> Result<(), LoweringError> {
        Delete::validate(self)
    }
}

/// Row-producing wrapper around a mutation with `RETURNING`.
#[derive(Clone, Debug)]
pub struct Returning<M> {
    pub(crate) mutation: M,
}

impl<M> Returning<M> {
    const fn new(mutation: M) -> Self {
        Self { mutation }
    }
}

impl<E: Entity> Returning<Insert<E>> {
    /// Returns captured insert values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> Vec<Value> {
        self.mutation.binds()
    }
}

impl<E: Entity> CacheableQuery for Returning<Insert<E>> {
    fn shape(&self) -> QueryShape {
        self.mutation.shape()
    }

    fn validate(&self) -> Result<(), LoweringError> {
        self.mutation.validate()
    }
}

impl<E: Entity> Returning<Update<E>> {
    /// Returns captured assignment and predicate values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> &[Value] {
        self.mutation.binds()
    }
}

impl<E: Entity> CacheableQuery for Returning<Update<E>> {
    fn shape(&self) -> QueryShape {
        self.mutation.shape()
    }

    fn validate(&self) -> Result<(), LoweringError> {
        self.mutation.validate()
    }
}

impl<E: Entity> Returning<Delete<E>> {
    /// Returns captured predicate values in positional bind order.
    #[must_use]
    pub fn binds(&self) -> &[Value] {
        self.mutation.binds()
    }
}

impl<E: Entity> CacheableQuery for Returning<Delete<E>> {
    fn shape(&self) -> QueryShape {
        self.mutation.shape()
    }

    fn validate(&self) -> Result<(), LoweringError> {
        self.mutation.validate()
    }
}

/// Mutation entry points available on every entity marker.
pub trait EntityMutation: Entity {
    /// Starts an insert with one model.
    #[must_use]
    fn insert(model: Self::Model) -> Insert<Self> {
        Insert::new(model)
    }

    /// Inserts the model, updating every non-key inserted column when the
    /// primary key conflicts.
    ///
    /// Shorthand for `insert(model).on_conflict_update()`; use
    /// [`Insert::on_conflict`] to arbitrate on other unique columns or to
    /// choose the updated columns.
    #[must_use]
    fn upsert(model: Self::Model) -> Insert<Self> {
        Insert::new(model).on_conflict_update()
    }

    /// Starts an update with no assignments and no selected rows.
    #[must_use]
    fn update() -> Update<Self> {
        Update::new()
    }

    /// Starts a delete with no selected rows.
    #[must_use]
    fn delete() -> Delete<Self> {
        Delete::new()
    }
}

impl<E: Entity> EntityMutation for E {}

fn require_bounded(filtered: bool, all_rows: bool) -> Result<(), LoweringError> {
    if filtered || all_rows {
        Ok(())
    } else {
        Err(LoweringError::UnboundedMutation)
    }
}

fn ensure_bind_capacity(count: usize) -> Result<(), LoweringError> {
    if count == 0 || u32::try_from(count - 1).is_ok() {
        Ok(())
    } else {
        Err(LoweringError::CapacityExceeded {
            detail: "bind position",
        })
    }
}

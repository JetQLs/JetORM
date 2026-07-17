use std::marker::PhantomData;

use crate::entity::{Column, Entity};

/// Referential action a foreign key takes when the referenced row changes.
///
/// The set mirrors standard SQL; unknown variants must be handled as the
/// dialect surface grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ReferentialAction {
    /// Reject the change at statement end.
    NoAction,
    /// Reject the change immediately.
    Restrict,
    /// Propagate the change to referencing rows.
    Cascade,
    /// Set referencing columns to SQL `NULL`.
    SetNull,
    /// Set referencing columns to their default values.
    SetDefault,
}

/// Constraint facts carried by the edge that owns a foreign key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ForeignKeyMeta {
    on_delete: ReferentialAction,
    on_update: ReferentialAction,
}

impl ForeignKeyMeta {
    /// Creates constraint metadata with explicit referential actions.
    #[must_use]
    pub const fn new(on_delete: ReferentialAction, on_update: ReferentialAction) -> Self {
        Self {
            on_delete,
            on_update,
        }
    }

    /// Returns the action taken when the referenced row is deleted.
    #[must_use]
    pub const fn on_delete(&self) -> ReferentialAction {
        self.on_delete
    }

    /// Returns the action taken when the referenced key is updated.
    #[must_use]
    pub const fn on_update(&self) -> ReferentialAction {
        self.on_update
    }
}

/// One named relation edge between two entities.
///
/// Implementors are zero-sized marker types, exactly like columns: the edge
/// from posts to their author is a type such as `post::Author`, so two
/// relations targeting one entity — or an entity relating to itself — stay
/// distinct where type-keyed designs collapse. The marker fixes the joined
/// column pair at the type level, which is what lets loaders and (later)
/// joins type-check their keys at compile time.
///
/// Relations join one column to one column today, consistent with
/// [`crate::SingleKeyEntity`]; composite keys extend the trait rather than
/// complicate every consumer now.
pub trait Relation: Copy + 'static {
    /// Entity on the referencing side of the edge.
    type Source: Entity;
    /// Entity on the referenced side of the edge.
    type Target: Entity;
    /// Referencing column, owned by [`Relation::Source`].
    type SourceColumn: Column<Entity = Self::Source>;
    /// Referenced column, owned by [`Relation::Target`].
    type TargetColumn: Column<Entity = Self::Target>;

    /// Stable name of the edge, unique within the source entity.
    const NAME: &'static str;

    /// Whether one source row matches at most one target row.
    ///
    /// True for the owning direction of a foreign key (the referenced
    /// column is unique); false for inverses.
    const TO_ONE: bool;

    /// Constraint carried by the source table, when this edge owns one.
    ///
    /// Inverse edges and purely logical associations carry `None`; schema
    /// diffing derives `FOREIGN KEY` clauses from edges that carry `Some`.
    const FOREIGN_KEY: Option<ForeignKeyMeta>;
}

/// The reverse direction of an existing relation, free of charge.
///
/// `Inverse<post::Author>` is the users-to-posts edge: same joined columns,
/// swapped sides, no constraint of its own. Deriving the forward edge is
/// therefore enough to traverse both ways.
pub struct Inverse<R>(PhantomData<fn() -> R>);

impl<R> Clone for Inverse<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for Inverse<R> {}

impl<R> Default for Inverse<R> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<R> std::fmt::Debug for Inverse<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Inverse")
    }
}

impl<R> Relation for Inverse<R>
where
    R: Relation,
{
    type Source = R::Target;
    type Target = R::Source;
    type SourceColumn = R::TargetColumn;
    type TargetColumn = R::SourceColumn;

    const NAME: &'static str = R::NAME;
    // A foreign key guarantees uniqueness of its referenced column, not of
    // its referencing one: walking the edge backwards can match many rows.
    const TO_ONE: bool = false;
    const FOREIGN_KEY: Option<ForeignKeyMeta> = None;
}

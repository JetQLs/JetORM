use super::{SchemaId, TableRef};

/// Target that selects a unique conflict arbiter for an `INSERT`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConflictTarget {
    /// Infers a unique index from one or more table columns.
    Columns(Vec<String>),
    /// Names an existing unique or exclusion constraint.
    Constraint(String),
}

/// One `target = EXCLUDED.source` assignment in an upsert action.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct UpsertAssignment {
    target: String,
    source: String,
}

impl UpsertAssignment {
    /// Creates an assignment from an excluded input column.
    #[must_use]
    pub fn new(target: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            source: source.into(),
        }
    }

    /// Returns the table column written by the conflict action.
    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the inserted column read through the excluded-row namespace.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Action taken when an inserted row conflicts with a unique arbiter.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum ConflictAction {
    /// Silently skips the conflicting row.
    DoNothing,
    /// Updates the existing row from the excluded input row.
    DoUpdate(Vec<UpsertAssignment>),
}

/// Complete dialect-independent `ON CONFLICT` description.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ConflictClause {
    target: Option<ConflictTarget>,
    action: ConflictAction,
}

impl ConflictClause {
    /// Creates `DO NOTHING`, optionally restricted to one conflict target.
    #[must_use]
    pub const fn do_nothing(target: Option<ConflictTarget>) -> Self {
        Self {
            target,
            action: ConflictAction::DoNothing,
        }
    }

    /// Creates `DO UPDATE` for one required conflict target.
    #[must_use]
    pub fn do_update(target: ConflictTarget, assignments: Vec<UpsertAssignment>) -> Self {
        Self {
            target: Some(target),
            action: ConflictAction::DoUpdate(assignments),
        }
    }

    /// Returns the optional unique conflict target.
    #[must_use]
    pub const fn target(&self) -> Option<&ConflictTarget> {
        self.target.as_ref()
    }

    /// Returns the action applied to a conflicting row.
    #[must_use]
    pub const fn action(&self) -> &ConflictAction {
        &self.action
    }
}

/// Built-in data-mutation dialect.
///
/// Mutations keep table-row typing in an interned `schema`. A mutation without
/// `returning` produces one [`super::Type::Unit`] result; otherwise it produces
/// one relation whose schema matches the named returning columns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MutationOp {
    /// Inserts one or more rows yielded by a value region.
    ///
    /// The operation owns one single-block region with no block arguments. Its
    /// terminator yields `rows * columns.len()` scalar values in row-major
    /// order.
    Insert {
        /// Destination table.
        table: TableRef,
        /// Complete destination row schema.
        schema: SchemaId,
        /// Inserted columns in value-region order.
        columns: Vec<String>,
        /// Number of inserted rows.
        rows: u32,
        /// Optional upsert policy.
        conflict: Option<ConflictClause>,
        /// Direct table columns emitted by `RETURNING`.
        returning: Vec<String>,
    },
    /// Updates rows selected by one expression region.
    ///
    /// The region block receives the complete table row. It yields one Boolean
    /// predicate followed by one scalar expression per assignment column.
    Update {
        /// Destination table.
        table: TableRef,
        /// Complete destination row schema.
        schema: SchemaId,
        /// Assigned columns aligned with yielded values after the predicate.
        assignments: Vec<String>,
        /// Direct table columns emitted by `RETURNING`.
        returning: Vec<String>,
    },
    /// Deletes rows selected by one Boolean expression region.
    ///
    /// The region block receives the complete table row and yields exactly one
    /// Boolean predicate.
    Delete {
        /// Destination table.
        table: TableRef,
        /// Complete destination row schema.
        schema: SchemaId,
        /// Direct table columns emitted by `RETURNING`.
        returning: Vec<String>,
    },
}

impl MutationOp {
    /// Returns the destination table.
    #[must_use]
    pub const fn table(&self) -> &TableRef {
        match self {
            Self::Insert { table, .. }
            | Self::Update { table, .. }
            | Self::Delete { table, .. } => table,
        }
    }

    /// Returns the complete destination row schema.
    #[must_use]
    pub const fn schema(&self) -> SchemaId {
        match self {
            Self::Insert { schema, .. }
            | Self::Update { schema, .. }
            | Self::Delete { schema, .. } => *schema,
        }
    }

    /// Returns direct table columns emitted by `RETURNING`.
    #[must_use]
    pub fn returning(&self) -> &[String] {
        match self {
            Self::Insert { returning, .. }
            | Self::Update { returning, .. }
            | Self::Delete { returning, .. } => returning,
        }
    }
}

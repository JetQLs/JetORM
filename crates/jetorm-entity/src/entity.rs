use crate::error::DecodeError;
use crate::meta::{ColumnMeta, TableMeta};
use crate::value::{SqlValue, Value};

/// Static description of one database table.
///
/// Implementors are zero-sized marker types; all metadata is `const` and
/// available without a database connection. The columns slice is ordered and
/// that order is semantic: it defines the positional row layout used by
/// [`Model`] conversion, IR relation schemas, and migration diffing.
pub trait Entity: Copy + 'static {
    /// Plain data struct holding one row of this entity.
    type Model: Model<Entity = Self>;

    /// Structured table identity.
    const TABLE: TableMeta;

    /// Ordered column metadata defining the positional row layout.
    const COLUMNS: &'static [ColumnMeta];

    /// Positions of primary-key columns within [`Self::COLUMNS`].
    const PRIMARY_KEY: &'static [usize];
}

/// Positional conversion between a row struct and runtime values.
///
/// Both directions follow [`Entity::COLUMNS`] order exactly. The trait keeps
/// executors independent from concrete drivers: a driver row is first decoded
/// into positional [`Value`]s and then into the user's struct.
pub trait Model: Sized {
    /// Entity described by this row struct.
    type Entity: Entity<Model = Self>;

    /// Converts the row into positional values in column order.
    fn into_values(self) -> Vec<Value>;

    /// Reconstructs the row from positional values in column order.
    ///
    /// # Errors
    ///
    /// Returns an error when the width or a payload kind does not match the
    /// entity's column metadata.
    fn from_values(values: Vec<Value>) -> Result<Self, DecodeError>;

    /// Returns one column's value by position, or `None` out of range.
    ///
    /// Relation loaders read join keys through this without consuming the
    /// row; write paths will read changed columns the same way.
    fn value(&self, column: usize) -> Option<Value>;
}

/// Entities whose primary key is exactly one column.
///
/// `#[derive(JetModel)]` implements this automatically for single-column
/// keys; composite-key entities do not implement it and therefore have no
/// by-id lookup until composite key support lands. The marker's `Default`
/// bound lets generic code conjure the column to build predicates.
pub trait SingleKeyEntity: Entity {
    /// Marker of the primary-key column.
    type PrimaryKeyColumn: Column<Entity = Self> + Default;
}

/// Zero-sized marker for one entity column.
///
/// The marker carries the column's Rust type, so expression builders accept
/// only compatible operand types at compile time. For an `Option<T>` field
/// the marker's [`Column::Rust`] is the inner `T`; nullability is carried by
/// [`Column::NULLABLE`] and mirrored in the column metadata.
pub trait Column: Copy + 'static {
    /// Entity owning this column.
    type Entity: Entity;

    /// Non-optional Rust type stored in this column.
    type Rust: SqlValue;

    /// Full Rust type of the model field: `Option<Rust>` when the column is
    /// nullable, `Rust` otherwise. Projections decode through this type, so
    /// a projected nullable column comes back as an `Option` rather than a
    /// panic on NULL.
    type Field: SqlValue;

    /// Position of this column within [`Entity::COLUMNS`].
    const INDEX: usize;

    /// Whether the column stores SQL `NULL` (an `Option` field in Rust).
    const NULLABLE: bool;

    /// Returns this column's metadata entry.
    #[must_use]
    fn meta() -> &'static ColumnMeta {
        &<Self::Entity as Entity>::COLUMNS[Self::INDEX]
    }
}

/// Structured table identity retained independently from rendered SQL.
///
/// The optional schema component maps onto the database's namespace concept
/// (for example a PostgreSQL schema). Catalog qualification can be added here
/// without breaking the frontend contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TableMeta {
    schema: Option<&'static str>,
    name: &'static str,
}

impl TableMeta {
    /// Creates an unqualified table identity.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self { schema: None, name }
    }

    /// Clones this identity with a database schema qualifier.
    #[must_use]
    pub const fn with_schema(mut self, schema: &'static str) -> Self {
        self.schema = Some(schema);
        self
    }

    /// Returns the optional database schema qualifier.
    #[must_use]
    pub const fn schema(&self) -> Option<&'static str> {
        self.schema
    }

    /// Returns the required table name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }
}

/// Dialect-independent column type carried by entity metadata.
///
/// The set is intentionally narrower than a database's full type system: it
/// covers the types JetORM maps onto Rust field types today. Every variant is
/// `Copy` and constructible in `const` context so derive macros can emit
/// whole-table metadata as constants. Temporal types use microsecond
/// precision, matching both `chrono`'s lossless range and PostgreSQL storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColumnType {
    /// SQL boolean.
    Boolean,
    /// 16-bit signed integer.
    Int16,
    /// 32-bit signed integer.
    Int32,
    /// 64-bit signed integer.
    Int64,
    /// 32-bit IEEE floating point.
    Float32,
    /// 64-bit IEEE floating point.
    Float64,
    /// Unicode text.
    Text,
    /// Opaque byte sequence.
    Bytes,
    /// Calendar date without a time zone.
    Date,
    /// Time of day without a date, microsecond precision.
    Time,
    /// Date and time without time-zone semantics, microsecond precision.
    Timestamp,
    /// Date and time in Coordinated Universal Time, microsecond precision.
    TimestampUtc,
    /// Universally unique identifier.
    Uuid,
    /// Structured JSON document.
    Json,
}

/// Static description of one entity column.
///
/// Column metadata is ordered: an entity's [`crate::Entity::COLUMNS`] slice
/// defines positional row layout for [`crate::Model`] conversion and for the
/// relation schemas produced during IR lowering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ColumnMeta {
    name: &'static str,
    rust_name: &'static str,
    column_type: ColumnType,
    nullable: bool,
    primary_key: bool,
    auto_increment: bool,
    unique: bool,
}

impl ColumnMeta {
    /// Creates non-nullable, non-key column metadata.
    #[must_use]
    pub const fn new(name: &'static str, rust_name: &'static str, column_type: ColumnType) -> Self {
        Self {
            name,
            rust_name,
            column_type,
            nullable: false,
            primary_key: false,
            auto_increment: false,
            unique: false,
        }
    }

    /// Clones this metadata allowing SQL `NULL` values.
    #[must_use]
    pub const fn nullable(mut self) -> Self {
        self.nullable = true;
        self
    }

    /// Clones this metadata as a primary-key member.
    #[must_use]
    pub const fn primary_key(mut self) -> Self {
        self.primary_key = true;
        self
    }

    /// Clones this metadata as database-generated on insert.
    #[must_use]
    pub const fn auto_increment(mut self) -> Self {
        self.auto_increment = true;
        self
    }

    /// Clones this metadata with a uniqueness constraint.
    #[must_use]
    pub const fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    /// Returns the SQL column name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// Returns the Rust field name on the model struct.
    #[must_use]
    pub const fn rust_name(&self) -> &'static str {
        self.rust_name
    }

    /// Returns the dialect-independent column type.
    #[must_use]
    pub const fn column_type(&self) -> ColumnType {
        self.column_type
    }

    /// Reports whether SQL `NULL` is a valid stored value.
    #[must_use]
    pub const fn is_nullable(&self) -> bool {
        self.nullable
    }

    /// Reports whether this column participates in the primary key.
    #[must_use]
    pub const fn is_primary_key(&self) -> bool {
        self.primary_key
    }

    /// Reports whether the database generates this column's value on insert.
    #[must_use]
    pub const fn is_auto_increment(&self) -> bool {
        self.auto_increment
    }

    /// Reports whether this column carries a uniqueness constraint.
    #[must_use]
    pub const fn is_unique(&self) -> bool {
        self.unique
    }
}

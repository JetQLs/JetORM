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
///
/// The type is deliberately exhaustive: downstream code matching on it should
/// stop compiling when JetORM learns a new type, rather than silently taking
/// a fallback branch.
///
/// Serialized as its variant name, with arrays spelled `"Element[]"` — the
/// form migration TOML files use.
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
    /// Exact arbitrary-precision decimal (`numeric` without a declared
    /// precision, so no stored value is ever rounded by the type).
    Decimal,
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
    /// Array of one scalar element type.
    ///
    /// Deliberately flat: the element enum names only scalars, so arrays
    /// never nest — matching what entity fields can express (`Vec<T>` of
    /// a scalar `T`).
    ArrayOf(ElementType),
}

/// Scalar element types an array column can hold.
///
/// Bytes and JSON are absent: `Vec<u8>` already is the bytes column, and
/// JSON documents hold their own arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ElementType {
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
    /// Exact arbitrary-precision decimal.
    Decimal,
    /// Unicode text.
    Text,
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
}

impl ElementType {
    /// Returns the element's own column type.
    #[must_use]
    pub const fn as_column_type(self) -> ColumnType {
        match self {
            Self::Boolean => ColumnType::Boolean,
            Self::Int16 => ColumnType::Int16,
            Self::Int32 => ColumnType::Int32,
            Self::Int64 => ColumnType::Int64,
            Self::Float32 => ColumnType::Float32,
            Self::Float64 => ColumnType::Float64,
            Self::Decimal => ColumnType::Decimal,
            Self::Text => ColumnType::Text,
            Self::Date => ColumnType::Date,
            Self::Time => ColumnType::Time,
            Self::Timestamp => ColumnType::Timestamp,
            Self::TimestampUtc => ColumnType::TimestampUtc,
            Self::Uuid => ColumnType::Uuid,
        }
    }

    /// Returns the element's serialized spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Boolean => "Boolean",
            Self::Int16 => "Int16",
            Self::Int32 => "Int32",
            Self::Int64 => "Int64",
            Self::Float32 => "Float32",
            Self::Float64 => "Float64",
            Self::Decimal => "Decimal",
            Self::Text => "Text",
            Self::Date => "Date",
            Self::Time => "Time",
            Self::Timestamp => "Timestamp",
            Self::TimestampUtc => "TimestampUtc",
            Self::Uuid => "Uuid",
        }
    }

    /// Parses the serialized spelling produced by [`Self::name`].
    #[must_use]
    fn parse(spelling: &str) -> Option<Self> {
        Some(match spelling {
            "Boolean" => Self::Boolean,
            "Int16" => Self::Int16,
            "Int32" => Self::Int32,
            "Int64" => Self::Int64,
            "Float32" => Self::Float32,
            "Float64" => Self::Float64,
            "Decimal" => Self::Decimal,
            "Text" => Self::Text,
            "Date" => Self::Date,
            "Time" => Self::Time,
            "Timestamp" => Self::Timestamp,
            "TimestampUtc" => Self::TimestampUtc,
            "Uuid" => Self::Uuid,
            _ => return None,
        })
    }
}

impl ColumnType {
    /// Returns the serialized spelling: the variant name, with arrays as
    /// `"Element[]"`.
    #[must_use]
    pub fn spelling(self) -> std::borrow::Cow<'static, str> {
        use std::borrow::Cow;
        Cow::Borrowed(match self {
            Self::Boolean => "Boolean",
            Self::Int16 => "Int16",
            Self::Int32 => "Int32",
            Self::Int64 => "Int64",
            Self::Float32 => "Float32",
            Self::Float64 => "Float64",
            Self::Decimal => "Decimal",
            Self::Text => "Text",
            Self::Bytes => "Bytes",
            Self::Date => "Date",
            Self::Time => "Time",
            Self::Timestamp => "Timestamp",
            Self::TimestampUtc => "TimestampUtc",
            Self::Uuid => "Uuid",
            Self::Json => "Json",
            Self::ArrayOf(element) => {
                return Cow::Owned(format!("{}[]", element.name()));
            }
        })
    }

    fn parse(spelling: &str) -> Option<Self> {
        if let Some(base) = spelling.strip_suffix("[]") {
            return ElementType::parse(base).map(Self::ArrayOf);
        }
        Some(match spelling {
            "Boolean" => Self::Boolean,
            "Int16" => Self::Int16,
            "Int32" => Self::Int32,
            "Int64" => Self::Int64,
            "Float32" => Self::Float32,
            "Float64" => Self::Float64,
            "Decimal" => Self::Decimal,
            "Text" => Self::Text,
            "Bytes" => Self::Bytes,
            "Date" => Self::Date,
            "Time" => Self::Time,
            "Timestamp" => Self::Timestamp,
            "TimestampUtc" => Self::TimestampUtc,
            "Uuid" => Self::Uuid,
            "Json" => Self::Json,
            _ => return None,
        })
    }
}

impl serde::Serialize for ColumnType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.spelling())
    }
}

impl<'de> serde::Deserialize<'de> for ColumnType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spelling = <std::borrow::Cow<'_, str>>::deserialize(deserializer)?;
        Self::parse(&spelling)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown column type `{spelling}`")))
    }
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
    type_name: Option<&'static str>,
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
            type_name: None,
            nullable: false,
            primary_key: false,
            auto_increment: false,
            unique: false,
        }
    }

    /// Clones this metadata backed by a named database type.
    #[must_use]
    pub const fn with_type_name(mut self, type_name: Option<&'static str>) -> Self {
        self.type_name = type_name;
        self
    }

    /// Returns the named database type backing this column, when any.
    #[must_use]
    pub const fn type_name(&self) -> Option<&'static str> {
        self.type_name
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

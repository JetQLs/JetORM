use std::fmt;

use super::SchemaId;

/// Static type assigned to an SSA value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Type {
    /// One SQL scalar with explicit nullability.
    Scalar(ScalarType),
    /// Ordered product type used by dialect extensions and lowering stages.
    Tuple(Vec<Type>),
    /// Row-producing relation described by an interned schema.
    Relation(SchemaId),
    /// Unit value used by dialect operations without a material SQL result.
    Unit,
}

impl Type {
    /// Creates a scalar SQL value type.
    #[must_use]
    pub const fn scalar(kind: SqlType, nullable: bool) -> Self {
        Self::Scalar(ScalarType::new(kind, nullable))
    }

    /// Creates a SQL boolean with explicit nullability.
    #[must_use]
    pub const fn boolean(nullable: bool) -> Self {
        Self::Scalar(ScalarType::new(SqlType::Boolean, nullable))
    }

    /// Creates a relation type referencing an interned row schema.
    #[must_use]
    pub const fn relation(schema: SchemaId) -> Self {
        Self::Relation(schema)
    }

    /// Returns the scalar descriptor when this is a scalar value.
    #[must_use]
    pub const fn as_scalar(&self) -> Option<&ScalarType> {
        match self {
            Self::Scalar(scalar) => Some(scalar),
            Self::Tuple(_) | Self::Relation(_) | Self::Unit => None,
        }
    }

    /// Returns the schema handle when this is a relation value.
    #[must_use]
    pub const fn as_relation(&self) -> Option<SchemaId> {
        match self {
            Self::Relation(schema) => Some(*schema),
            Self::Scalar(_) | Self::Tuple(_) | Self::Unit => None,
        }
    }

    /// Reports whether this type or a nested tuple element is relational.
    #[must_use]
    pub fn contains_relation(&self) -> bool {
        match self {
            Self::Relation(_) => true,
            Self::Tuple(elements) => elements.iter().any(Self::contains_relation),
            Self::Scalar(_) | Self::Unit => false,
        }
    }
}

/// SQL scalar kind paired with explicit nullability.
///
/// Nullability is part of the SSA type because SQL predicates and arithmetic use
/// three-valued logic. Transformations must preserve it unless they can prove a
/// stronger type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScalarType {
    kind: SqlType,
    nullable: bool,
}

impl ScalarType {
    /// Creates a scalar descriptor from its SQL kind and nullability.
    #[must_use]
    pub const fn new(kind: SqlType, nullable: bool) -> Self {
        Self { kind, nullable }
    }

    /// Returns the dialect-independent SQL kind.
    #[must_use]
    pub const fn kind(&self) -> &SqlType {
        &self.kind
    }

    /// Reports whether SQL `NULL` is a valid value.
    #[must_use]
    pub const fn is_nullable(&self) -> bool {
        self.nullable
    }

    /// Clones this descriptor with new nullability.
    #[must_use]
    pub fn with_nullability(&self, nullable: bool) -> Self {
        Self {
            kind: self.kind.clone(),
            nullable,
        }
    }

    /// Reports whether this is a SQL boolean.
    #[must_use]
    pub const fn is_boolean(&self) -> bool {
        matches!(self.kind, SqlType::Boolean)
    }
}

/// Dialect-independent SQL scalar kinds retained across frontend lowering.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SqlType {
    /// Three-valued SQL boolean.
    Boolean,
    /// Fixed-width integer.
    Integer {
        /// Storage width in bits.
        bits: u16,
        /// Whether negative values are representable.
        signed: bool,
    },
    /// IEEE-style floating point.
    Float {
        /// Storage width in bits.
        bits: u16,
    },
    /// Exact fixed-point decimal.
    Decimal {
        /// Total significant decimal digits.
        precision: u16,
        /// Digits to the right of the decimal point; negative scales are allowed.
        scale: i16,
    },
    /// Unicode text.
    Utf8,
    /// Opaque byte sequence.
    Binary,
    /// Calendar date without a time zone.
    Date,
    /// Time of day without a date.
    Time {
        /// Fractional-second decimal digits.
        precision: u8,
    },
    /// Date and time with explicit zone semantics.
    Timestamp {
        /// Fractional-second decimal digits.
        precision: u8,
        /// Time-zone interpretation.
        timezone: TimeZone,
    },
    /// Calendar and clock duration.
    Interval,
    /// Universally unique identifier.
    Uuid,
    /// Structured JSON value.
    Json,
    /// A frontend or database-specific scalar retained until dialect lowering.
    Custom(String),
}

/// Compiler-oriented alias for [`SqlType`].
pub type ScalarKind = SqlType;

/// Time-zone interpretation attached to a timestamp type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum TimeZone {
    /// No time-zone conversion semantics.
    Naive,
    /// Coordinated Universal Time.
    Utc,
    /// Named IANA or backend-specific time zone.
    Named(String),
}

/// Named row field whose position is semantically significant.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Field {
    name: String,
    ty: Type,
}

impl Field {
    /// Creates a named row field.
    #[must_use]
    pub fn new(name: impl Into<String>, ty: Type) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }

    /// Returns the field name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field value type.
    #[must_use]
    pub const fn ty(&self) -> &Type {
        &self.ty
    }
}

/// Ordered row schema interned by a [`super::Module`].
///
/// Field order defines relation column order and the block-argument order of
/// logical expression regions. The verifier rejects duplicate field names and
/// relation-typed fields.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Schema {
    fields: Vec<Field>,
}

impl Schema {
    /// Creates an ordered row schema.
    #[must_use]
    pub fn new(fields: impl Into<Vec<Field>>) -> Self {
        Self {
            fields: fields.into(),
        }
    }

    /// Returns fields in column order.
    #[must_use]
    pub fn fields(&self) -> &[Field] {
        &self.fields
    }

    /// Clones field types in column order for block-argument construction.
    #[must_use]
    pub fn field_types(&self) -> Vec<Type> {
        self.fields.iter().map(|field| field.ty.clone()).collect()
    }

    /// Returns the number of fields.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Reports whether the row has no fields.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Structured table identity retained independently from rendered SQL spelling.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TableRef {
    catalog: Option<String>,
    schema: Option<String>,
    name: String,
}

impl TableRef {
    /// Creates an unqualified table reference.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: None,
            name: name.into(),
        }
    }

    /// Creates a fully or partially qualified table reference.
    #[must_use]
    pub fn qualified(
        catalog: Option<impl Into<String>>,
        schema: Option<impl Into<String>>,
        name: impl Into<String>,
    ) -> Self {
        Self {
            catalog: catalog.map(Into::into),
            schema: schema.map(Into::into),
            name: name.into(),
        }
    }

    /// Returns the optional catalog component.
    #[must_use]
    pub fn catalog(&self) -> Option<&str> {
        self.catalog.as_deref()
    }

    /// Returns the optional schema component.
    #[must_use]
    pub fn schema(&self) -> Option<&str> {
        self.schema.as_deref()
    }

    /// Returns the required table name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Function identity retained before backend-specific symbol resolution.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FunctionRef {
    namespace: Option<String>,
    name: String,
}

impl FunctionRef {
    /// Creates an unqualified function reference.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            namespace: None,
            name: name.into(),
        }
    }

    /// Creates a namespace-qualified function reference.
    #[must_use]
    pub fn qualified(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: Some(namespace.into()),
            name: name.into(),
        }
    }

    /// Returns the optional function namespace.
    #[must_use]
    pub fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    /// Returns the function name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Bit-preserving `f64` payload suitable for equality and structural hashing.
///
/// Raw bits keep NaN payloads and signed zero distinct, avoiding host floating-
/// point equality rules during constant interning and fingerprinting.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FloatBits(u64);

impl FloatBits {
    /// Creates a float literal from raw IEEE-754 bits.
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// Captures an `f64` without canonicalizing NaNs or signed zero.
    #[must_use]
    pub fn from_f64(value: f64) -> Self {
        Self(value.to_bits())
    }

    /// Returns the raw IEEE-754 bits.
    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Reconstructs the corresponding `f64`.
    #[must_use]
    pub fn to_f64(self) -> f64 {
        f64::from_bits(self.0)
    }
}

/// Literal payload whose concrete SQL type is supplied by its SSA result.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Literal {
    /// Untyped SQL `NULL`; the result type supplies its concrete scalar kind.
    Null,
    /// Boolean literal.
    Boolean(bool),
    /// Signed integer literal.
    Integer(i128),
    /// Unsigned integer literal.
    Unsigned(u128),
    /// Bit-preserving floating-point literal.
    Float(FloatBits),
    /// Exact decimal literal.
    Decimal {
        /// Unscaled integer coefficient.
        coefficient: i128,
        /// Decimal scale applied to the coefficient.
        scale: i16,
    },
    /// Unicode text literal.
    String(String),
    /// Binary literal.
    Bytes(Vec<u8>),
    /// Days since the Unix epoch.
    Date(i32),
    /// Nanoseconds since midnight.
    Time(i64),
    /// Microseconds since the Unix epoch.
    Timestamp(i64),
    /// Calendar-and-clock interval literal.
    Interval {
        /// Calendar month component.
        months: i32,
        /// Calendar day component.
        days: i32,
        /// Sub-day nanosecond component.
        nanos: i64,
    },
    /// UUID literal in network byte order.
    Uuid([u8; 16]),
    /// Canonical JSON text produced by the frontend.
    Json(String),
}

/// Result-stability contract for function-like operations.
///
/// Volatility does not describe errors or other side effects; callers must also
/// consult the operation's [`super::EffectSet`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Volatility {
    /// Equal arguments produce equal results across statements.
    Immutable,
    /// Equal arguments produce equal results within one statement execution.
    Stable,
    /// Repeated evaluation may produce a different result.
    Volatile,
}

/// Half-open byte range retained for diagnostics and source mapping.
///
/// Source provenance does not affect query semantics and is excluded from
/// structural fingerprints.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SourceSpan {
    source: String,
    start: u32,
    end: u32,
}

impl SourceSpan {
    /// Creates a half-open byte range in a named source.
    #[must_use]
    pub fn new(source: impl Into<String>, start: u32, end: u32) -> Self {
        Self {
            source: source.into(),
            start,
            end,
        }
    }

    /// Returns the source name or URI.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Returns the inclusive starting byte offset.
    #[must_use]
    pub const fn start(&self) -> u32 {
        self.start
    }

    /// Returns the exclusive ending byte offset.
    #[must_use]
    pub const fn end(&self) -> u32 {
        self.end
    }
}

impl fmt::Display for TableRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(catalog) = &self.catalog {
            write!(formatter, "{catalog}.")?;
        }
        if let Some(schema) = &self.schema {
            write!(formatter, "{schema}.")?;
        }
        formatter.write_str(&self.name)
    }
}

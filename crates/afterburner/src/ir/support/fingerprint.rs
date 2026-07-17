use std::{
    collections::{BTreeMap, HashMap, HashSet},
    error::Error,
    fmt,
};

use super::{
    Attribute, BinaryOperator, BlockId, ExtensionOp, FunctionRef, JoinKind, Literal, LogicalOp,
    Module, NullOrder, OperationId, OperationKind, ScalarOp, SchemaId, SetOperator, SortDirection,
    SortKey, SqlType, TerminatorOp, TimeZone, Type, UnaryOperator, ValueId, VerificationError,
    Volatility, verify_module,
};

/// Deterministic semantic identity for profile admission and incremental caches.
///
/// Arena slots, source spans, profile-site ids, and the module edit revision are
/// intentionally excluded. Operation order, value flow, types, attributes, and
/// dialect payloads are included. Fingerprints are comparable only when produced
/// by the same fingerprint format version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StructuralFingerprint([u8; 16]);

impl StructuralFingerprint {
    /// Creates a fingerprint from an externally stored byte representation.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the portable big-endian byte representation.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl fmt::Display for StructuralFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Failure to construct a semantic fingerprint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FingerprintError {
    /// The module failed ordinary IR verification.
    InvalidModule(Vec<VerificationError>),
    /// A verified traversal encountered an inconsistent internal reference.
    InconsistentReference(String),
}

impl fmt::Display for FingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidModule(errors) => {
                write!(
                    formatter,
                    "cannot fingerprint invalid IR ({} errors)",
                    errors.len()
                )
            }
            Self::InconsistentReference(message) => formatter.write_str(message),
        }
    }
}

impl Error for FingerprintError {}

/// Computes an arena-id-independent semantic fingerprint for a module.
///
/// The module is verified before traversal. Definitions and blocks receive
/// canonical numbers from semantic traversal order, so allocation history does
/// not affect the result.
///
/// This is a versioned 128-bit FNV-1a digest, not a cryptographic digest. It is
/// intended to reject stale or structurally incompatible PGO data cheaply.
/// Security-sensitive artifact authentication requires a separate signature or
/// message-authentication code.
///
/// # Errors
///
/// Returns [`FingerprintError::InvalidModule`] when verification fails.
pub fn structural_fingerprint(module: &Module) -> Result<StructuralFingerprint, FingerprintError> {
    verify_module(module).map_err(FingerprintError::InvalidModule)?;
    let mut context = FingerprintContext {
        module,
        hasher: StableHasher::new(),
        canonical_values: HashMap::new(),
        canonical_blocks: HashMap::new(),
        next_value: 0,
        next_block: 0,
        schema_stack: HashSet::new(),
    };
    // v2: Limit encodes operand-presence booleans instead of inline counts.
    context.hasher.bytes(b"afterburner-ir-v2");
    context.hash_region(module.root_region())?;
    Ok(StructuralFingerprint(context.hasher.finish().to_be_bytes()))
}

struct FingerprintContext<'module> {
    module: &'module Module,
    hasher: StableHasher,
    // These maps replace generational arena handles with ids assigned in
    // definition/traversal order. Allocation and deletion history then disappear
    // from the byte stream.
    canonical_values: HashMap<ValueId, u64>,
    canonical_blocks: HashMap<BlockId, u64>,
    next_value: u64,
    next_block: u64,
    schema_stack: HashSet<SchemaId>,
}

impl FingerprintContext<'_> {
    fn hash_region(&mut self, region_id: super::RegionId) -> Result<(), FingerprintError> {
        let region = self.module.region(region_id).ok_or_else(|| {
            FingerprintError::InconsistentReference(format!("stale region {region_id}"))
        })?;
        self.hasher.tag(1);
        self.hasher.usize(region.blocks().len());
        // Register the whole region before hashing any block. Terminators may
        // reference forward successors that have not yet been traversed.
        for block_id in region.blocks() {
            self.canonical_blocks.entry(*block_id).or_insert_with(|| {
                let canonical = self.next_block;
                self.next_block += 1;
                canonical
            });
        }
        for block_id in region.blocks() {
            self.hash_block(*block_id)?;
        }
        Ok(())
    }

    fn hash_block(&mut self, block_id: BlockId) -> Result<(), FingerprintError> {
        let block = self.module.block(block_id).ok_or_else(|| {
            FingerprintError::InconsistentReference(format!("stale block {block_id}"))
        })?;
        self.hasher.tag(2);
        self.hasher.u64(self.canonical_block(block_id)?);
        self.hasher.usize(block.arguments().len());
        for argument in block.arguments() {
            let value = self.module.value(*argument).ok_or_else(|| {
                FingerprintError::InconsistentReference(format!("stale value {argument}"))
            })?;
            self.assign_value(*argument);
            self.hash_type(value.ty())?;
        }
        self.hasher.usize(block.operations().len());
        for operation in block.operations() {
            self.hash_operation(*operation)?;
        }
        Ok(())
    }

    fn hash_operation(&mut self, operation_id: OperationId) -> Result<(), FingerprintError> {
        let operation = self.module.operation(operation_id).ok_or_else(|| {
            FingerprintError::InconsistentReference(format!("stale operation {operation_id}"))
        })?;
        self.hasher.tag(3);
        self.hash_operation_kind(operation.kind())?;
        self.hasher.usize(operation.operands().len());
        for operand in operation.operands() {
            let canonical = self.canonical_values.get(operand).copied().ok_or_else(|| {
                FingerprintError::InconsistentReference(format!(
                    "value {operand} was used before canonical definition"
                ))
            })?;
            self.hasher.u64(canonical);
        }
        self.hasher.usize(operation.results().len());
        for result in operation.results() {
            let value = self.module.value(*result).ok_or_else(|| {
                FingerprintError::InconsistentReference(format!("stale value {result}"))
            })?;
            self.assign_value(*result);
            self.hash_type(value.ty())?;
        }
        self.hasher.usize(operation.successors().len());
        for successor in operation.successors() {
            self.hasher.u64(self.canonical_block(*successor)?);
        }
        // Attributes affect query semantics. Source spans and profile-site ids
        // are operational metadata and are intentionally absent from this stream.
        self.hash_attributes(operation.metadata().attributes())?;
        self.hasher.usize(operation.regions().len());
        for region in operation.regions() {
            self.hash_region(*region)?;
        }
        Ok(())
    }

    fn assign_value(&mut self, value: ValueId) {
        // Verification guarantees definitions precede all permitted uses in the
        // semantic walk, so first assignment is also canonical definition order.
        self.canonical_values.entry(value).or_insert_with(|| {
            let canonical = self.next_value;
            self.next_value += 1;
            canonical
        });
    }

    fn canonical_block(&self, block: BlockId) -> Result<u64, FingerprintError> {
        self.canonical_blocks.get(&block).copied().ok_or_else(|| {
            FingerprintError::InconsistentReference(format!(
                "successor {block} is outside its canonical region"
            ))
        })
    }

    fn hash_operation_kind(&mut self, kind: &OperationKind) -> Result<(), FingerprintError> {
        match kind {
            OperationKind::Logical(logical) => {
                self.hasher.tag(10);
                self.hash_logical(logical);
            }
            OperationKind::Scalar(scalar) => {
                self.hasher.tag(11);
                self.hash_scalar(scalar)?;
            }
            OperationKind::Terminator(terminator) => {
                self.hasher.tag(12);
                self.hasher.tag(match terminator {
                    TerminatorOp::Yield => 0,
                    TerminatorOp::QueryReturn => 1,
                });
            }
            OperationKind::Extension(extension) => {
                self.hasher.tag(13);
                self.hash_extension(extension);
            }
        }
        Ok(())
    }

    fn hash_logical(&mut self, logical: &LogicalOp) {
        match logical {
            LogicalOp::Scan { table, columns } => {
                self.hasher.tag(0);
                self.hasher.optional_str(table.catalog());
                self.hasher.optional_str(table.schema());
                self.hasher.string(table.name());
                self.hasher.usize(columns.len());
                for column in columns {
                    self.hasher.string(column);
                }
            }
            LogicalOp::Values { rows } => {
                self.hasher.tag(1);
                self.hasher.usize(rows.len());
                for row in rows {
                    self.hasher.usize(row.len());
                    for literal in row {
                        hash_literal(&mut self.hasher, literal);
                    }
                }
            }
            LogicalOp::Empty => self.hasher.tag(2),
            LogicalOp::Filter => self.hasher.tag(3),
            LogicalOp::Project => self.hasher.tag(4),
            LogicalOp::Join {
                kind,
                has_condition,
            } => {
                self.hasher.tag(5);
                self.hasher.tag(join_tag(*kind));
                self.hasher.boolean(*has_condition);
            }
            LogicalOp::Aggregate => self.hasher.tag(6),
            LogicalOp::Window => self.hasher.tag(7),
            LogicalOp::Sort { keys } => {
                self.hasher.tag(8);
                self.hasher.usize(keys.len());
                for key in keys {
                    hash_sort_key(&mut self.hasher, *key);
                }
            }
            LogicalOp::Limit {
                has_offset,
                has_fetch,
            } => {
                self.hasher.tag(9);
                self.hasher.boolean(*has_offset);
                self.hasher.boolean(*has_fetch);
            }
            LogicalOp::Distinct => self.hasher.tag(10),
            LogicalOp::Set { operator, all } => {
                self.hasher.tag(11);
                self.hasher.tag(set_tag(*operator));
                self.hasher.boolean(*all);
            }
        }
    }

    fn hash_scalar(&mut self, scalar: &ScalarOp) -> Result<(), FingerprintError> {
        match scalar {
            ScalarOp::Literal(literal) => {
                self.hasher.tag(0);
                hash_literal(&mut self.hasher, literal);
            }
            ScalarOp::Parameter { position, name } => {
                self.hasher.tag(1);
                self.hasher.u32(*position);
                self.hasher.optional_str(name.as_deref());
            }
            ScalarOp::Unary(operator) => {
                self.hasher.tag(2);
                self.hasher.tag(unary_tag(*operator));
            }
            ScalarOp::Binary(operator) => {
                self.hasher.tag(3);
                self.hasher.tag(binary_tag(*operator));
            }
            ScalarOp::Call {
                function,
                volatility,
                effects,
            } => {
                self.hasher.tag(4);
                self.hash_function(function);
                self.hasher.tag(volatility_tag(*volatility));
                self.hasher.tag(effects.bits());
            }
            ScalarOp::Cast { to } => {
                self.hasher.tag(5);
                self.hash_type(to)?;
            }
            ScalarOp::Case { arms } => {
                self.hasher.tag(6);
                self.hasher.u32(*arms);
            }
            ScalarOp::AggregateCall {
                function,
                distinct,
                volatility,
                effects,
            } => {
                self.hasher.tag(7);
                self.hash_function(function);
                self.hasher.boolean(*distinct);
                self.hasher.tag(volatility_tag(*volatility));
                self.hasher.tag(effects.bits());
            }
            ScalarOp::WindowCall {
                function,
                volatility,
                effects,
            } => {
                self.hasher.tag(8);
                self.hash_function(function);
                self.hasher.tag(volatility_tag(*volatility));
                self.hasher.tag(effects.bits());
            }
        }
        Ok(())
    }

    fn hash_extension(&mut self, extension: &ExtensionOp) {
        self.hasher.string(extension.dialect());
        self.hasher.string(extension.name());
        self.hasher.tag(extension.effects().bits());
        self.hasher.boolean(extension.is_terminator());
    }

    fn hash_function(&mut self, function: &FunctionRef) {
        self.hasher.optional_str(function.namespace());
        self.hasher.string(function.name());
    }

    fn hash_attributes(
        &mut self,
        attributes: &BTreeMap<String, Attribute>,
    ) -> Result<(), FingerprintError> {
        self.hasher.usize(attributes.len());
        for (key, value) in attributes {
            self.hasher.string(key);
            self.hash_attribute(value)?;
        }
        Ok(())
    }

    fn hash_attribute(&mut self, attribute: &Attribute) -> Result<(), FingerprintError> {
        match attribute {
            Attribute::Boolean(value) => {
                self.hasher.tag(0);
                self.hasher.boolean(*value);
            }
            Attribute::Integer(value) => {
                self.hasher.tag(1);
                self.hasher.i128(*value);
            }
            Attribute::Unsigned(value) => {
                self.hasher.tag(2);
                self.hasher.u128(*value);
            }
            Attribute::String(value) => {
                self.hasher.tag(3);
                self.hasher.string(value);
            }
            Attribute::Bytes(value) => {
                self.hasher.tag(4);
                self.hasher.bytes(value);
            }
            Attribute::Type(ty) => {
                self.hasher.tag(5);
                self.hash_type(ty)?;
            }
            Attribute::Array(values) => {
                self.hasher.tag(6);
                self.hasher.usize(values.len());
                for value in values {
                    self.hash_attribute(value)?;
                }
            }
            Attribute::Dictionary(values) => {
                self.hasher.tag(7);
                self.hash_attributes(values)?;
            }
        }
        Ok(())
    }

    fn hash_type(&mut self, ty: &Type) -> Result<(), FingerprintError> {
        match ty {
            Type::Scalar(scalar) => {
                self.hasher.tag(0);
                self.hasher.boolean(scalar.is_nullable());
                hash_sql_type(&mut self.hasher, scalar.kind());
            }
            Type::Tuple(elements) => {
                self.hasher.tag(1);
                self.hasher.usize(elements.len());
                for element in elements {
                    self.hash_type(element)?;
                }
            }
            Type::Relation(schema) => {
                self.hasher.tag(2);
                self.hash_schema(*schema)?;
            }
            Type::Unit => self.hasher.tag(3),
        }
        Ok(())
    }

    fn hash_schema(&mut self, schema_id: SchemaId) -> Result<(), FingerprintError> {
        // Relation types refer to interned schemas. Guard the recursive expansion
        // so corrupt or newly extended recursive type graphs fail deterministically.
        if !self.schema_stack.insert(schema_id) {
            return Err(FingerprintError::InconsistentReference(format!(
                "recursive schema reference at {schema_id}"
            )));
        }
        let schema = self.module.schema(schema_id).ok_or_else(|| {
            FingerprintError::InconsistentReference(format!("stale schema {schema_id}"))
        })?;
        self.hasher.usize(schema.fields().len());
        for field in schema.fields() {
            self.hasher.string(field.name());
            self.hash_type(field.ty())?;
        }
        self.schema_stack.remove(&schema_id);
        Ok(())
    }
}

/// Version-local deterministic encoder based on 128-bit FNV-1a.
///
/// Tags, field order, integer endianness, and the top-level domain separator form
/// the persisted fingerprint format. Any incompatible encoding change must also
/// change the `afterburner-ir-v2` domain string.
struct StableHasher {
    state: u128,
}

impl StableHasher {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

    const fn new() -> Self {
        Self {
            state: Self::OFFSET,
        }
    }

    const fn finish(self) -> u128 {
        self.state
    }

    fn tag(&mut self, tag: u8) {
        self.byte(tag);
    }

    fn byte(&mut self, byte: u8) {
        self.state ^= u128::from(byte);
        self.state = self.state.wrapping_mul(Self::PRIME);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.usize(bytes.len());
        for byte in bytes {
            self.byte(*byte);
        }
    }

    fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    fn optional_str(&mut self, value: Option<&str>) {
        self.boolean(value.is_some());
        if let Some(value) = value {
            self.string(value);
        }
    }

    fn boolean(&mut self, value: bool) {
        self.byte(u8::from(value));
    }

    fn usize(&mut self, value: usize) {
        self.u64(value as u64);
    }

    fn u32(&mut self, value: u32) {
        self.raw_bytes(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.raw_bytes(&value.to_le_bytes());
    }

    fn i128(&mut self, value: i128) {
        self.raw_bytes(&value.to_le_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.raw_bytes(&value.to_le_bytes());
    }

    fn raw_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.byte(*byte);
        }
    }
}

fn hash_literal(hasher: &mut StableHasher, literal: &Literal) {
    match literal {
        Literal::Null => hasher.tag(0),
        Literal::Boolean(value) => {
            hasher.tag(1);
            hasher.boolean(*value);
        }
        Literal::Integer(value) => {
            hasher.tag(2);
            hasher.i128(*value);
        }
        Literal::Unsigned(value) => {
            hasher.tag(3);
            hasher.u128(*value);
        }
        Literal::Float(value) => {
            hasher.tag(4);
            hasher.u64(value.bits());
        }
        Literal::Decimal { coefficient, scale } => {
            hasher.tag(5);
            hasher.i128(*coefficient);
            hasher.raw_bytes(&scale.to_le_bytes());
        }
        Literal::String(value) => {
            hasher.tag(6);
            hasher.string(value);
        }
        Literal::Bytes(value) => {
            hasher.tag(7);
            hasher.bytes(value);
        }
        Literal::Date(value) => {
            hasher.tag(8);
            hasher.raw_bytes(&value.to_le_bytes());
        }
        Literal::Time(value) => {
            hasher.tag(9);
            hasher.raw_bytes(&value.to_le_bytes());
        }
        Literal::Timestamp(value) => {
            hasher.tag(10);
            hasher.raw_bytes(&value.to_le_bytes());
        }
        Literal::Interval {
            months,
            days,
            nanos,
        } => {
            hasher.tag(11);
            hasher.raw_bytes(&months.to_le_bytes());
            hasher.raw_bytes(&days.to_le_bytes());
            hasher.raw_bytes(&nanos.to_le_bytes());
        }
        Literal::Uuid(value) => {
            hasher.tag(12);
            hasher.raw_bytes(value);
        }
        Literal::Json(value) => {
            hasher.tag(13);
            hasher.string(value);
        }
    }
}

fn hash_sql_type(hasher: &mut StableHasher, ty: &SqlType) {
    match ty {
        SqlType::Boolean => hasher.tag(0),
        SqlType::Integer { bits, signed } => {
            hasher.tag(1);
            hasher.raw_bytes(&bits.to_le_bytes());
            hasher.boolean(*signed);
        }
        SqlType::Float { bits } => {
            hasher.tag(2);
            hasher.raw_bytes(&bits.to_le_bytes());
        }
        SqlType::Decimal { precision, scale } => {
            hasher.tag(3);
            hasher.raw_bytes(&precision.to_le_bytes());
            hasher.raw_bytes(&scale.to_le_bytes());
        }
        SqlType::Utf8 => hasher.tag(4),
        SqlType::Binary => hasher.tag(5),
        SqlType::Date => hasher.tag(6),
        SqlType::Time { precision } => {
            hasher.tag(7);
            hasher.tag(*precision);
        }
        SqlType::Timestamp {
            precision,
            timezone,
        } => {
            hasher.tag(8);
            hasher.tag(*precision);
            match timezone {
                TimeZone::Naive => hasher.tag(0),
                TimeZone::Utc => hasher.tag(1),
                TimeZone::Named(name) => {
                    hasher.tag(2);
                    hasher.string(name);
                }
            }
        }
        SqlType::Interval => hasher.tag(9),
        SqlType::Uuid => hasher.tag(10),
        SqlType::Json => hasher.tag(11),
        SqlType::Custom(name) => {
            hasher.tag(12);
            hasher.string(name);
        }
    }
}

fn hash_sort_key(hasher: &mut StableHasher, key: SortKey) {
    hasher.tag(match key.direction() {
        SortDirection::Ascending => 0,
        SortDirection::Descending => 1,
    });
    hasher.tag(match key.null_order() {
        NullOrder::First => 0,
        NullOrder::Last => 1,
        NullOrder::DialectDefault => 2,
    });
}

fn join_tag(value: JoinKind) -> u8 {
    match value {
        JoinKind::Inner => 0,
        JoinKind::Left => 1,
        JoinKind::Right => 2,
        JoinKind::Full => 3,
        JoinKind::Semi => 4,
        JoinKind::Anti => 5,
        JoinKind::Cross => 6,
    }
}

fn set_tag(value: SetOperator) -> u8 {
    match value {
        SetOperator::Union => 0,
        SetOperator::Intersect => 1,
        SetOperator::Except => 2,
    }
}

fn unary_tag(value: UnaryOperator) -> u8 {
    match value {
        UnaryOperator::Not => 0,
        UnaryOperator::Negate => 1,
        UnaryOperator::IsNull => 2,
        UnaryOperator::IsNotNull => 3,
    }
}

fn binary_tag(value: BinaryOperator) -> u8 {
    match value {
        BinaryOperator::Add => 0,
        BinaryOperator::Subtract => 1,
        BinaryOperator::Multiply => 2,
        BinaryOperator::Divide => 3,
        BinaryOperator::Modulo => 4,
        BinaryOperator::Equal => 5,
        BinaryOperator::NotEqual => 6,
        BinaryOperator::LessThan => 7,
        BinaryOperator::LessThanOrEqual => 8,
        BinaryOperator::GreaterThan => 9,
        BinaryOperator::GreaterThanOrEqual => 10,
        BinaryOperator::And => 11,
        BinaryOperator::Or => 12,
        BinaryOperator::Concat => 13,
        BinaryOperator::Like => 14,
        BinaryOperator::CaseInsensitiveLike => 15,
        BinaryOperator::IsDistinctFrom => 16,
    }
}

fn volatility_tag(value: Volatility) -> u8 {
    match value {
        Volatility::Immutable => 0,
        Volatility::Stable => 1,
        Volatility::Volatile => 2,
    }
}

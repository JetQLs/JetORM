use std::{error::Error, fmt};

use afterburner::IntoAfterBurnerIr;
use afterburner::ir::{
    BinaryOperator, BlockId, EditError, Field, IrEditor, LogicalOp, Module, OperationSpec,
    ScalarOp, ScalarType, Schema, SortKey, SqlType, TableRef, TerminatorOp, TimeZone, Type,
    UnaryOperator, ValueId,
};
use jetorm_entity::{ColumnMeta, ColumnType, Entity, TableMeta, Value};

use crate::expr::Node;
use crate::select::Select;

/// Fractional-second digits used for every temporal column type.
///
/// Microseconds are lossless for `chrono` values in the supported range and
/// match PostgreSQL's native storage precision.
const TEMPORAL_PRECISION: u8 = 6;

/// Failure produced while lowering a typed query into AfterBurner IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoweringError {
    /// An IR edit was rejected; this indicates a bug in the lowering itself.
    Internal(EditError),
    /// Two scalar operands had incompatible SQL kinds.
    OperandKindMismatch {
        /// SQL kind of the left operand.
        left: SqlType,
        /// SQL kind of the right operand.
        right: SqlType,
    },
    /// A captured value was never assigned a bind position; this indicates a
    /// bug in query normalization.
    UnboundValue,
}

impl fmt::Display for LoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Internal(error) => write!(formatter, "IR construction failed: {error}"),
            Self::OperandKindMismatch { left, right } => write!(
                formatter,
                "operand SQL kinds {left:?} and {right:?} are incompatible"
            ),
            Self::UnboundValue => {
                formatter.write_str("expression value was not normalized into a bind")
            }
        }
    }
}

impl Error for LoweringError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Internal(error) => Some(error),
            Self::OperandKindMismatch { .. } | Self::UnboundValue => None,
        }
    }
}

impl From<EditError> for LoweringError {
    fn from(error: EditError) -> Self {
        Self::Internal(error)
    }
}

impl<E> IntoAfterBurnerIr for Select<E>
where
    E: Entity,
{
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower(&self)
    }
}

/// Lowers one typed select into a complete, unverified IR module.
///
/// The root block receives the logical pipeline in SQL evaluation order:
/// scan, filter, distinct, sort, limit, then the query terminator. Captured
/// values lower to IR parameters whose positions equal the query's bind-table
/// positions.
fn lower<E>(select: &Select<E>) -> Result<Module, LoweringError>
where
    E: Entity,
{
    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();

        let fields: Vec<Field> = E::COLUMNS
            .iter()
            .map(|column| Field::new(column.name(), Type::Scalar(column_scalar_type(column))))
            .collect();
        let field_types: Vec<Type> = fields.iter().map(|field| field.ty().clone()).collect();
        let schema = editor.intern_schema(Schema::new(fields));
        let relation_type = Type::relation(schema);

        let scan = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Scan {
                table: table_ref(&E::TABLE),
                columns: E::COLUMNS
                    .iter()
                    .map(|column| column.name().to_owned())
                    .collect(),
            })
            .with_result(relation_type.clone()),
        )?;
        let mut relation = editor.result(scan, 0)?;

        if let Some(predicate) = &select.filter {
            let filter = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Filter)
                    .with_operands(vec![relation])
                    .with_result(relation_type.clone()),
            )?;
            let region = editor.add_region(filter)?;
            let block = editor.append_block(region, field_types.clone())?;
            let (predicate_value, _) = lower_node(
                &mut editor,
                block,
                predicate,
                &select.binds,
                E::COLUMNS,
                None,
            )?;
            editor.append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate_value]),
            )?;
            relation = editor.result(filter, 0)?;
        }

        if select.distinct {
            let distinct = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Distinct)
                    .with_operands(vec![relation])
                    .with_result(relation_type.clone()),
            )?;
            relation = editor.result(distinct, 0)?;
        }

        if !select.order.is_empty() {
            let keys: Vec<SortKey> = select
                .order
                .iter()
                .map(|key| SortKey::new(key.direction, key.null_order))
                .collect();
            let sort = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Sort { keys })
                    .with_operands(vec![relation])
                    .with_result(relation_type.clone()),
            )?;
            let region = editor.add_region(sort)?;
            let block = editor.append_block(region, field_types.clone())?;
            let mut yielded = Vec::with_capacity(select.order.len());
            for key in &select.order {
                yielded.push(editor.block_argument(block, key.column)?);
            }
            editor.append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(yielded),
            )?;
            relation = editor.result(sort, 0)?;
        }

        if select.offset.is_some() || select.fetch.is_some() {
            let limit = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Limit {
                    offset: select.offset,
                    fetch: select.fetch,
                })
                .with_operands(vec![relation])
                .with_result(relation_type.clone()),
            )?;
            relation = editor.result(limit, 0)?;
        }

        editor.append_operation(
            root,
            OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
        )?;
    }
    Ok(module)
}

/// Lowers one expression node inside a row-lambda block.
///
/// `expected` propagates a partner operand's scalar type onto binds, so a
/// value compared against a column adopts the column's exact type, including
/// nullability, without a widening cast.
fn lower_node(
    editor: &mut IrEditor<'_>,
    block: BlockId,
    node: &Node,
    binds: &[Value],
    columns: &'static [ColumnMeta],
    expected: Option<&ScalarType>,
) -> Result<(ValueId, ScalarType), LoweringError> {
    match node {
        Node::Column(index) => {
            let value = editor.block_argument(block, *index)?;
            Ok((value, column_scalar_type(&columns[*index])))
        }
        Node::Bind(position) => {
            let ty = expected.cloned().unwrap_or_else(|| {
                let bind = &binds[*position];
                ScalarType::new(sql_type(bind.column_type()), bind.is_null())
            });
            let parameter = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::Parameter {
                    position: *position as u32,
                    name: None,
                })
                .with_result(Type::Scalar(ty.clone())),
            )?;
            Ok((editor.result(parameter, 0)?, ty))
        }
        Node::Value(_) => Err(LoweringError::UnboundValue),
        Node::Unary { op, operand } => {
            let (operand_value, operand_ty) =
                lower_node(editor, block, operand, binds, columns, None)?;
            let result_ty = match op {
                UnaryOperator::IsNull | UnaryOperator::IsNotNull => {
                    ScalarType::new(SqlType::Boolean, false)
                }
                UnaryOperator::Not | UnaryOperator::Negate => operand_ty,
            };
            let operation = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::Unary(*op))
                    .with_operands(vec![operand_value])
                    .with_result(Type::Scalar(result_ty.clone())),
            )?;
            Ok((editor.result(operation, 0)?, result_ty))
        }
        Node::Binary { op, left, right } => {
            // A bind adopts the partner column's exact type up front, so the
            // common column-versus-value comparison needs no widening cast.
            let left_expected = column_peek(right, columns);
            let right_expected = column_peek(left, columns);
            let (left_value, left_ty) =
                lower_node(editor, block, left, binds, columns, left_expected.as_ref())?;
            let (right_value, right_ty) = lower_node(
                editor,
                block,
                right,
                binds,
                columns,
                right_expected.as_ref(),
            )?;
            let (left_value, right_value, operand_ty) = unify_nullability(
                editor,
                block,
                (left_value, left_ty),
                (right_value, right_ty),
            )?;
            let result_ty = binary_result_type(*op, &operand_ty);
            let operation = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::Binary(*op))
                    .with_operands(vec![left_value, right_value])
                    .with_result(Type::Scalar(result_ty.clone())),
            )?;
            Ok((editor.result(operation, 0)?, result_ty))
        }
    }
}

/// Returns a column reference's scalar type without lowering it.
fn column_peek(node: &Node, columns: &'static [ColumnMeta]) -> Option<ScalarType> {
    match node {
        Node::Column(index) => Some(column_scalar_type(&columns[*index])),
        Node::Bind(_) | Node::Value(_) | Node::Unary { .. } | Node::Binary { .. } => None,
    }
}

/// Makes two operand types identical by widening one side to nullable.
///
/// AfterBurner requires exact operand type equality. When the two sides agree
/// on the SQL kind but disagree on nullability — for example a null test
/// combined with a nullable comparison under `AND` — the non-nullable side is
/// widened with an explicit cast, matching SQL's implicit semantics.
fn unify_nullability(
    editor: &mut IrEditor<'_>,
    block: BlockId,
    left: (ValueId, ScalarType),
    right: (ValueId, ScalarType),
) -> Result<(ValueId, ValueId, ScalarType), LoweringError> {
    let (left_value, left_ty) = left;
    let (right_value, right_ty) = right;
    if left_ty == right_ty {
        return Ok((left_value, right_value, left_ty));
    }
    if left_ty.kind() != right_ty.kind() {
        return Err(LoweringError::OperandKindMismatch {
            left: left_ty.kind().clone(),
            right: right_ty.kind().clone(),
        });
    }
    if left_ty.is_nullable() {
        let (widened, ty) = widen_to_nullable(editor, block, right_value, &right_ty)?;
        Ok((left_value, widened, ty))
    } else {
        let (widened, ty) = widen_to_nullable(editor, block, left_value, &left_ty)?;
        Ok((widened, right_value, ty))
    }
}

fn widen_to_nullable(
    editor: &mut IrEditor<'_>,
    block: BlockId,
    value: ValueId,
    ty: &ScalarType,
) -> Result<(ValueId, ScalarType), LoweringError> {
    let target = ty.with_nullability(true);
    let cast = editor.append_operation(
        block,
        OperationSpec::new(ScalarOp::Cast {
            to: Type::Scalar(target.clone()),
        })
        .with_operands(vec![value])
        .with_result(Type::Scalar(target.clone())),
    )?;
    Ok((editor.result(cast, 0)?, target))
}

/// Returns a binary operation's result type from its unified operand type.
fn binary_result_type(op: BinaryOperator, operand: &ScalarType) -> ScalarType {
    match op {
        // Null-safe distinctness is total: it never returns SQL NULL.
        BinaryOperator::IsDistinctFrom => ScalarType::new(SqlType::Boolean, false),
        BinaryOperator::Equal
        | BinaryOperator::NotEqual
        | BinaryOperator::LessThan
        | BinaryOperator::LessThanOrEqual
        | BinaryOperator::GreaterThan
        | BinaryOperator::GreaterThanOrEqual
        | BinaryOperator::And
        | BinaryOperator::Or
        | BinaryOperator::Like
        | BinaryOperator::CaseInsensitiveLike => {
            ScalarType::new(SqlType::Boolean, operand.is_nullable())
        }
        BinaryOperator::Add
        | BinaryOperator::Subtract
        | BinaryOperator::Multiply
        | BinaryOperator::Divide
        | BinaryOperator::Modulo
        | BinaryOperator::Concat => operand.clone(),
    }
}

fn table_ref(table: &TableMeta) -> TableRef {
    TableRef::qualified(None::<&str>, table.schema(), table.name())
}

fn column_scalar_type(column: &ColumnMeta) -> ScalarType {
    ScalarType::new(sql_type(column.column_type()), column.is_nullable())
}

/// Maps the entity column type onto the AfterBurner SQL kind.
fn sql_type(column_type: ColumnType) -> SqlType {
    match column_type {
        ColumnType::Boolean => SqlType::Boolean,
        ColumnType::Int16 => SqlType::Integer {
            bits: 16,
            signed: true,
        },
        ColumnType::Int32 => SqlType::Integer {
            bits: 32,
            signed: true,
        },
        ColumnType::Int64 => SqlType::Integer {
            bits: 64,
            signed: true,
        },
        ColumnType::Float32 => SqlType::Float { bits: 32 },
        ColumnType::Float64 => SqlType::Float { bits: 64 },
        ColumnType::Text => SqlType::Utf8,
        ColumnType::Bytes => SqlType::Binary,
        ColumnType::Date => SqlType::Date,
        ColumnType::Time => SqlType::Time {
            precision: TEMPORAL_PRECISION,
        },
        ColumnType::Timestamp => SqlType::Timestamp {
            precision: TEMPORAL_PRECISION,
            timezone: TimeZone::Naive,
        },
        ColumnType::TimestampUtc => SqlType::Timestamp {
            precision: TEMPORAL_PRECISION,
            timezone: TimeZone::Utc,
        },
        ColumnType::Uuid => SqlType::Uuid,
        ColumnType::Json => SqlType::Json,
    }
}

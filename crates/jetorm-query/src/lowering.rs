use std::{error::Error, fmt};

use afterburner::IntoAfterBurnerIr;
use afterburner::ir::{
    BinaryOperator, BlockId, EditError, EffectSet, Field, FunctionRef, IrEditor, JoinKind,
    LogicalOp, Module, OperationSpec, ScalarOp, ScalarType, Schema, SortKey, SqlType, TableRef,
    TerminatorOp, TimeZone, Type, UnaryOperator, ValueId, Volatility,
};
use jetorm_entity::{Column, ColumnMeta, ColumnType, Entity, Relation, TableMeta};

use crate::aggregate::{AggregateSpec, GroupedSelect};
use crate::expr::{Predicate, SortKeySpec};
use crate::join::JoinSelect;
use crate::projection::ColumnList;
use crate::select::{CountQuery, Select};

/// Fractional-second digits used for every temporal column type.
///
/// Microseconds are lossless for `chrono` values in the supported range and
/// match PostgreSQL's native storage precision.
const TEMPORAL_PRECISION: u8 = 6;

/// Failure produced while lowering a typed query into AfterBurner IR.
///
/// The set of failure modes grows as the frontend gains expression and
/// relational features, so callers must handle unknown variants.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
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
    /// The query combined a projection with `DISTINCT`, whose interaction
    /// (deduplicate the full row, or the projected row?) is not expressible
    /// yet. Remove one of the two.
    DistinctOverProjection,
    /// The query combined a relation join with `DISTINCT`, whose meaning
    /// (deduplicate the source row, or the joined pair?) is not expressible
    /// yet. Remove one of the two.
    DistinctOverJoin,
    /// The query combined grouping with a row limit or offset, whose
    /// meaning (limit the source rows, or the groups?) is not expressible
    /// yet. Remove one of the two.
    LimitOverGroup,
    /// The same column appears twice among a grouping's keys.
    DuplicateGroupKey {
        /// Position of the repeated column.
        column: usize,
    },
}

impl fmt::Display for LoweringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Internal(error) => write!(formatter, "IR construction failed: {error}"),
            Self::OperandKindMismatch { left, right } => write!(
                formatter,
                "operand SQL kinds {left:?} and {right:?} are incompatible"
            ),
            Self::DistinctOverProjection => formatter
                .write_str("distinct combined with a column projection is not supported yet"),
            Self::DistinctOverJoin => {
                formatter.write_str("distinct combined with a relation join is not supported yet")
            }
            Self::LimitOverGroup => formatter
                .write_str("a row limit or offset combined with grouping is not supported yet"),
            Self::DuplicateGroupKey { column } => {
                write!(
                    formatter,
                    "group key column {column} appears more than once"
                )
            }
        }
    }
}

impl Error for LoweringError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Internal(error) => Some(error),
            Self::OperandKindMismatch { .. }
            | Self::DistinctOverProjection
            | Self::DistinctOverJoin
            | Self::LimitOverGroup
            | Self::DuplicateGroupKey { .. } => None,
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

impl<E> IntoAfterBurnerIr for CountQuery<E>
where
    E: Entity,
{
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_count(&self)
    }
}

impl<R> IntoAfterBurnerIr for JoinSelect<R>
where
    R: Relation,
{
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_join(&self)
    }
}

impl<E, K, A> IntoAfterBurnerIr for GroupedSelect<E, K, A>
where
    E: Entity,
    K: ColumnList<E>,
    A: crate::aggregate::AggregateList<E>,
{
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_grouped::<E>(
            &self.select,
            &K::indexes(),
            &self.aggregates,
            self.order_by_keys,
        )
    }
}

/// Everything both select and count lowering need about one query, borrowed
/// from whichever builder is being lowered.
struct RowPipeline<'query> {
    filter: Option<&'query Predicate>,
    distinct: bool,
    order: &'query [SortKeySpec],
    has_offset: bool,
    has_fetch: bool,
    /// Number of predicate binds; row-count parameters position after them.
    predicate_binds: usize,
}

/// State the pipeline leaves behind for the query-specific tail.
struct LoweredRows {
    relation: ValueId,
    field_types: Vec<Type>,
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
        let rows = lower_pipeline::<E>(
            &mut editor,
            root,
            &RowPipeline {
                filter: select.filter.as_deref(),
                distinct: select.distinct,
                order: &select.order,
                has_offset: select.offset.is_some(),
                has_fetch: select.fetch.is_some(),
                predicate_binds: select.binds.len(),
            },
        )?;
        let mut relation = rows.relation;

        if let Some(projection) = &select.projection {
            if select.distinct {
                return Err(LoweringError::DistinctOverProjection);
            }
            // Projection applies last, so filters, sort keys, and row limits
            // keep addressing the full row; SQL can always express that
            // (the SELECT list narrows the row leaving FROM untouched).
            let output = Schema::new(
                projection
                    .iter()
                    .map(|index| {
                        let column = &E::COLUMNS[*index];
                        Field::new(column.name(), Type::Scalar(column_scalar_type(column)))
                    })
                    .collect::<Vec<_>>(),
            );
            let output_schema = editor.intern_schema(output);
            let project = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Project)
                    .with_operands(vec![relation])
                    .with_result(Type::relation(output_schema)),
            )?;
            let region = editor.add_region(project)?;
            let block = editor.append_block(region, rows.field_types.clone())?;
            let mut yielded = Vec::with_capacity(projection.len());
            for index in projection {
                yielded.push(editor.block_argument(block, *index)?);
            }
            editor.append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(yielded),
            )?;
            relation = editor.result(project, 0)?;
        }

        editor.append_operation(
            root,
            OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
        )?;
    }
    Ok(module)
}

/// Lowers one typed count into a complete, unverified IR module.
///
/// The row pipeline is the select pipeline minus ordering — no ordering can
/// change how many rows there are — collapsed by a grand-total aggregate
/// (`Aggregate` with the empty grouping set) yielding a single non-null
/// `count(*)` value.
fn lower_count<E>(count: &CountQuery<E>) -> Result<Module, LoweringError>
where
    E: Entity,
{
    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let rows = lower_pipeline::<E>(
            &mut editor,
            root,
            &RowPipeline {
                filter: count.filter.as_deref(),
                distinct: count.distinct,
                order: &[],
                has_offset: count.offset.is_some(),
                has_fetch: count.fetch.is_some(),
                predicate_binds: count.binds.len(),
            },
        )?;

        let count_type = ScalarType::new(
            SqlType::Integer {
                bits: 64,
                signed: true,
            },
            false,
        );
        let output_schema = editor.intern_schema(Schema::new(vec![Field::new(
            "count",
            Type::Scalar(count_type.clone()),
        )]));
        let aggregate = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Aggregate { group_keys: 0 })
                .with_operands(vec![rows.relation])
                .with_result(Type::relation(output_schema)),
        )?;
        let region = editor.add_region(aggregate)?;
        let block = editor.append_block(region, rows.field_types)?;
        let call = editor.append_operation(
            block,
            OperationSpec::new(ScalarOp::AggregateCall {
                function: FunctionRef::new("count"),
                distinct: false,
                volatility: Volatility::Immutable,
                effects: EffectSet::PURE,
            })
            .with_result(Type::Scalar(count_type)),
        )?;
        let total = editor.result(call, 0)?;
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(vec![total]),
        )?;
        let relation = editor.result(aggregate, 0)?;
        editor.append_operation(
            root,
            OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
        )?;
    }
    Ok(module)
}

/// Lowers one relation join into a complete, unverified IR module.
///
/// The module scans both entities, `LEFT JOIN`s them on the relation's
/// column pair, and then applies the source query's row stages to the
/// joined relation. Source fields keep their leading positions, so filter
/// predicates and sort keys keep addressing them unchanged; target fields
/// follow with nullability widened, as SQL widens the null-extended side.
fn lower_join<R>(join: &JoinSelect<R>) -> Result<Module, LoweringError>
where
    R: Relation,
{
    if join.select.distinct {
        return Err(LoweringError::DistinctOverJoin);
    }

    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let (source, _, source_types) = scan_entity::<R::Source>(&mut editor, root)?;
        let (target, _, target_types) = scan_entity::<R::Target>(&mut editor, root)?;

        // Output schema: source row as-is, then the target row widened to
        // nullable — an unmatched left row null-extends it. Target field
        // names take the relation as a prefix so the two sides can never
        // collide (schemas reject duplicate names).
        let mut fields: Vec<Field> = R::Source::COLUMNS
            .iter()
            .map(|column| Field::new(column.name(), Type::Scalar(column_scalar_type(column))))
            .collect();
        for column in R::Target::COLUMNS {
            fields.push(Field::new(
                format!("{}__{}", R::NAME, column.name()),
                Type::Scalar(column_scalar_type(column).with_nullability(true)),
            ));
        }
        let joined_types: Vec<Type> = fields.iter().map(|field| field.ty().clone()).collect();
        let joined_schema = editor.intern_schema(Schema::new(fields));
        let joined_type = Type::relation(joined_schema);

        let join_op = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Join {
                kind: JoinKind::Left,
                has_condition: true,
            })
            .with_operands(vec![source, target])
            .with_result(joined_type.clone()),
        )?;
        // The condition evaluates before null extension, so its block sees
        // both rows with their original types.
        let region = editor.add_region(join_op)?;
        let mut condition_types = source_types.clone();
        condition_types.extend(target_types);
        let block = editor.append_block(region, condition_types)?;
        let left = (
            editor.block_argument(block, <R::SourceColumn as Column>::INDEX)?,
            column_scalar_type(&R::Source::COLUMNS[<R::SourceColumn as Column>::INDEX]),
        );
        let right = (
            editor.block_argument(
                block,
                R::Source::COLUMNS.len() + <R::TargetColumn as Column>::INDEX,
            )?,
            column_scalar_type(&R::Target::COLUMNS[<R::TargetColumn as Column>::INDEX]),
        );
        let (left_value, right_value, unified) =
            unify_nullability(&mut editor, block, left, right)?;
        let equality = editor.append_operation(
            block,
            OperationSpec::new(ScalarOp::Binary(BinaryOperator::Equal))
                .with_operands(vec![left_value, right_value])
                .with_result(Type::scalar(SqlType::Boolean, unified.is_nullable())),
        )?;
        let predicate = editor.result(equality, 0)?;
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate]),
        )?;
        let joined = editor.result(join_op, 0)?;

        // Source and related predicates lower into one WHERE over the
        // joined row: source columns from position zero, target columns at
        // the source's width and widened to nullable, as the null-extended
        // side really is.
        let mut filtered = joined;
        if join.select.filter.is_some() || join.related_filter.is_some() {
            let filter = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Filter)
                    .with_operands(vec![filtered])
                    .with_result(joined_type.clone()),
            )?;
            let region = editor.add_region(filter)?;
            let block = editor.append_block(region, joined_types.clone())?;
            let mut condition: Option<(ValueId, ScalarType)> = None;
            if let Some(predicate) = join.select.filter.as_deref() {
                condition = Some(lower_node(
                    &mut editor,
                    block,
                    predicate,
                    &PredicateColumns {
                        columns: R::Source::COLUMNS,
                        offset: 0,
                        widen_nullable: false,
                    },
                )?);
            }
            if let Some(predicate) = join.related_filter.as_deref() {
                let related = lower_node(
                    &mut editor,
                    block,
                    predicate,
                    &PredicateColumns {
                        columns: R::Target::COLUMNS,
                        offset: R::Source::COLUMNS.len(),
                        widen_nullable: true,
                    },
                )?;
                condition = Some(match condition {
                    None => related,
                    Some(source) => {
                        let (left_value, right_value, unified) =
                            unify_nullability(&mut editor, block, source, related)?;
                        let both = editor.append_operation(
                            block,
                            OperationSpec::new(ScalarOp::Binary(BinaryOperator::And))
                                .with_operands(vec![left_value, right_value])
                                .with_result(Type::Scalar(unified.clone())),
                        )?;
                        (editor.result(both, 0)?, unified)
                    }
                });
            }
            let (predicate_value, _) =
                condition.expect("at least one predicate exists inside this branch");
            editor.append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate_value]),
            )?;
            filtered = editor.result(filter, 0)?;
        }

        let rows = lower_row_stages::<R::Source>(
            &mut editor,
            root,
            filtered,
            joined_type,
            joined_types,
            &RowPipeline {
                filter: None,
                distinct: false,
                order: &join.select.order,
                has_offset: join.select.offset.is_some(),
                has_fetch: join.select.fetch.is_some(),
                predicate_binds: join.select.binds.len(),
            },
        )?;
        editor.append_operation(
            root,
            OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![rows.relation]),
        )?;
    }
    Ok(module)
}

/// Lowers one grouped aggregate into a complete, unverified IR module.
///
/// The row pipeline (scan and filter — SQL's `WHERE`) feeds an `Aggregate`
/// whose region yields the group keys first, straight from the row's block
/// arguments as the grouping contract requires, followed by one aggregate
/// call per spec. Key ordering, when requested, sorts the aggregate output
/// by its leading key fields.
fn lower_grouped<E>(
    select: &Select<E>,
    keys: &[usize],
    aggregates: &[AggregateSpec],
    order_by_keys: bool,
) -> Result<Module, LoweringError>
where
    E: Entity,
{
    // A limit's meaning under grouping (source rows or groups?) is
    // ambiguous, so it is rejected rather than guessed — the same policy
    // as distinct over projections. Repeated keys would produce duplicate
    // schema fields, which the verifier rejects with an internal-looking
    // error; failing here names the user's actual mistake.
    if select.offset.is_some() || select.fetch.is_some() {
        return Err(LoweringError::LimitOverGroup);
    }
    for (position, key) in keys.iter().enumerate() {
        if keys[..position].contains(key) {
            return Err(LoweringError::DuplicateGroupKey { column: *key });
        }
    }

    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let rows = lower_pipeline::<E>(
            &mut editor,
            root,
            &RowPipeline {
                filter: select.filter.as_deref(),
                distinct: select.distinct,
                order: &[],
                has_offset: false,
                has_fetch: false,
                predicate_binds: select.binds.len(),
            },
        )?;

        // Output schema: the key columns keep their names and types, each
        // aggregate takes a positional name and its own promoted type.
        let mut fields = Vec::with_capacity(keys.len() + aggregates.len());
        for index in keys {
            let column = &E::COLUMNS[*index];
            fields.push(Field::new(
                column.name(),
                Type::Scalar(column_scalar_type(column)),
            ));
        }
        let mut aggregate_types = Vec::with_capacity(aggregates.len());
        for (position, spec) in aggregates.iter().enumerate() {
            let ty = ScalarType::new(sql_type(spec.column_type), spec.nullable);
            aggregate_types.push(ty.clone());
            // The prefix keeps aggregate fields out of the namespace any
            // entity column could plausibly occupy.
            fields.push(Field::new(
                format!("__agg_{position}_{}", spec.function.sql_name()),
                Type::Scalar(ty),
            ));
        }
        let output_types: Vec<Type> = fields.iter().map(|field| field.ty().clone()).collect();
        let output_schema = editor.intern_schema(Schema::new(fields));
        let output_relation = Type::relation(output_schema);

        let group_count = u32::try_from(keys.len()).expect("column counts fit in 32 bits");
        let aggregate = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Aggregate {
                group_keys: group_count,
            })
            .with_operands(vec![rows.relation])
            .with_result(output_relation.clone()),
        )?;
        let region = editor.add_region(aggregate)?;
        let block = editor.append_block(region, rows.field_types)?;
        // The region yields the grouping expressions first — the exact
        // block arguments, as grouping identity requires — and then the
        // full output row, whose leading key fields repeat those same
        // arguments.
        let mut yielded = Vec::with_capacity(2 * keys.len() + aggregates.len());
        for index in keys {
            yielded.push(editor.block_argument(block, *index)?);
        }
        for index in keys {
            yielded.push(editor.block_argument(block, *index)?);
        }
        for (spec, ty) in aggregates.iter().zip(&aggregate_types) {
            let mut operands = Vec::new();
            if let Some(column) = spec.column {
                operands.push(editor.block_argument(block, column)?);
            }
            let call = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::AggregateCall {
                    function: FunctionRef::new(spec.function.sql_name()),
                    distinct: false,
                    volatility: Volatility::Immutable,
                    effects: EffectSet::PURE,
                })
                .with_operands(operands)
                .with_result(Type::Scalar(ty.clone())),
            )?;
            yielded.push(editor.result(call, 0)?);
        }
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(yielded),
        )?;
        let mut relation = editor.result(aggregate, 0)?;

        if order_by_keys {
            let sort_keys = vec![
                SortKey::new(
                    afterburner::ir::SortDirection::Ascending,
                    afterburner::ir::NullOrder::Last,
                );
                keys.len()
            ];
            let sort = editor.append_operation(
                root,
                OperationSpec::new(LogicalOp::Sort { keys: sort_keys })
                    .with_operands(vec![relation])
                    .with_result(output_relation.clone()),
            )?;
            let region = editor.add_region(sort)?;
            let block = editor.append_block(region, output_types.clone())?;
            let mut yielded = Vec::with_capacity(keys.len());
            for position in 0..keys.len() {
                yielded.push(editor.block_argument(block, position)?);
            }
            editor.append_operation(
                block,
                OperationSpec::new(TerminatorOp::Yield).with_operands(yielded),
            )?;
            relation = editor.result(sort, 0)?;
        }

        editor.append_operation(
            root,
            OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
        )?;
    }
    Ok(module)
}

/// Lowers the row-producing pipeline shared by selects and counts: scan,
/// filter, distinct, sort, limit, in SQL evaluation order. Captured values
/// lower to IR parameters whose positions equal the query's bind-table
/// positions.
fn lower_pipeline<E>(
    editor: &mut IrEditor<'_>,
    root: BlockId,
    pipeline: &RowPipeline<'_>,
) -> Result<LoweredRows, LoweringError>
where
    E: Entity,
{
    let (relation, relation_type, field_types) = scan_entity::<E>(editor, root)?;
    lower_row_stages::<E>(editor, root, relation, relation_type, field_types, pipeline)
}

/// Appends one entity scan and returns its relation, relation type, and the
/// row's field types for later region blocks.
fn scan_entity<E>(
    editor: &mut IrEditor<'_>,
    root: BlockId,
) -> Result<(ValueId, Type, Vec<Type>), LoweringError>
where
    E: Entity,
{
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
    Ok((editor.result(scan, 0)?, relation_type, field_types))
}

/// Applies the row stages — filter, distinct, sort, limit — to one relation.
///
/// The relation's leading fields must be `E`'s columns in declaration order:
/// filter predicates and sort keys address them positionally, which is what
/// lets the same stages run over a bare scan and over a join whose left side
/// is the entity.
fn lower_row_stages<E>(
    editor: &mut IrEditor<'_>,
    root: BlockId,
    mut relation: ValueId,
    relation_type: Type,
    field_types: Vec<Type>,
    pipeline: &RowPipeline<'_>,
) -> Result<LoweredRows, LoweringError>
where
    E: Entity,
{
    if let Some(predicate) = pipeline.filter {
        let filter = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Filter)
                .with_operands(vec![relation])
                .with_result(relation_type.clone()),
        )?;
        let region = editor.add_region(filter)?;
        let block = editor.append_block(region, field_types.clone())?;
        let (predicate_value, _) = lower_node(
            editor,
            block,
            predicate,
            &PredicateColumns {
                columns: E::COLUMNS,
                offset: 0,
                widen_nullable: false,
            },
        )?;
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate_value]),
        )?;
        relation = editor.result(filter, 0)?;
    }

    if pipeline.distinct {
        let distinct = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Distinct)
                .with_operands(vec![relation])
                .with_result(relation_type.clone()),
        )?;
        relation = editor.result(distinct, 0)?;
    }

    if !pipeline.order.is_empty() {
        let keys: Vec<SortKey> = pipeline
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
        let mut yielded = Vec::with_capacity(pipeline.order.len());
        for key in pipeline.order {
            yielded.push(editor.block_argument(block, key.column)?);
        }
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(yielded),
        )?;
        relation = editor.result(sort, 0)?;
    }

    if pipeline.has_offset || pipeline.has_fetch {
        // Row counts lower as parameters positioned directly after the
        // predicate binds — the same order `Select::binds` emits values —
        // so every page of a paginated query shares one statement.
        let count_type = Type::scalar(
            SqlType::Integer {
                bits: 64,
                signed: true,
            },
            false,
        );
        let mut operands = vec![relation];
        let count_slots = usize::from(pipeline.has_offset) + usize::from(pipeline.has_fetch);
        for slot in 0..count_slots {
            let parameter = editor.append_operation(
                root,
                OperationSpec::new(ScalarOp::Parameter {
                    position: (pipeline.predicate_binds + slot) as u32,
                    name: None,
                })
                .with_result(count_type.clone()),
            )?;
            operands.push(editor.result(parameter, 0)?);
        }
        let limit = editor.append_operation(
            root,
            OperationSpec::new(LogicalOp::Limit {
                has_offset: pipeline.has_offset,
                has_fetch: pipeline.has_fetch,
            })
            .with_operands(operands)
            .with_result(relation_type.clone()),
        )?;
        relation = editor.result(limit, 0)?;
    }

    Ok(LoweredRows {
        relation,
        field_types,
    })
}

/// Lowers one predicate node inside a row-lambda block.
///
/// Where one predicate's columns live inside the block being lowered.
///
/// A plain select's predicate addresses the row from position zero; a
/// join's related predicate addresses the target entity's columns at an
/// offset, widened to nullable because the left join null-extends them.
struct PredicateColumns {
    columns: &'static [ColumnMeta],
    offset: usize,
    widen_nullable: bool,
}

/// The predicate carries the static typing of every operand, so lowering
/// never consults the bind table: the query's shape alone determines its IR.
fn lower_node(
    editor: &mut IrEditor<'_>,
    block: BlockId,
    node: &Predicate,
    columns: &PredicateColumns,
) -> Result<(ValueId, ScalarType), LoweringError> {
    match node {
        Predicate::Column(index) => {
            let value = editor.block_argument(block, columns.offset + *index)?;
            let mut ty = column_scalar_type(&columns.columns[*index]);
            if columns.widen_nullable {
                ty = ty.with_nullability(true);
            }
            Ok((value, ty))
        }
        Predicate::Bind { position, ty } => {
            let kind = if ty.list {
                SqlType::Array {
                    element: Box::new(sql_type(ty.column_type)),
                }
            } else {
                sql_type(ty.column_type)
            };
            let ty = ScalarType::new(kind, ty.nullable);
            let parameter = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::Parameter {
                    position: *position,
                    name: None,
                })
                .with_result(Type::Scalar(ty.clone())),
            )?;
            Ok((editor.result(parameter, 0)?, ty))
        }
        Predicate::Unary { op, operand } => {
            let (operand_value, operand_ty) = lower_node(editor, block, operand, columns)?;
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
        Predicate::Binary { op, left, right } => {
            let (left_value, left_ty) = lower_node(editor, block, left, columns)?;
            let (right_value, right_ty) = lower_node(editor, block, right, columns)?;
            // Membership pairs a scalar with an array of its kind, so the
            // operand-unification rule for symmetric operators cannot apply.
            let (left_value, right_value, operand_ty) = if *op == BinaryOperator::InArray {
                let result = ScalarType::new(SqlType::Boolean, left_ty.is_nullable());
                let _ = right_ty;
                (left_value, right_value, result)
            } else {
                let (left_value, right_value, unified) = unify_nullability(
                    editor,
                    block,
                    (left_value, left_ty),
                    (right_value, right_ty),
                )?;
                let result = binary_result_type(*op, &unified);
                (left_value, right_value, result)
            };
            let operation = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::Binary(*op))
                    .with_operands(vec![left_value, right_value])
                    .with_result(Type::Scalar(operand_ty.clone())),
            )?;
            Ok((editor.result(operation, 0)?, operand_ty))
        }
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
        BinaryOperator::InArray
        | BinaryOperator::Equal
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
        // Precision zero is the unconstrained-numeric sentinel: the dialect
        // renders it as bare `numeric`, so no cast ever rounds a bound
        // value. A declared precision would become `numeric(p, s)` casts.
        ColumnType::Decimal => SqlType::Decimal {
            precision: 0,
            scale: 0,
        },
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

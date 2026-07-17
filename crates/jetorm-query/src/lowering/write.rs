//! Lowering for typed data-mutation builders.

use afterburner::IntoAfterBurnerIr;
use afterburner::ir::{
    ConflictClause, ConflictTarget, Field, IrEditor, Literal, Module, MutationOp, OperationSpec,
    ScalarOp, Schema, TerminatorOp, Type, UpsertAssignment,
};
use jetorm_entity::Entity;

use super::{
    LoweringError, checked_bind_position, column_scalar_type, lower_entity_node, table_ref,
};
use crate::mutation::InsertConflict;
use crate::{Delete, Insert, Returning, Update};

impl<E: Entity> IntoAfterBurnerIr for Insert<E> {
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_insert(&self)
    }
}

impl<E: Entity> IntoAfterBurnerIr for Returning<Insert<E>> {
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_insert(&self.mutation)
    }
}

impl<E: Entity> IntoAfterBurnerIr for Update<E> {
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_update(&self)
    }
}

impl<E: Entity> IntoAfterBurnerIr for Returning<Update<E>> {
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_update(&self.mutation)
    }
}

impl<E: Entity> IntoAfterBurnerIr for Delete<E> {
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_delete(&self)
    }
}

impl<E: Entity> IntoAfterBurnerIr for Returning<Delete<E>> {
    type Error = LoweringError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        lower_delete(&self.mutation)
    }
}

fn lower_insert<E: Entity>(insert: &Insert<E>) -> Result<Module, LoweringError> {
    insert.validate()?;
    let included = E::COLUMNS
        .iter()
        .enumerate()
        .filter(|(_, column)| !column.is_auto_increment())
        .collect::<Vec<_>>();
    let conflict = match insert.conflict {
        InsertConflict::None => None,
        InsertConflict::DoNothing => Some(ConflictClause::do_nothing(primary_key_target::<E>())),
        InsertConflict::UpdateInserted => {
            let target = primary_key_target::<E>().ok_or(LoweringError::MissingPrimaryKey)?;
            let assignments = included
                .iter()
                .filter(|(_, column)| !column.is_primary_key())
                .map(|(_, column)| UpsertAssignment::new(column.name(), column.name()))
                .collect::<Vec<_>>();
            if assignments.is_empty() {
                return Err(LoweringError::EmptyUpsertUpdate);
            }
            Some(ConflictClause::do_update(target, assignments))
        }
    };

    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let schema = entity_schema::<E>(&mut editor);
        let returning = returning_columns::<E>(insert.returning);
        let result_type = mutation_result_type(schema, insert.returning);
        let rows =
            u32::try_from(insert.rows.len()).map_err(|_| LoweringError::CapacityExceeded {
                detail: "insert row count",
            })?;
        let operation = editor.append_operation(
            root,
            OperationSpec::new(MutationOp::Insert {
                table: table_ref(&E::TABLE),
                schema,
                columns: included
                    .iter()
                    .map(|(_, column)| column.name().to_owned())
                    .collect(),
                rows,
                conflict,
                returning,
            })
            .with_result(result_type),
        )?;
        let region = editor.add_region(operation)?;
        let block = editor.append_block(region, Vec::new())?;
        let mut values = Vec::with_capacity(insert.rows.len() * included.len());
        let mut position = 0_usize;
        for _row in &insert.rows {
            for (_, column) in &included {
                let parameter = editor.append_operation(
                    block,
                    OperationSpec::new(ScalarOp::Parameter {
                        position: checked_bind_position(position, 0)?,
                        name: None,
                    })
                    .with_result(Type::Scalar(column_scalar_type(column))),
                )?;
                values.push(editor.result(parameter, 0)?);
                position += 1;
            }
        }
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(values),
        )?;
        append_mutation_return(&mut editor, root, operation, insert.returning)?;
    }
    Ok(module)
}

fn lower_update<E: Entity>(update: &Update<E>) -> Result<Module, LoweringError> {
    update.validate()?;

    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let schema = entity_schema::<E>(&mut editor);
        let returning = returning_columns::<E>(update.returning);
        let result_type = mutation_result_type(schema, update.returning);
        let operation = editor.append_operation(
            root,
            OperationSpec::new(MutationOp::Update {
                table: table_ref(&E::TABLE),
                schema,
                assignments: update
                    .assignments
                    .iter()
                    .map(|(index, _)| E::COLUMNS[*index].name().to_owned())
                    .collect(),
                returning,
            })
            .with_result(result_type),
        )?;
        let region = editor.add_region(operation)?;
        let block = editor.append_block(
            region,
            E::COLUMNS
                .iter()
                .map(|column| Type::Scalar(column_scalar_type(column)))
                .collect::<Vec<_>>(),
        )?;
        let predicate = if let Some(predicate) = &update.filter {
            lower_entity_node(&mut editor, block, predicate, E::COLUMNS)?.0
        } else {
            true_literal(&mut editor, block)?
        };
        let mut yielded = vec![predicate];
        for (index, position) in &update.assignments {
            let parameter = editor.append_operation(
                block,
                OperationSpec::new(ScalarOp::Parameter {
                    position: checked_bind_position(*position, 0)?,
                    name: None,
                })
                .with_result(Type::Scalar(column_scalar_type(&E::COLUMNS[*index]))),
            )?;
            yielded.push(editor.result(parameter, 0)?);
        }
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(yielded),
        )?;
        append_mutation_return(&mut editor, root, operation, update.returning)?;
    }
    Ok(module)
}

fn lower_delete<E: Entity>(delete: &Delete<E>) -> Result<Module, LoweringError> {
    delete.validate()?;
    let mut module = Module::new();
    let root = module.root_block();
    {
        let mut editor = module.editor();
        let schema = entity_schema::<E>(&mut editor);
        let returning = returning_columns::<E>(delete.returning);
        let result_type = mutation_result_type(schema, delete.returning);
        let operation = editor.append_operation(
            root,
            OperationSpec::new(MutationOp::Delete {
                table: table_ref(&E::TABLE),
                schema,
                returning,
            })
            .with_result(result_type),
        )?;
        let region = editor.add_region(operation)?;
        let block = editor.append_block(
            region,
            E::COLUMNS
                .iter()
                .map(|column| Type::Scalar(column_scalar_type(column)))
                .collect::<Vec<_>>(),
        )?;
        let predicate = if let Some(predicate) = &delete.filter {
            lower_entity_node(&mut editor, block, predicate, E::COLUMNS)?.0
        } else {
            true_literal(&mut editor, block)?
        };
        editor.append_operation(
            block,
            OperationSpec::new(TerminatorOp::Yield).with_operands(vec![predicate]),
        )?;
        append_mutation_return(&mut editor, root, operation, delete.returning)?;
    }
    Ok(module)
}

fn entity_schema<E: Entity>(editor: &mut IrEditor<'_>) -> afterburner::ir::SchemaId {
    editor.intern_schema(Schema::new(
        E::COLUMNS
            .iter()
            .map(|column| Field::new(column.name(), Type::Scalar(column_scalar_type(column))))
            .collect::<Vec<_>>(),
    ))
}

fn mutation_result_type(schema: afterburner::ir::SchemaId, returning: bool) -> Type {
    if returning {
        Type::relation(schema)
    } else {
        Type::Unit
    }
}

fn returning_columns<E: Entity>(returning: bool) -> Vec<String> {
    if returning {
        E::COLUMNS
            .iter()
            .map(|column| column.name().to_owned())
            .collect()
    } else {
        Vec::new()
    }
}

fn primary_key_target<E: Entity>() -> Option<ConflictTarget> {
    (!E::PRIMARY_KEY.is_empty()).then(|| {
        ConflictTarget::Columns(
            E::PRIMARY_KEY
                .iter()
                .map(|index| E::COLUMNS[*index].name().to_owned())
                .collect(),
        )
    })
}

fn true_literal(
    editor: &mut IrEditor<'_>,
    block: afterburner::ir::BlockId,
) -> Result<afterburner::ir::ValueId, LoweringError> {
    let literal = editor.append_operation(
        block,
        OperationSpec::new(ScalarOp::Literal(Literal::Boolean(true)))
            .with_result(Type::boolean(false)),
    )?;
    Ok(editor.result(literal, 0)?)
}

fn append_mutation_return(
    editor: &mut IrEditor<'_>,
    root: afterburner::ir::BlockId,
    operation: afterburner::ir::OperationId,
    returning: bool,
) -> Result<(), LoweringError> {
    let value = editor.result(operation, 0)?;
    let terminator = if returning {
        TerminatorOp::QueryReturn
    } else {
        TerminatorOp::CommandReturn
    };
    editor.append_operation(
        root,
        OperationSpec::new(terminator).with_operands(vec![value]),
    )?;
    Ok(())
}

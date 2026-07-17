//! Rendering for PostgreSQL data-mutation statements.

use afterburner::ir::{
    ConflictAction, ConflictTarget, Module, MutationOp, OperationId, OperationKind, TerminatorOp,
    ValueId,
};

use crate::error::RenderError;
use crate::postgres::scalar::{ParamMap, RowScope, render_value};
use crate::postgres::{quote_identifier, table_sql};

pub(crate) struct Renderer<'module> {
    module: &'module Module,
    params: ParamMap,
}

impl<'module> Renderer<'module> {
    pub(crate) fn new(module: &'module Module) -> Self {
        Self {
            module,
            params: ParamMap::default(),
        }
    }

    pub(crate) fn into_bind_order(self) -> Vec<u32> {
        self.params.into_bind_order()
    }

    pub(crate) fn render(&mut self, operation_id: OperationId) -> Result<String, RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent("mutation operation handle is stale"))?;
        let OperationKind::Mutation(mutation) = operation.kind() else {
            return Err(RenderError::inconsistent(
                "mutation renderer received a non-mutation operation",
            ));
        };
        match mutation {
            MutationOp::Insert {
                table,
                columns,
                rows,
                conflict,
                returning,
                ..
            } => {
                let (block, values) = self.yielded_values(operation_id)?;
                let width = columns.len();
                let scope = RowScope::new(block, "inserted", &[]);
                let mut rendered_rows = Vec::with_capacity(*rows as usize);
                for row in values.chunks(width) {
                    let mut rendered = Vec::with_capacity(width);
                    for value in row {
                        rendered.push(render_value(self.module, &mut self.params, &scope, *value)?);
                    }
                    rendered_rows.push(format!("({})", rendered.join(", ")));
                }
                let columns = quote_list(columns)?;
                let mut sql = format!(
                    "INSERT INTO {} ({columns}) VALUES {}",
                    table_sql(table)?,
                    rendered_rows.join(", ")
                );
                if let Some(conflict) = conflict {
                    sql.push_str(" ON CONFLICT");
                    match conflict.target() {
                        Some(ConflictTarget::Columns(columns)) => {
                            sql.push_str(&format!(" ({})", quote_list(columns)?));
                        }
                        Some(ConflictTarget::Constraint(constraint)) => {
                            sql.push_str(&format!(
                                " ON CONSTRAINT {}",
                                quote_identifier(constraint)?
                            ));
                        }
                        None => {}
                    }
                    match conflict.action() {
                        ConflictAction::DoNothing => sql.push_str(" DO NOTHING"),
                        ConflictAction::DoUpdate(assignments) => {
                            let assignments = assignments
                                .iter()
                                .map(|assignment| {
                                    Ok(format!(
                                        "{} = EXCLUDED.{}",
                                        quote_identifier(assignment.target())?,
                                        quote_identifier(assignment.source())?
                                    ))
                                })
                                .collect::<Result<Vec<_>, RenderError>>()?;
                            sql.push_str(&format!(" DO UPDATE SET {}", assignments.join(", ")));
                        }
                    }
                }
                append_returning(&mut sql, returning)?;
                Ok(sql)
            }
            MutationOp::Update {
                table,
                assignments,
                returning,
                ..
            } => {
                let (block, values) = self.yielded_values(operation_id)?;
                let (predicate, values) = values.split_first().ok_or_else(|| {
                    RenderError::inconsistent("update region yields no predicate")
                })?;
                let columns = self.table_columns(mutation)?;
                let scope = RowScope::new(block, "t0", &columns);
                let mut rendered = Vec::with_capacity(assignments.len());
                for (column, value) in assignments.iter().zip(values) {
                    rendered.push(format!(
                        "{} = {}",
                        quote_identifier(column)?,
                        render_value(self.module, &mut self.params, &scope, *value)?
                    ));
                }
                let predicate = render_value(self.module, &mut self.params, &scope, *predicate)?;
                let mut sql = format!(
                    "UPDATE {} AS {} SET {} WHERE {predicate}",
                    table_sql(table)?,
                    quote_identifier("t0")?,
                    rendered.join(", ")
                );
                append_returning(&mut sql, returning)?;
                Ok(sql)
            }
            MutationOp::Delete {
                table, returning, ..
            } => {
                let (block, values) = self.yielded_values(operation_id)?;
                let predicate = *values.first().ok_or_else(|| {
                    RenderError::inconsistent("delete region yields no predicate")
                })?;
                let columns = self.table_columns(mutation)?;
                let scope = RowScope::new(block, "t0", &columns);
                let predicate = render_value(self.module, &mut self.params, &scope, predicate)?;
                let mut sql = format!(
                    "DELETE FROM {} AS {} WHERE {predicate}",
                    table_sql(table)?,
                    quote_identifier("t0")?
                );
                append_returning(&mut sql, returning)?;
                Ok(sql)
            }
        }
    }

    fn table_columns(&self, mutation: &MutationOp) -> Result<Vec<String>, RenderError> {
        let schema = self
            .module
            .schema(mutation.schema())
            .ok_or_else(|| RenderError::inconsistent("mutation table schema is stale"))?;
        Ok(schema
            .fields()
            .iter()
            .map(|field| field.name().to_owned())
            .collect())
    }

    fn yielded_values(
        &self,
        operation_id: OperationId,
    ) -> Result<(afterburner::ir::BlockId, Vec<ValueId>), RenderError> {
        let operation = self
            .module
            .operation(operation_id)
            .ok_or_else(|| RenderError::inconsistent("mutation operation is stale"))?;
        let region = operation
            .regions()
            .first()
            .and_then(|region| self.module.region(*region))
            .ok_or_else(|| RenderError::inconsistent("mutation region is stale"))?;
        let block_id = *region
            .blocks()
            .first()
            .ok_or_else(|| RenderError::inconsistent("mutation region has no block"))?;
        let block = self
            .module
            .block(block_id)
            .ok_or_else(|| RenderError::inconsistent("mutation block is stale"))?;
        let terminator = block
            .operations()
            .last()
            .and_then(|operation| self.module.operation(*operation))
            .ok_or_else(|| RenderError::inconsistent("mutation block has no terminator"))?;
        if terminator.kind() != &OperationKind::Terminator(TerminatorOp::Yield) {
            return Err(RenderError::inconsistent(
                "mutation region must terminate with yield",
            ));
        }
        Ok((block_id, terminator.operands().to_vec()))
    }
}

fn quote_list(columns: &[String]) -> Result<String, RenderError> {
    columns
        .iter()
        .map(|column| quote_identifier(column))
        .collect::<Result<Vec<_>, _>>()
        .map(|columns| columns.join(", "))
}

fn append_returning(sql: &mut String, columns: &[String]) -> Result<(), RenderError> {
    if !columns.is_empty() {
        sql.push_str(" RETURNING ");
        sql.push_str(&quote_list(columns)?);
    }
    Ok(())
}

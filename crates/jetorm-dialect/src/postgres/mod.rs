//! PostgreSQL rendering of AfterBurner IR.
//!
//! Spelling decisions specific to this dialect:
//!
//! - Identifiers are always double-quoted with embedded quotes doubled, and
//!   every `FROM` item receives a generated alias (`"t0"`, `"t1"`, ...), so
//!   column references are unambiguous by construction.
//! - Placeholders are `$n`, dense in order of first appearance, and always
//!   carry an explicit `::type` cast derived from the parameter's SSA type.
//! - `ColumnType::Json` maps onto `jsonb`; temporal literals are built from
//!   epoch arithmetic (`DATE '1970-01-01' + n`) rather than locale-sensitive
//!   text formats.

pub mod ddl;
mod mutation;
mod scalar;
mod select;
mod types;

use afterburner::ir::{Module, OperationKind, TerminatorOp, ValueDefinition, verify_module};

use crate::dialect::Dialect;
use crate::error::RenderError;
use crate::statement::{Statement, StatementResult};

/// The PostgreSQL dialect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Postgres;

impl Dialect for Postgres {
    fn name(&self) -> &'static str {
        "postgresql"
    }

    fn render_statement(&self, module: &Module) -> Result<Statement, RenderError> {
        verify_module(module).map_err(RenderError::InvalidModule)?;
        let root = module
            .block(module.root_block())
            .ok_or_else(|| RenderError::inconsistent("root block handle is stale"))?;
        let terminator = root
            .operations()
            .last()
            .and_then(|operation| module.operation(*operation))
            .ok_or_else(|| RenderError::inconsistent("root block has no terminator"))?;
        let result = match terminator.kind() {
            OperationKind::Terminator(TerminatorOp::QueryReturn) => StatementResult::Rows,
            OperationKind::Terminator(TerminatorOp::CommandReturn) => StatementResult::AffectedRows,
            _ => {
                return Err(RenderError::unsupported(
                    "module root must end in a query or command return",
                ));
            }
        };
        let returned = *terminator
            .operands()
            .first()
            .ok_or_else(|| RenderError::inconsistent("root return has no operand"))?;
        let ValueDefinition::OperationResult { operation, .. } = module
            .value(returned)
            .ok_or_else(|| RenderError::inconsistent("root return references a stale value"))?
            .definition()
        else {
            return Err(RenderError::inconsistent(
                "root return must reference an operation result",
            ));
        };

        match module
            .operation(operation)
            .ok_or_else(|| RenderError::inconsistent("returned operation handle is stale"))?
            .kind()
        {
            OperationKind::Mutation(_) => {
                let mut renderer = mutation::Renderer::new(module);
                let sql = renderer.render(operation)?;
                Ok(Statement::new(sql, renderer.into_bind_order(), result))
            }
            OperationKind::Logical(_) if result == StatementResult::Rows => {
                let mut renderer = select::Renderer::new(module);
                let sql = renderer.render_module()?;
                Ok(Statement::new(sql, renderer.into_bind_order(), result))
            }
            _ => Err(RenderError::unsupported(
                "returned operation is not a PostgreSQL statement root",
            )),
        }
    }
}

/// Quotes one SQL identifier, doubling embedded quotes.
///
/// # Errors
///
/// Returns an error for identifiers PostgreSQL cannot represent.
pub(crate) fn quote_identifier(name: &str) -> Result<String, RenderError> {
    if name.is_empty() {
        return Err(RenderError::inconsistent("identifier must not be empty"));
    }
    if name.contains('\0') {
        return Err(RenderError::unsupported(
            "identifiers cannot contain NUL bytes",
        ));
    }
    // Embedded quotes are vanishingly rare; the common path pays for one
    // allocation, not a `replace` scan-and-copy plus a `format!`.
    if name.contains('"') {
        return Ok(format!("\"{}\"", name.replace('"', "\"\"")));
    }
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('"');
    quoted.push_str(name);
    quoted.push('"');
    Ok(quoted)
}

pub(crate) fn table_sql(table: &afterburner::ir::TableRef) -> Result<String, RenderError> {
    if table.catalog().is_some() {
        return Err(RenderError::unsupported(
            "PostgreSQL cannot reference tables in another catalog",
        ));
    }
    match table.schema() {
        Some(schema) => Ok(format!(
            "{}.{}",
            quote_identifier(schema)?,
            quote_identifier(table.name())?
        )),
        None => quote_identifier(table.name()),
    }
}

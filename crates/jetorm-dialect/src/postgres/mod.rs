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
mod scalar;
mod select;
mod types;

use afterburner::ir::{Module, verify_module};

use crate::dialect::Dialect;
use crate::error::RenderError;
use crate::statement::Statement;

/// The PostgreSQL dialect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Postgres;

impl Dialect for Postgres {
    fn name(&self) -> &'static str {
        "postgresql"
    }

    fn render_query(&self, module: &Module) -> Result<Statement, RenderError> {
        verify_module(module).map_err(RenderError::InvalidModule)?;
        let mut renderer = select::Renderer::new(module);
        let sql = renderer.render_module()?;
        Ok(Statement::new(sql, renderer.into_bind_order()))
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

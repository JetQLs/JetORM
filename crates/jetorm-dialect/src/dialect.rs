use afterburner::ir::Module;

use crate::error::RenderError;
use crate::statement::{Statement, StatementResult};

mod sealed {
    /// Restricts `Dialect` implementations to this crate.
    ///
    /// [`crate::Statement`]'s representation — SQL text plus a bind-position
    /// layout — is an internal contract between the renderer and the
    /// executor, and it will change as dialects gain features. Sealing keeps
    /// that freedom; unsealing later is not a breaking change, so this is the
    /// reversible choice.
    pub trait Sealed {}

    impl Sealed for crate::postgres::Postgres {}
}

/// Renders verified AfterBurner IR into one SQL dialect.
///
/// Implementations own every dialect-specific spelling decision: identifier
/// quoting, placeholder syntax, type names, and literal formats. They accept
/// only whole verified modules, so a rendered [`Statement`] is always
/// consistent with the IR the optimizer saw.
///
/// The trait is sealed: dialects live in this crate, alongside the
/// [`Statement`] representation they must produce.
pub trait Dialect: sealed::Sealed {
    /// Returns the stable dialect name used in diagnostics and cache keys.
    fn name(&self) -> &'static str;

    /// Renders one verified query or mutation module into an executable statement.
    ///
    /// # Errors
    ///
    /// Returns an error when the module fails IR verification, uses an
    /// operation or type this dialect cannot render, or requires a shape SQL
    /// cannot express faithfully.
    fn render_statement(&self, module: &Module) -> Result<Statement, RenderError>;

    /// Renders one row-producing module through the query-only convenience API.
    ///
    /// # Errors
    ///
    /// Returns an error when rendering fails or the module produces only an
    /// affected-row count.
    fn render_query(&self, module: &Module) -> Result<Statement, RenderError> {
        let statement = self.render_statement(module)?;
        if statement.result() != StatementResult::Rows {
            return Err(RenderError::unsupported(
                "render_query requires a row-producing module",
            ));
        }
        Ok(statement)
    }
}

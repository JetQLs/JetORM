use afterburner::ir::Module;

use crate::error::RenderError;
use crate::statement::Statement;

/// Renders verified AfterBurner IR into one SQL dialect.
///
/// Implementations own every dialect-specific spelling decision: identifier
/// quoting, placeholder syntax, type names, and literal formats. They accept
/// only whole verified modules, so a rendered [`Statement`] is always
/// consistent with the IR the optimizer saw.
pub trait Dialect {
    /// Returns the stable dialect name used in diagnostics and cache keys.
    fn name(&self) -> &'static str;

    /// Renders one verified query module into an executable statement.
    ///
    /// # Errors
    ///
    /// Returns an error when the module fails IR verification, uses an
    /// operation or type this dialect cannot render, or requires a shape SQL
    /// cannot express faithfully.
    fn render_query(&self, module: &Module) -> Result<Statement, RenderError>;
}

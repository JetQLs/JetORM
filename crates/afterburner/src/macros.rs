/// Lowers a typed ORM query into AfterBurner IR and verifies the result.
///
/// The query expression must implement [`crate::IntoAfterBurnerIr`]. SQL text is
/// deliberately not part of this interface. The macro uses `$crate` internally,
/// so it remains valid when AfterBurner is called from another module or when the
/// dependency is renamed.
///
/// # Examples
///
/// ```
/// use afterburner::{AfterBurnerOptions, afterburner};
/// use afterburner::ir::{
///     Field, Literal, LogicalOp, Module, OperationSpec, Schema, SqlType,
///     TerminatorOp, Type,
/// };
///
/// let mut module = Module::new();
/// let root = module.root_block();
/// {
///     let mut editor = module.editor();
///     let schema = editor.intern_schema(Schema::new(vec![Field::new(
///         "enabled",
///         Type::scalar(SqlType::Boolean, false),
///     )]));
///     let values = editor
///         .append_operation(
///             root,
///             OperationSpec::new(LogicalOp::Values {
///                 rows: vec![vec![Literal::Boolean(true)]],
///             })
///             .with_result(Type::relation(schema)),
///         )
///         .unwrap();
///     let relation = editor.result(values, 0).unwrap();
///     editor
///         .append_operation(
///             root,
///             OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
///         )
///         .unwrap();
/// }
///
/// let verified = afterburner!(module, options = AfterBurnerOptions::new());
/// assert!(verified.is_ok());
/// ```
#[macro_export]
macro_rules! afterburner {
    ($query:expr, options = $options:expr $(,)?) => {{
        let __afterburner_options = $options;
        match $crate::IntoAfterBurnerIr::into_afterburner_ir($query) {
            ::core::result::Result::Err(error) => {
                ::core::result::Result::Err($crate::AfterBurnerError::Lowering(error))
            }
            ::core::result::Result::Ok(module) => {
                if __afterburner_options.verifies_ir() {
                    match $crate::ir::verify_module(&module) {
                        ::core::result::Result::Ok(()) => ::core::result::Result::Ok(module),
                        ::core::result::Result::Err(errors) => ::core::result::Result::Err(
                            $crate::AfterBurnerError::Verification(errors),
                        ),
                    }
                } else {
                    ::core::result::Result::Ok(module)
                }
            }
        }
    }};
    ($query:expr $(,)?) => {
        $crate::afterburner!($query, options = $crate::AfterBurnerOptions::default(),)
    };
}

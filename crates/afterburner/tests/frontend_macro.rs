use std::{convert::Infallible, error::Error, fmt};

use afterburner::{
    AfterBurnerError, AfterBurnerOptions, IntoAfterBurnerIr, afterburner,
    ir::{
        Field, Literal, LogicalOp, Module, OperationSpec, Schema, SqlType, TerminatorOp, Type,
        verify_module,
    },
};

mod external_frontend {
    use super::{
        Field, Infallible, IntoAfterBurnerIr, Literal, LogicalOp, Module, OperationSpec, Schema,
        SqlType, TerminatorOp, Type,
    };

    pub struct TypedQuery;

    impl IntoAfterBurnerIr for TypedQuery {
        type Error = Infallible;

        fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
            let mut module = Module::new();
            let root = module.root_block();
            {
                let mut editor = module.editor();
                let schema = editor.intern_schema(Schema::new(vec![Field::new(
                    "enabled",
                    Type::scalar(SqlType::Boolean, false),
                )]));
                let values = editor
                    .append_operation(
                        root,
                        OperationSpec::new(LogicalOp::Values {
                            rows: vec![vec![Literal::Boolean(true)]],
                        })
                        .with_result(Type::relation(schema)),
                    )
                    .expect("test query builds one values operation");
                let relation = editor
                    .result(values, 0)
                    .expect("values operation has one result");
                editor
                    .append_operation(
                        root,
                        OperationSpec::new(TerminatorOp::QueryReturn).with_operands(vec![relation]),
                    )
                    .expect("test query builds one return operation");
            }
            Ok(module)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FrontendError;

impl fmt::Display for FrontendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unsupported typed query")
    }
}

impl Error for FrontendError {}

struct FailingQuery;

impl IntoAfterBurnerIr for FailingQuery {
    type Error = FrontendError;

    fn into_afterburner_ir(self) -> Result<Module, Self::Error> {
        Err(FrontendError)
    }
}

#[test]
fn macro_lowers_a_query_defined_in_another_module() {
    let module = afterburner!(external_frontend::TypedQuery)
        .expect("typed frontend query should lower to valid IR");

    assert!(verify_module(&module).is_ok());
}

#[test]
fn macro_preserves_frontend_lowering_errors() {
    let error = afterburner!(FailingQuery).expect_err("lowering should fail");

    assert_eq!(error, AfterBurnerError::Lowering(FrontendError));
}

#[test]
fn macro_verifies_ir_by_default_and_honors_options() {
    let verified = afterburner!(Module::new());
    assert!(matches!(
        verified,
        Err(AfterBurnerError::Verification(errors)) if !errors.is_empty()
    ));

    let unchecked = afterburner!(
        Module::new(),
        options = AfterBurnerOptions::new().with_ir_verification(false),
    )
    .expect("explicitly disabled verification should return the module");
    assert!(verify_module(&unchecked).is_err());
}

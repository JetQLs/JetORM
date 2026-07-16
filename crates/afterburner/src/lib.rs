//! Compiler-oriented query IR for JetORM.
//!
//! AfterBurner models a query as typed, multi-level SSA. Relational operations
//! produce relation values, while their row expressions live in nested regions.
//! Generational handles, constant-time inverse use-list unlinking, recursive
//! ownership erasure, type-safe native attachments, whole-module verification,
//! and stable structural fingerprints provide the contracts needed by optimizer
//! and PGO layers without coupling the IR to a particular frontend or backend.
//!
//! # Example
//!
//! ```
//! use afterburner::ir::{
//!     Field, Literal, LogicalOp, Module, OperationSpec, Schema, SqlType,
//!     TerminatorOp, Type, verify_module,
//! };
//!
//! let mut module = Module::new();
//! let root = module.root_block();
//! {
//!     let mut editor = module.editor();
//!     let schema = editor.intern_schema(Schema::new(vec![Field::new(
//!         "id",
//!         Type::scalar(SqlType::Integer { bits: 64, signed: true }, false),
//!     )]));
//!     let values = editor
//!         .append_operation(
//!             root,
//!             OperationSpec::new(LogicalOp::Values {
//!                 rows: vec![vec![Literal::Integer(1)]],
//!             })
//!             .with_result(Type::relation(schema)),
//!         )
//!         .unwrap();
//!     let relation = editor.result(values, 0).unwrap();
//!     editor
//!         .append_operation(
//!             root,
//!             OperationSpec::new(TerminatorOp::QueryReturn)
//!                 .with_operands(vec![relation]),
//!         )
//!         .unwrap();
//! }
//!
//! verify_module(&module).unwrap();
//! ```
//!
//! The crate defines IR construction and integrity primitives. SQL parsing,
//! optimizer pipelines, physical planning, and live profile storage belong to
//! layers built on top of this representation.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ir;

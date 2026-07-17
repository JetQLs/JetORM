//! Derive macros generating JetORM entity metadata.
//!
//! `#[derive(JetModel)]` turns one plain data struct into a complete entity
//! description: a zero-sized entity marker implementing `Entity`, a module of
//! zero-sized column markers implementing `Column`, and a positional `Model`
//! conversion. All metadata is emitted as constants, so schema information is
//! available at compile time to query builders and the migration differ.
//!
//! # Container attributes
//!
//! - `#[jet(table = "users")]` — required SQL table name.
//! - `#[jet(schema = "public")]` — optional database schema qualifier.
//! - `#[jet(module = "user")]` — column-marker module name; defaults to the
//!   snake_case struct name.
//! - `#[jet(crate_path = "::jetorm")]` — path to the JetORM facade crate in
//!   generated code; override when re-exporting under another name.
//!
//! # Field attributes
//!
//! - `#[jet(primary_key)]` — column participates in the primary key.
//! - `#[jet(auto_increment)]` — value is database-generated on insert.
//! - `#[jet(unique)]` — column carries a uniqueness constraint.
//! - `#[jet(column = "email_address")]` — SQL column name override; defaults
//!   to the field name.
//!
//! # Example
//!
//! ```ignore
//! #[derive(JetModel)]
//! #[jet(table = "users")]
//! pub struct User {
//!     #[jet(primary_key, auto_increment)]
//!     pub id: i64,
//!     pub name: String,
//!     pub email: Option<String>,
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod attrs;
mod expand;
mod partial;
mod types;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Derives entity metadata, column markers, and model conversion.
///
/// See the crate-level documentation for the accepted `#[jet(...)]`
/// attributes and a usage example.
#[proc_macro_derive(JetModel, attributes(jet))]
pub fn derive_jet_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derives a partial model decoding a projection of one entity.
///
/// The struct holds a subset of the entity's columns; each field maps to
/// the column marker with the field's PascalCase name inside the module
/// named by `#[jet(columns = "...")]`; a field named differently from the
/// entity's names the entity field with `#[jet(column = "email")]`. Field
/// types must match the columns' field
/// types exactly — `Option` for nullable columns — checked at compile time.
///
/// ```ignore
/// #[derive(JetPartial)]
/// #[jet(columns = "user")]
/// pub struct UserSummary {
///     pub id: i64,
///     pub name: String,
/// }
///
/// let rows: Vec<UserSummary> = UserEntity::find()
///     .select_as::<UserSummary>()
///     .all(&db)
///     .await?;
/// ```
#[proc_macro_derive(JetPartial, attributes(jet))]
pub fn derive_jet_partial(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    partial::expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

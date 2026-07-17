//! Expansion of `#[derive(JetPartial)]`.
//!
//! A partial model is a plain struct holding a subset of one entity's
//! columns. The derive implements `ColumnList` for the struct itself, so it
//! plugs into the existing projection machinery: `select_as::<Partial>()`
//! fetches exactly the declared columns and decodes each row into the
//! struct. Field types are checked against the columns' field types at
//! compile time — a partial cannot silently disagree with its entity about
//! nullability or Rust type.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, Path};

use crate::attrs;
use crate::expand::{pascal_case, type_ident, unraw};

struct PartialAttrs {
    /// Path of the column-marker module the entity derive generated.
    columns: Path,
    crate_path: Path,
}

fn parse_container(input: &DeriveInput) -> syn::Result<PartialAttrs> {
    let mut columns = None;
    let mut crate_path = None;

    for attribute in attrs::jet_attributes(&input.attrs) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("columns") {
                let literal = meta.value()?.parse::<LitStr>()?;
                columns = Some(literal.parse::<Path>()?);
                Ok(())
            } else if meta.path.is_ident("crate_path") {
                let literal = meta.value()?.parse::<LitStr>()?;
                crate_path = Some(literal.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error("unknown container attribute; expected `columns` or `crate_path`"))
            }
        })?;
    }

    let Some(columns) = columns else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetPartial)]` requires `#[jet(columns = \"...\")]` naming \
             the entity's column-marker module (for example `user`)",
        ));
    };
    let crate_path = match crate_path {
        Some(path) => path,
        None => syn::parse_str::<Path>("::jetorm")?,
    };

    Ok(PartialAttrs {
        columns,
        crate_path,
    })
}

/// Entity-field override parsed from a field's `#[jet(...)]` attribute.
///
/// The value names the entity's field the same way the entity derive saw
/// it — `column = "email"` — and goes through the same PascalCase and
/// keyword-escaping transform, so it always agrees with the marker the
/// entity derive actually generated (`Email`, or `r#Type` for a field
/// named `type`). PascalCase input is accepted unchanged.
fn parse_field_marker(field: &syn::Field) -> syn::Result<Option<syn::Ident>> {
    let mut marker = None;
    for attribute in attrs::jet_attributes(&field.attrs) {
        attribute.parse_nested_meta(|meta| {
            if !meta.path.is_ident("column") {
                return Err(meta.error("unknown field attribute; expected `column`"));
            }
            let literal = meta.value()?.parse::<LitStr>()?;
            let name = literal.value();
            if name.is_empty()
                || !name
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
            {
                return Err(syn::Error::new(
                    literal.span(),
                    format!("`column = {name:?}` is not a field name"),
                ));
            }
            marker = Some(
                type_ident(&pascal_case(&name), literal.span())
                    .map_err(|message| syn::Error::new(literal.span(), message))?,
            );
            Ok(())
        })?;
    }
    Ok(marker)
}

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(JetPartial)]` does not support generic structs",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetPartial)]` supports only structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetPartial)]` requires named fields",
        ));
    };
    if fields.named.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetPartial)]` requires at least one field",
        ));
    }

    let container = parse_container(input)?;
    let cr = &container.crate_path;
    let module = &container.columns;
    let partial_ident = &input.ident;

    let mut markers = Vec::with_capacity(fields.named.len());
    let mut field_idents = Vec::with_capacity(fields.named.len());
    let mut field_types = Vec::with_capacity(fields.named.len());
    for field in &fields.named {
        let field_ident = field
            .ident
            .clone()
            .expect("named fields always carry an identifier");
        let marker = match parse_field_marker(field)? {
            Some(explicit) => explicit,
            None => type_ident(&pascal_case(&unraw(&field_ident)), field_ident.span())
                .map_err(|message| syn::Error::new_spanned(field, message))?,
        };
        markers.push(quote!(#module::#marker));
        field_idents.push(field_ident);
        field_types.push(field.ty.clone());
    }

    // The entity is a fact of the column module; the first marker names it
    // and the remaining markers must agree, which `Column<Entity = E>` in
    // the trait bound enforces when this impl is used.
    let first_marker = &markers[0];
    let entity = quote!(<#first_marker as #cr::Column>::Entity);
    let width = markers.len();

    // Each field's type must be exactly the column's field type — `Option`
    // for nullable columns, the bare type otherwise. `identity` only
    // coerces between equal types, so a mismatch is a compile error at the
    // derive site instead of a decode error at runtime.
    let type_checks = markers.iter().zip(&field_types).map(|(marker, ty)| {
        quote! {
            const _: fn(<#marker as #cr::Column>::Field) -> #ty =
                ::core::convert::identity;
        }
    });

    let decode_fields = markers.iter().zip(&field_idents).map(|(marker, ident)| {
        quote! {
            #ident: <<#marker as #cr::Column>::Field as #cr::SqlValue>::from_value(
                values.next().expect("row width was checked above"),
            )
            .map_err(|mismatch| #cr::DecodeError::Column {
                name: <#marker as #cr::Column>::meta().name(),
                mismatch,
            })?
        }
    });

    Ok(quote! {
        #(#type_checks)*

        #[automatically_derived]
        impl #cr::ColumnList<#entity> for #partial_ident {
            type Row = Self;

            fn indexes() -> ::std::vec::Vec<usize> {
                ::std::vec![#(<#markers as #cr::Column>::INDEX),*]
            }

            fn decode(
                values: ::std::vec::Vec<#cr::Value>,
            ) -> ::std::result::Result<Self, #cr::DecodeError> {
                if values.len() != #width {
                    return ::std::result::Result::Err(#cr::DecodeError::ColumnCount {
                        expected: #width,
                        actual: values.len(),
                    });
                }
                let mut values = values.into_iter();
                ::std::result::Result::Ok(Self {
                    #(#decode_fields),*
                })
            }
        }
    })
}

//! Expansion of `#[derive(JetEnum)]`.
//!
//! A string-backed enum column: the Rust enum stores as `text` holding one
//! stable name per variant, so it works on any database without a native
//! enum type and diffs like any text column. The derive implements
//! `SqlValue`, which is all a column type is — a `JetEnum` field on a
//! `JetModel` struct needs no further annotation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, Path};

use crate::attrs;
use crate::expand::{snake_case, unraw};

fn parse_crate_path(input: &DeriveInput) -> syn::Result<Path> {
    let mut crate_path = None;
    for attribute in attrs::jet_attributes(&input.attrs) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("crate_path") {
                let literal = meta.value()?.parse::<LitStr>()?;
                crate_path = Some(literal.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error("unknown container attribute; expected `crate_path`"))
            }
        })?;
    }
    match crate_path {
        Some(path) => Ok(path),
        None => Ok(syn::parse_str::<Path>("::jetorm")?),
    }
}

/// Stored-name override parsed from a variant's `#[jet(...)]` attribute.
fn parse_variant_name(variant: &syn::Variant) -> syn::Result<Option<String>> {
    let mut name = None;
    for attribute in attrs::jet_attributes(&variant.attrs) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let literal = meta.value()?.parse::<LitStr>()?;
                if literal.value().is_empty() {
                    return Err(syn::Error::new(
                        literal.span(),
                        "the stored name must not be empty",
                    ));
                }
                name = Some(literal.value());
                Ok(())
            } else {
                Err(meta.error("unknown variant attribute; expected `rename`"))
            }
        })?;
    }
    Ok(name)
}

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(JetEnum)]` does not support generic enums",
        ));
    }
    let Data::Enum(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetEnum)]` supports only enums",
        ));
    };
    if data.variants.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetEnum)]` requires at least one variant",
        ));
    }

    let cr = parse_crate_path(input)?;
    let enum_ident = &input.ident;

    let mut seen = std::collections::BTreeMap::new();
    let mut into_arms = Vec::with_capacity(data.variants.len());
    let mut from_arms = Vec::with_capacity(data.variants.len());
    let mut names = Vec::with_capacity(data.variants.len());
    for variant in &data.variants {
        let Fields::Unit = variant.fields else {
            return Err(syn::Error::new_spanned(
                variant,
                "`#[derive(JetEnum)]` requires unit variants; a payload has \
                 no stored representation",
            ));
        };
        let ident = &variant.ident;
        let stored = match parse_variant_name(variant)? {
            Some(explicit) => explicit,
            None => snake_case(&unraw(ident)),
        };
        if let Some(previous) = seen.insert(stored.clone(), ident.clone()) {
            return Err(syn::Error::new_spanned(
                variant,
                format!(
                    "stored name {stored:?} is already used by variant `{previous}`; \
                     rename one with `#[jet(rename = \"...\")]`"
                ),
            ));
        }
        into_arms.push(quote!(Self::#ident => #stored));
        from_arms.push(quote!(#stored => ::core::result::Result::Ok(Self::#ident)));
        names.push(stored);
    }

    Ok(quote! {
        #[automatically_derived]
        impl #cr::SqlValue for #enum_ident {
            const COLUMN_TYPE: #cr::ColumnType = #cr::ColumnType::Text;

            fn into_value(self) -> #cr::Value {
                #cr::Value::Text(
                    match self {
                        #(#into_arms,)*
                    }
                    .to_owned(),
                )
            }

            fn from_value(
                value: #cr::Value,
            ) -> ::core::result::Result<Self, #cr::ValueTypeMismatch> {
                let text = <::std::string::String as #cr::SqlValue>::from_value(value)?;
                match text.as_str() {
                    #(#from_arms,)*
                    _ => ::core::result::Result::Err(#cr::ValueTypeMismatch::new(
                        #cr::ColumnType::Text,
                        ::core::concat!(
                            "text outside the ",
                            ::core::stringify!(#enum_ident),
                            " variant names",
                        ),
                    )),
                }
            }
        }
    })
}

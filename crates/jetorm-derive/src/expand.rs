use proc_macro2::{Ident, TokenStream};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields};

use crate::attrs::{self, FieldAttrs};
use crate::types::{self, ColumnSpec};

struct EntityColumn {
    field_ident: Ident,
    field_ty: syn::Type,
    marker_ident: Ident,
    sql_name: String,
    spec: ColumnSpec,
    attrs: FieldAttrs,
}

pub fn expand(input: &DeriveInput) -> syn::Result<TokenStream> {
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "`#[derive(JetModel)]` does not support generic structs",
        ));
    }
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetModel)]` supports only structs",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetModel)]` requires named fields",
        ));
    };
    if fields.named.is_empty() {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetModel)]` requires at least one field",
        ));
    }

    let container = attrs::parse_container(input)?;
    let mut columns = Vec::with_capacity(fields.named.len());
    for field in &fields.named {
        let field_ident = field
            .ident
            .clone()
            .expect("named fields always carry an identifier");
        let field_attrs = attrs::parse_field(field)?;
        if field_attrs.auto_increment && !field_attrs.primary_key {
            return Err(syn::Error::new_spanned(
                field,
                "`auto_increment` requires `primary_key` on the same field",
            ));
        }
        let spec = types::resolve(&field.ty)?;
        let sql_name = field_attrs
            .column
            .clone()
            .unwrap_or_else(|| field_ident.to_string());
        columns.push(EntityColumn {
            marker_ident: format_ident!("{}", pascal_case(&field_ident.to_string())),
            sql_name,
            field_ty: field.ty.clone(),
            spec,
            attrs: field_attrs,
            field_ident,
        });
    }

    let cr = &container.crate_path;
    let vis = &input.vis;
    let model_ident = &input.ident;
    let entity_ident = format_ident!("{model_ident}Entity");
    let module_ident = format_ident!(
        "{}",
        container
            .module
            .clone()
            .unwrap_or_else(|| snake_case(&model_ident.to_string()))
    );

    let table_meta = {
        let table = &container.table;
        let base = quote!(#cr::TableMeta::new(#table));
        match &container.schema {
            Some(schema) => quote!(#base.with_schema(#schema)),
            None => base,
        }
    };

    let column_metas = columns.iter().map(|column| {
        let sql_name = &column.sql_name;
        let rust_name = column.field_ident.to_string();
        let column_type = &column.spec.column_type;
        let mut meta =
            quote!(#cr::ColumnMeta::new(#sql_name, #rust_name, #cr::ColumnType::#column_type));
        if column.spec.nullable {
            meta = quote!(#meta.nullable());
        }
        if column.attrs.primary_key {
            meta = quote!(#meta.primary_key());
        }
        if column.attrs.auto_increment {
            meta = quote!(#meta.auto_increment());
        }
        if column.attrs.unique {
            meta = quote!(#meta.unique());
        }
        meta
    });

    let primary_key_indices = columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.attrs.primary_key)
        .map(|(index, _)| index);

    let into_value_fields = columns.iter().map(|column| {
        let field_ident = &column.field_ident;
        let field_ty = &column.field_ty;
        quote!(<#field_ty as #cr::SqlValue>::into_value(self.#field_ident))
    });

    let from_value_fields = columns.iter().map(|column| {
        let field_ident = &column.field_ident;
        let field_ty = &column.field_ty;
        let sql_name = &column.sql_name;
        quote! {
            #field_ident: <#field_ty as #cr::SqlValue>::from_value(
                values.next().expect("row width was checked above"),
            )
            .map_err(|mismatch| #cr::DecodeError::Column {
                name: #sql_name,
                mismatch,
            })?
        }
    });

    let column_markers = columns.iter().enumerate().map(|(index, column)| {
        let marker_ident = &column.marker_ident;
        let inner_ty = &column.spec.inner;
        let nullable = column.spec.nullable;
        let doc = format!(
            "Column marker for `{}.{}`.",
            container.table, column.sql_name
        );
        quote! {
            #[doc = #doc]
            #[derive(Clone, Copy, Debug)]
            pub struct #marker_ident;

            #[automatically_derived]
            impl #cr::Column for #marker_ident {
                type Entity = super::#entity_ident;
                type Rust = #inner_ty;
                const INDEX: usize = #index;
                const NULLABLE: bool = #nullable;
            }
        }
    });

    let entity_doc = format!("Entity marker for the `{}` table.", container.table);
    let module_doc = format!("Column markers for the `{}` table.", container.table);
    let column_count = columns.len();

    Ok(quote! {
        #[doc = #entity_doc]
        #[derive(Clone, Copy, Debug)]
        #vis struct #entity_ident;

        #[automatically_derived]
        impl #cr::Entity for #entity_ident {
            type Model = #model_ident;
            const TABLE: #cr::TableMeta = #table_meta;
            const COLUMNS: &'static [#cr::ColumnMeta] = &[#(#column_metas),*];
            const PRIMARY_KEY: &'static [usize] = &[#(#primary_key_indices),*];
        }

        #[automatically_derived]
        impl #cr::Model for #model_ident {
            type Entity = #entity_ident;

            fn into_values(self) -> ::std::vec::Vec<#cr::Value> {
                ::std::vec![#(#into_value_fields),*]
            }

            fn from_values(
                values: ::std::vec::Vec<#cr::Value>,
            ) -> ::std::result::Result<Self, #cr::DecodeError> {
                if values.len() != #column_count {
                    return ::std::result::Result::Err(#cr::DecodeError::ColumnCount {
                        expected: #column_count,
                        actual: values.len(),
                    });
                }
                let mut values = values.into_iter();
                ::std::result::Result::Ok(Self {
                    #(#from_value_fields),*
                })
            }
        }

        #[doc = #module_doc]
        #vis mod #module_ident {
            #[allow(unused_imports)]
            use super::*;

            #(#column_markers)*
        }
    })
}

fn snake_case(input: &str) -> String {
    let mut output = String::with_capacity(input.len() + 4);
    for (index, character) in input.chars().enumerate() {
        if character.is_uppercase() {
            if index > 0 {
                output.push('_');
            }
            output.extend(character.to_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

fn pascal_case(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut uppercase_next = true;
    for character in input.chars() {
        if character == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            output.extend(character.to_uppercase());
            uppercase_next = false;
        } else {
            output.push(character);
        }
    }
    output
}

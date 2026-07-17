use std::collections::BTreeMap;

use proc_macro2::{Ident, Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields};

use crate::attrs::{self, FieldAttrs};
use crate::types::{self, ColumnSpec};

struct EntityColumn {
    field_ident: Ident,
    field_ty: syn::Type,
    marker_ident: Ident,
    sql_name: String,
    /// Field name as written, with any raw-identifier prefix removed.
    rust_name: String,
    spec: ColumnSpec,
    attrs: FieldAttrs,
}

/// Keywords that must be spelled `r#name` to be used as an identifier.
///
/// `self`, `Self`, `super`, and `crate` are deliberately absent: they are not
/// valid raw identifiers, so names colliding with them are rejected instead.
const RAW_ONLY_KEYWORDS: &[&str] = &[
    "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "do", "dyn",
    "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl", "in", "let",
    "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub", "ref", "return",
    "static", "struct", "trait", "true", "try", "type", "typeof", "unsafe", "unsized", "use",
    "virtual", "where", "while", "yield",
];

/// Names that no identifier — raw or otherwise — may take.
const RESERVED_NAMES: &[&str] = &["self", "Self", "super", "crate"];

/// Strips a raw identifier's `r#` prefix, leaving the name it spells.
pub(crate) fn unraw(ident: &Ident) -> String {
    let spelled = ident.to_string();
    spelled
        .strip_prefix("r#")
        .map_or(spelled.clone(), ToOwned::to_owned)
}

/// Builds an identifier, escaping it as raw when the name is a keyword.
pub(crate) fn type_ident(name: &str, span: Span) -> Result<Ident, String> {
    if RESERVED_NAMES.contains(&name) {
        return Err(format!(
            "the generated name `{name}` is reserved by Rust and cannot be escaped; \
             rename the field or set `#[jet(column = \"...\")]`"
        ));
    }
    if RAW_ONLY_KEYWORDS.contains(&name) {
        return Ok(Ident::new_raw(name, span));
    }
    Ok(Ident::new(name, span))
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
    let mut claimed_names: BTreeMap<String, Ident> = BTreeMap::new();
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
        let spec = types::resolve(&field.ty);
        // A raw identifier's `r#` prefix is Rust spelling, not part of the
        // name: field `r#type` is the column `type` and the marker `Type`.
        let rust_name = unraw(&field_ident);
        let sql_name = field_attrs
            .column
            .clone()
            .unwrap_or_else(|| rust_name.clone());
        if sql_name.is_empty() {
            return Err(syn::Error::new_spanned(
                field,
                "column name must not be empty",
            ));
        }
        if let Some(previous) = claimed_names.insert(sql_name.clone(), field_ident.clone()) {
            return Err(syn::Error::new_spanned(
                field,
                format!(
                    "column {sql_name:?} is already mapped by field `{previous}`; \
                     two fields cannot share one column"
                ),
            ));
        }
        columns.push(EntityColumn {
            marker_ident: type_ident(&pascal_case(&rust_name), field_ident.span())
                .map_err(|message| syn::Error::new_spanned(field, message))?,
            sql_name,
            rust_name,
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
    // A struct named e.g. `Type` yields the module name `type`, which only
    // exists as a raw identifier.
    let module_ident = type_ident(
        &container
            .module
            .clone()
            .unwrap_or_else(|| snake_case(&unraw(model_ident))),
        model_ident.span(),
    )
    .map_err(|message| {
        syn::Error::new_spanned(
            model_ident,
            format!("{message}; or set `#[jet(module = \"...\")]`"),
        )
    })?;

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
        let rust_name = &column.rust_name;
        // The SQL kind comes from the `SqlValue` trait, not from a syntactic
        // table: any type implementing `SqlValue` is a column type, aliases
        // included, and an unsupported type fails with a trait error at the
        // field rather than a macro error.
        let column_type = column.attrs.column_type.as_ref().map_or_else(
            || {
                let inner = &column.spec.inner;
                quote!(<#inner as #cr::SqlValue>::COLUMN_TYPE)
            },
            |variant| quote!(#cr::ColumnType::#variant),
        );
        let mut meta = quote!(#cr::ColumnMeta::new(#sql_name, #rust_name, #column_type));
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

    let value_arms = columns.iter().enumerate().map(|(index, column)| {
        let field_ident = &column.field_ident;
        let field_ty = &column.field_ty;
        quote! {
            #index => ::core::option::Option::Some(
                <#field_ty as #cr::SqlValue>::into_value(self.#field_ident.clone()),
            )
        }
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

    // Marker names already claimed by columns; relation markers share the
    // module, so a collision must be a spanned derive error rather than a
    // bare rustc duplicate-definition error deep in generated code. Two
    // columns can also collide with each other — `user_id` and `userId`
    // both yield `UserId` — so claiming is checked, not collected.
    let mut claimed_markers: BTreeMap<String, String> = BTreeMap::new();
    for column in &columns {
        if let Some(previous) =
            claimed_markers.insert(column.marker_ident.to_string(), column.rust_name.clone())
        {
            return Err(syn::Error::new(
                column.field_ident.span(),
                format!(
                    "fields `{previous}` and `{}` generate the same column marker                      `{}`; rename one",
                    column.rust_name, column.marker_ident
                ),
            ));
        }
    }
    // The references path must mean the same type inside the marker module
    // (where this table's own markers shadow glob imports) and at the outer
    // scope (where FOREIGN_KEYS lives). Hidden aliases in the marker-free
    // nested module give both sides one resolution point.
    let relation_target_aliases = columns
        .iter()
        .filter(|column| column.attrs.references.is_some())
        .enumerate()
        .map(|(position, column)| {
            let target = column.attrs.references.as_ref().expect("filtered above");
            let alias = format_ident!("RelationTarget{position}");
            quote! {
                pub type #alias = #target;
            }
        })
        .collect::<Vec<_>>();
    let mut relation_position = 0usize;
    let relation_markers = columns
        .iter()
        .filter_map(|column| {
            let _ = column.attrs.references.as_ref()?;
            // `author_id` names the edge `author` by convention; a field
            // without the suffix has no usable default, since the bare
            // field name is already the column marker's.
            let relation_name = match column.attrs.relation.clone() {
                Some(explicit) => explicit,
                None => match column.rust_name.strip_suffix("_id") {
                    Some(stem) if !stem.is_empty() => stem.to_owned(),
                    _ => {
                        return Some(Err(syn::Error::new(
                            column.field_ident.span(),
                            format!(
                                "cannot derive a relation name from `{}`; \
                                 set `#[jet(relation = \"...\")]` on the field",
                                column.rust_name
                            ),
                        )));
                    }
                },
            };
            let marker = match type_ident(&pascal_case(&relation_name), column.field_ident.span()) {
                Ok(marker) => marker,
                Err(message) => {
                    return Some(Err(syn::Error::new(column.field_ident.span(), message)));
                }
            };
            if let Some(previous) =
                claimed_markers.insert(marker.to_string(), relation_name.clone())
            {
                return Some(Err(syn::Error::new(
                    column.field_ident.span(),
                    format!(
                        "relation `{relation_name}` generates marker `{marker}`, which \
                         `{previous}` already claims; rename it with \
                         `#[jet(relation = \"...\")]`"
                    ),
                )));
            }
            let source_marker = &column.marker_ident;
            let on_delete = column
                .attrs
                .on_delete
                .clone()
                .unwrap_or_else(|| Ident::new("NoAction", column.field_ident.span()));
            let on_update = column
                .attrs
                .on_update
                .clone()
                .unwrap_or_else(|| Ident::new("NoAction", column.field_ident.span()));
            let doc = format!(
                "Relation marker `{}`: `{}.{}` references the target column.",
                relation_name, container.table, column.sql_name,
            );
            let target_alias = format_ident!("RelationTarget{relation_position}");
            relation_position += 1;
            Some(Ok(quote! {
                #[doc = #doc]
                #[derive(Clone, Copy, Debug, Default)]
                pub struct #marker;

                #[automatically_derived]
                impl #cr::Relation for #marker {
                    type Source = super::#entity_ident;
                    type Target = <__jet_fields::#target_alias as #cr::Column>::Entity;
                    type SourceColumn = #source_marker;
                    type TargetColumn = __jet_fields::#target_alias;
                    const NAME: &'static str = #relation_name;
                    const TO_ONE: bool = true;
                    const FOREIGN_KEY: ::core::option::Option<#cr::ForeignKeyMeta> =
                        ::core::option::Option::Some(#cr::ForeignKeyMeta::new(
                            #cr::ReferentialAction::#on_delete,
                            #cr::ReferentialAction::#on_update,
                        ));
                }
            }))
        })
        .collect::<syn::Result<Vec<_>>>()?;

    let mut foreign_key_position = 0usize;
    let foreign_key_refs = columns.iter().enumerate().filter_map(|(index, column)| {
        let _ = column.attrs.references.as_ref()?;
        let target_alias = format_ident!("RelationTarget{foreign_key_position}");
        foreign_key_position += 1;
        let target = quote!(#module_ident::__jet_fields::#target_alias);
        let target = &target;
        let on_delete = column
            .attrs
            .on_delete
            .clone()
            .unwrap_or_else(|| Ident::new("NoAction", column.field_ident.span()));
        let on_update = column
            .attrs
            .on_update
            .clone()
            .unwrap_or_else(|| Ident::new("NoAction", column.field_ident.span()));
        Some(quote! {
            #cr::ForeignKeyRef::new(
                #index,
                <<#target as #cr::Column>::Entity as #cr::Entity>::TABLE,
                <<#target as #cr::Column>::Entity as #cr::Entity>::COLUMNS
                    [<#target as #cr::Column>::INDEX]
                    .name(),
                #cr::ForeignKeyMeta::new(
                    #cr::ReferentialAction::#on_delete,
                    #cr::ReferentialAction::#on_update,
                ),
            )
        })
    });

    // Field types are re-resolved through hidden aliases in a nested
    // module: inside the column module the markers themselves shadow any
    // user type sharing a marker's name (`status: Status`), so the alias
    // module — which contains no markers — is where the user's spelling of
    // the type still means what it meant on the struct.
    let field_aliases = columns.iter().enumerate().map(|(index, column)| {
        let rust_alias = format_ident!("R{index}");
        let field_alias = format_ident!("F{index}");
        let inner_ty = &column.spec.inner;
        let field_ty = &column.field_ty;
        quote! {
            pub type #rust_alias = #inner_ty;
            pub type #field_alias = #field_ty;
        }
    });

    let column_markers = columns.iter().enumerate().map(|(index, column)| {
        let marker_ident = &column.marker_ident;
        let rust_alias = format_ident!("R{index}");
        let field_alias = format_ident!("F{index}");
        let nullable = column.spec.nullable;
        let doc = format!(
            "Column marker for `{}.{}`.",
            container.table, column.sql_name
        );
        quote! {
            #[doc = #doc]
            #[derive(Clone, Copy, Debug, Default)]
            pub struct #marker_ident;

            #[automatically_derived]
            impl #cr::Column for #marker_ident {
                type Entity = super::#entity_ident;
                type Rust = __jet_fields::#rust_alias;
                type Field = __jet_fields::#field_alias;
                const INDEX: usize = #index;
                const NULLABLE: bool = #nullable;
            }
        }
    });

    let primary_key_markers: Vec<&Ident> = columns
        .iter()
        .filter(|column| column.attrs.primary_key)
        .map(|column| &column.marker_ident)
        .collect();
    let single_key_impl = (primary_key_markers.len() == 1).then(|| {
        let marker = primary_key_markers[0];
        quote! {
            #[automatically_derived]
            impl #cr::SingleKeyEntity for #entity_ident {
                type PrimaryKeyColumn = #module_ident::#marker;
            }
        }
    });

    // The key type: a single column's bare Rust value, or the tuple of the
    // key columns' values in declaration order. Types resolve through the
    // marker-free alias module like every other field type.
    let key_field_indices: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, column)| column.attrs.primary_key)
        .map(|(index, _)| index)
        .collect();
    let keyed_impl = (!key_field_indices.is_empty()).then(|| {
        let aliases: Vec<_> = key_field_indices
            .iter()
            .map(|index| format_ident!("R{index}"))
            .collect();
        let (key_type, bindings, values): (TokenStream, TokenStream, TokenStream) =
            if let [alias] = aliases.as_slice() {
                (
                    quote!(#module_ident::__jet_fields::#alias),
                    quote!(let value = key;),
                    quote!(::std::vec![
                        <#module_ident::__jet_fields::#alias as #cr::SqlValue>::into_value(value)
                    ]),
                )
            } else {
                let names: Vec<_> = (0..aliases.len())
                    .map(|position| format_ident!("value{position}"))
                    .collect();
                (
                    quote!((#(#module_ident::__jet_fields::#aliases,)*)),
                    quote!(let (#(#names,)*) = key;),
                    quote!(::std::vec![#(
                        <#module_ident::__jet_fields::#aliases as #cr::SqlValue>::into_value(#names)
                    ),*]),
                )
            };
        quote! {
            #[automatically_derived]
            impl #cr::KeyedEntity for #entity_ident {
                type Key = #key_type;

                fn key_values(key: Self::Key) -> ::std::vec::Vec<#cr::Value> {
                    #bindings
                    #values
                }
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
            const FOREIGN_KEYS: &'static [#cr::ForeignKeyRef] = &[#(#foreign_key_refs),*];
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

            fn value(&self, column: usize) -> ::core::option::Option<#cr::Value> {
                match column {
                    #(#value_arms,)*
                    _ => ::core::option::Option::None,
                }
            }
        }

        #single_key_impl

        #keyed_impl

        #[doc = #module_doc]
        #vis mod #module_ident {
            #[allow(unused_imports)]
            use super::*;

            #[doc(hidden)]
            pub mod __jet_fields {
                #[allow(unused_imports)]
                use super::super::*;

                #(#field_aliases)*

                #(#relation_target_aliases)*
            }

            #(#column_markers)*

            #(#relation_markers)*
        }
    })
}

pub(crate) fn snake_case(input: &str) -> String {
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

pub(crate) fn pascal_case(input: &str) -> String {
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

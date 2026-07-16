use proc_macro2::Ident;
use quote::format_ident;
use syn::{GenericArgument, PathArguments, Type};

/// Column typing facts resolved from one struct field type.
pub struct ColumnSpec {
    /// `ColumnType` variant name for the metadata constant.
    pub column_type: Ident,
    /// Field type with one outer `Option` stripped; the marker's `Rust` type.
    pub inner: Type,
    /// Whether the field was wrapped in `Option`.
    pub nullable: bool,
}

/// Resolves a struct field type to its dialect-independent column type.
///
/// Resolution is syntactic: the last path segment selects the SQL kind, so
/// renamed imports of supported types are not recognized. One outer `Option`
/// marks the column nullable.
pub fn resolve(ty: &Type) -> syn::Result<ColumnSpec> {
    if let Some(inner) = option_argument(ty) {
        let spec = resolve_inner(inner)?;
        return Ok(ColumnSpec {
            column_type: spec.column_type,
            inner: inner.clone(),
            nullable: true,
        });
    }
    resolve_inner(ty)
}

fn resolve_inner(ty: &Type) -> syn::Result<ColumnSpec> {
    let unsupported = || {
        syn::Error::new_spanned(
            ty,
            "unsupported column type; expected bool, i16, i32, i64, f32, f64, String, Vec<u8>, \
             Uuid, NaiveDate, NaiveTime, NaiveDateTime, DateTime<Utc>, or serde_json::Value \
             (optionally wrapped in Option)",
        )
    };
    let Type::Path(path) = ty else {
        return Err(unsupported());
    };
    let Some(segment) = path.path.segments.last() else {
        return Err(unsupported());
    };

    let column_type = match segment.ident.to_string().as_str() {
        "bool" => "Boolean",
        "i16" => "Int16",
        "i32" => "Int32",
        "i64" => "Int64",
        "f32" => "Float32",
        "f64" => "Float64",
        "String" => "Text",
        "Vec" if first_argument_is(segment, "u8") => "Bytes",
        "NaiveDate" => "Date",
        "NaiveTime" => "Time",
        "NaiveDateTime" => "Timestamp",
        "DateTime" if first_argument_is(segment, "Utc") => "TimestampUtc",
        "Uuid" => "Uuid",
        "Value" | "JsonValue" => "Json",
        _ => return Err(unsupported()),
    };

    Ok(ColumnSpec {
        column_type: format_ident!("{column_type}"),
        inner: ty.clone(),
        nullable: false,
    })
}

fn option_argument(ty: &Type) -> Option<&Type> {
    let Type::Path(path) = ty else {
        return None;
    };
    let segment = path.path.segments.last()?;
    if segment.ident != "Option" {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    match arguments.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

fn first_argument_is(segment: &syn::PathSegment, ident: &str) -> bool {
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return false;
    };
    let Some(GenericArgument::Type(Type::Path(path))) = arguments.args.first() else {
        return false;
    };
    path.path
        .segments
        .last()
        .is_some_and(|last| last.ident == ident)
}

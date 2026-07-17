use syn::{GenericArgument, PathArguments, Type};

/// Column typing facts resolved from one struct field type.
pub struct ColumnSpec {
    /// Field type with one outer `Option` stripped; the marker's `Rust` type.
    pub inner: Type,
    /// Whether the field was wrapped in `Option`.
    pub nullable: bool,
}

/// Splits a field type into its stored type and its nullability.
///
/// Only `Option` is inspected syntactically — it decides nullability and the
/// column marker's `Rust` type, which no trait can express. Everything else
/// about the type is resolved through the `SqlValue` trait in generated
/// code, so type aliases, renamed imports, and user-defined types all work:
/// implementing `SqlValue` for a type is what makes it a column type.
pub fn resolve(ty: &Type) -> ColumnSpec {
    match option_argument(ty) {
        Some(inner) => ColumnSpec {
            inner: inner.clone(),
            nullable: true,
        },
        None => ColumnSpec {
            inner: ty.clone(),
            nullable: false,
        },
    }
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

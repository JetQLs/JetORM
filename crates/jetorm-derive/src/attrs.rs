use syn::{Attribute, DeriveInput, Field, LitStr, Path};

/// Container-level configuration parsed from `#[jet(...)]` attributes.
pub struct ContainerAttrs {
    pub table: String,
    pub schema: Option<String>,
    pub module: Option<String>,
    pub crate_path: Path,
}

/// Field-level configuration parsed from `#[jet(...)]` attributes.
#[derive(Default)]
pub struct FieldAttrs {
    pub column: Option<String>,
    pub column_type: Option<syn::Ident>,
    pub primary_key: bool,
    pub auto_increment: bool,
    pub unique: bool,
    pub references: Option<Path>,
    pub relation: Option<String>,
    pub on_delete: Option<syn::Ident>,
    pub on_update: Option<syn::Ident>,
}

pub fn parse_container(input: &DeriveInput) -> syn::Result<ContainerAttrs> {
    let mut table = None;
    let mut schema = None;
    let mut module = None;
    let mut crate_path = None;

    for attribute in jet_attributes(&input.attrs) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else if meta.path.is_ident("schema") {
                schema = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else if meta.path.is_ident("module") {
                module = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else if meta.path.is_ident("crate_path") {
                let literal = meta.value()?.parse::<LitStr>()?;
                crate_path = Some(literal.parse::<Path>()?);
                Ok(())
            } else {
                Err(meta.error("unknown container attribute; expected `table`, `schema`, `module`, or `crate_path`"))
            }
        })?;
    }

    let Some(table) = table else {
        return Err(syn::Error::new_spanned(
            input,
            "`#[derive(JetModel)]` requires `#[jet(table = \"...\")]`",
        ));
    };
    let crate_path = match crate_path {
        Some(path) => path,
        None => syn::parse_str::<Path>("::jetorm")?,
    };

    Ok(ContainerAttrs {
        table,
        schema,
        module,
        crate_path,
    })
}

pub fn parse_field(field: &Field) -> syn::Result<FieldAttrs> {
    let mut attrs = FieldAttrs::default();

    for attribute in jet_attributes(&field.attrs) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("column") {
                attrs.column = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else if meta.path.is_ident("column_type") {
                let literal = meta.value()?.parse::<LitStr>()?;
                attrs.column_type = Some(literal.parse::<syn::Ident>()?);
                Ok(())
            } else if meta.path.is_ident("references") {
                let literal = meta.value()?.parse::<LitStr>()?;
                attrs.references = Some(literal.parse::<Path>()?);
                Ok(())
            } else if meta.path.is_ident("relation") {
                attrs.relation = Some(meta.value()?.parse::<LitStr>()?.value());
                Ok(())
            } else if meta.path.is_ident("on_delete") {
                attrs.on_delete = Some(parse_referential_action(&meta)?);
                Ok(())
            } else if meta.path.is_ident("on_update") {
                attrs.on_update = Some(parse_referential_action(&meta)?);
                Ok(())
            } else if meta.path.is_ident("primary_key") {
                attrs.primary_key = true;
                Ok(())
            } else if meta.path.is_ident("auto_increment") {
                attrs.auto_increment = true;
                Ok(())
            } else if meta.path.is_ident("unique") {
                attrs.unique = true;
                Ok(())
            } else {
                Err(meta.error(
                    "unknown field attribute; expected `column`, `column_type`, `primary_key`,                      `auto_increment`, `unique`, `references`, `relation`, `on_delete`, or                      `on_update`",
                ))
            }
        })?;
    }

    if attrs.references.is_none()
        && (attrs.relation.is_some() || attrs.on_delete.is_some() || attrs.on_update.is_some())
    {
        return Err(syn::Error::new_spanned(
            field,
            "`relation`, `on_delete`, and `on_update` require `references` on the same field",
        ));
    }

    Ok(attrs)
}

/// Parses one referential action name into its `ReferentialAction` variant.
fn parse_referential_action(meta: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<syn::Ident> {
    let literal = meta.value()?.parse::<LitStr>()?;
    let variant = match literal.value().as_str() {
        "no_action" => "NoAction",
        "restrict" => "Restrict",
        "cascade" => "Cascade",
        "set_null" => "SetNull",
        "set_default" => "SetDefault",
        other => {
            return Err(syn::Error::new_spanned(
                literal,
                format!(
                    "unknown referential action {other:?}; expected `no_action`, `restrict`,                      `cascade`, `set_null`, or `set_default`"
                ),
            ));
        }
    };
    Ok(syn::Ident::new(variant, literal.span()))
}

pub(crate) fn jet_attributes(attrs: &[Attribute]) -> impl Iterator<Item = &Attribute> {
    attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("jet"))
}

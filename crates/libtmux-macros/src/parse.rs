use proc_macro2::Span;
use syn::ext::IdentExt as _;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, Field, Fields, GenericArgument, Ident, Lit,
    LitStr, Meta, MetaList, MetaNameValue, Path, PathArguments, Token, Type, parenthesized,
};

use crate::model::{ContainerOptions, Errors, FieldKind, FieldSpec};

#[derive(Clone, Copy, Eq, PartialEq)]
enum FieldMode {
    Scalar,
    Skip,
    Enum,
    Many,
    One,
}

#[derive(Default)]
struct FieldOptions {
    rename: Option<LitStr>,
    rename_seen: bool,
    modes: Vec<(FieldMode, Span)>,
    invalid_metadata: bool,
}

pub(super) fn named_fields<'a>(
    input: &'a DeriveInput,
    errors: &mut Errors,
) -> Option<&'a Punctuated<Field, Token![,]>> {
    match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(fields) => Some(&fields.named),
            Fields::Unnamed(fields) => {
                errors.push(syn::Error::new(
                    fields.span(),
                    "Filterable can only be derived for a struct with named fields",
                ));
                None
            }
            Fields::Unit => {
                errors.push(syn::Error::new(
                    input.ident.span(),
                    "Filterable can only be derived for a struct with named fields",
                ));
                None
            }
        },
        Data::Enum(data) => {
            errors.push(syn::Error::new(
                data.enum_token.span,
                "Filterable can only be derived for a struct with named fields",
            ));
            None
        }
        Data::Union(data) => {
            errors.push(syn::Error::new(
                data.union_token.span,
                "Filterable can only be derived for a struct with named fields",
            ));
            None
        }
    }
}

pub(super) fn parse_container_options(
    attrs: &[Attribute],
    errors: &mut Errors,
) -> ContainerOptions {
    let mut options = ContainerOptions::default();
    for_each_filterable_meta(attrs, errors, |meta, errors| {
        parse_container_meta(meta, &mut options, errors);
    });
    options
}

fn parse_container_meta(meta: Meta, options: &mut ContainerOptions, errors: &mut Errors) {
    let Some(key) = meta.path().get_ident().map(Ident::to_string) else {
        errors.push(syn::Error::new(
            meta.span(),
            "unknown filterable container option",
        ));
        return;
    };

    match key.as_str() {
        "target" => parse_target(meta, options, errors),
        "fields" => parse_fields_name(meta, options, errors),
        "crate" => parse_crate_path(meta, options, errors),
        "rename" | "skip" | "enum" | "many" | "one" => errors.push(syn::Error::new(
            meta.span(),
            format!("filterable `{key}` is only valid on fields"),
        )),
        _ => errors.push(syn::Error::new(
            meta.span(),
            format!("unknown filterable container option `{key}`"),
        )),
    }
}

fn parse_target(meta: Meta, options: &mut ContainerOptions, errors: &mut Errors) {
    if options.target_seen {
        errors.push(syn::Error::new(
            meta.span(),
            "duplicate filterable container option `target`",
        ));
        return;
    }
    options.target_seen = true;
    if let Some(value) = name_value_string(meta, "target", errors) {
        options.target = Some(value);
    }
}

fn parse_fields_name(meta: Meta, options: &mut ContainerOptions, errors: &mut Errors) {
    if options.fields_seen {
        errors.push(syn::Error::new(
            meta.span(),
            "duplicate filterable container option `fields`",
        ));
        return;
    }
    options.fields_seen = true;
    let Some(value) = name_value_string(meta, "fields", errors) else {
        return;
    };
    match syn::parse_str::<Ident>(&value.value()) {
        Ok(mut ident) => {
            ident.set_span(value.span());
            options.fields = Some(ident);
        }
        Err(_) => errors.push(syn::Error::new(
            value.span(),
            "filterable `fields` must contain one Rust identifier",
        )),
    }
}

fn parse_crate_path(meta: Meta, options: &mut ContainerOptions, errors: &mut Errors) {
    if options.crate_seen {
        errors.push(syn::Error::new(
            meta.span(),
            "duplicate filterable container option `crate`",
        ));
        return;
    }
    options.crate_seen = true;
    let Some(value) = name_value_string(meta, "crate", errors) else {
        return;
    };
    match syn::parse_str::<Path>(&value.value()) {
        Ok(path) => options.crate_path = Some(path),
        Err(_) => errors.push(syn::Error::new(
            value.span(),
            "filterable `crate` must contain a Rust path",
        )),
    }
}

pub(super) fn parse_field(field: &Field, errors: &mut Errors) -> Option<FieldSpec> {
    let Some(ident) = field.ident.clone() else {
        errors.push(syn::Error::new(
            field.span(),
            "Filterable fields must have identifiers",
        ));
        return None;
    };
    let mut options = FieldOptions::default();
    for_each_filterable_meta(&field.attrs, errors, |meta, errors| {
        parse_field_meta(meta, &mut options, errors);
    });

    let mode = resolve_field_mode(&options, errors);
    if options.invalid_metadata {
        return None;
    }
    let mode = mode?;
    if mode == FieldMode::Skip {
        if options.rename.is_some() {
            let span = options
                .modes
                .iter()
                .find_map(|(mode, span)| (*mode == FieldMode::Skip).then_some(*span))
                .unwrap_or_else(|| ident.span());
            errors.push(syn::Error::new(
                span,
                "filterable `rename` cannot be combined with `skip`",
            ));
        }
        return None;
    }

    let wire_name = options.rename.unwrap_or_else(|| {
        let name = ident.unraw().to_string();
        LitStr::new(&name, ident.span())
    });
    validate_wire_name(&wire_name, "filter field name", errors);

    let kind = match mode {
        FieldMode::Scalar => infer_scalar(&field.ty),
        FieldMode::Enum => explicit_enum(&field.ty),
        FieldMode::Many => {
            explicit_relation(&field.ty, "Vec", |related| FieldKind::Many { related })
        }
        FieldMode::One => {
            explicit_relation(&field.ty, "Option", |related| FieldKind::One { related })
        }
        FieldMode::Skip => return None,
    };
    match kind {
        Ok(kind) => Some(FieldSpec {
            ident,
            wire_name,
            kind,
        }),
        Err(message) => {
            errors.push(syn::Error::new(field.ty.span(), message));
            None
        }
    }
}

fn parse_field_meta(meta: Meta, options: &mut FieldOptions, errors: &mut Errors) {
    let Some(key) = meta.path().get_ident().map(Ident::to_string) else {
        options.invalid_metadata = true;
        errors.push(syn::Error::new(
            meta.span(),
            "unknown filterable field option",
        ));
        return;
    };
    match key.as_str() {
        "rename" => parse_rename(meta, options, errors),
        "skip" => parse_field_flag(&meta, options, FieldMode::Skip, "skip", errors),
        "enum" => parse_field_flag(&meta, options, FieldMode::Enum, "enum", errors),
        "many" => parse_field_flag(&meta, options, FieldMode::Many, "many", errors),
        "one" => parse_field_flag(&meta, options, FieldMode::One, "one", errors),
        "target" | "fields" | "crate" => {
            options.invalid_metadata = true;
            errors.push(syn::Error::new(
                meta.span(),
                format!("filterable `{key}` is only valid on the container"),
            ));
        }
        _ => {
            options.invalid_metadata = true;
            errors.push(syn::Error::new(
                meta.span(),
                format!("unknown filterable field option `{key}`"),
            ));
        }
    }
}

fn parse_rename(meta: Meta, options: &mut FieldOptions, errors: &mut Errors) {
    if options.rename_seen {
        options.invalid_metadata = true;
        errors.push(syn::Error::new(
            meta.span(),
            "duplicate filterable field option `rename`",
        ));
        return;
    }
    options.rename_seen = true;
    if let Some(value) = name_value_string(meta, "rename", errors) {
        options.rename = Some(value);
    } else {
        options.invalid_metadata = true;
    }
}

fn parse_field_flag(
    meta: &Meta,
    options: &mut FieldOptions,
    mode: FieldMode,
    name: &str,
    errors: &mut Errors,
) {
    if !matches!(meta, Meta::Path(_)) {
        options.invalid_metadata = true;
        errors.push(syn::Error::new(
            meta.span(),
            format!("filterable `{name}` is a bare flag and does not accept a value"),
        ));
        return;
    }
    if options.modes.iter().any(|(existing, _)| *existing == mode) {
        options.invalid_metadata = true;
        errors.push(syn::Error::new(
            meta.span(),
            format!("duplicate filterable field option `{name}`"),
        ));
        return;
    }
    options.modes.push((mode, meta.span()));
}

fn resolve_field_mode(options: &FieldOptions, errors: &mut Errors) -> Option<FieldMode> {
    let Some((first, _)) = options.modes.first().copied() else {
        return Some(FieldMode::Scalar);
    };
    for (mode, span) in options.modes.iter().copied().skip(1) {
        errors.push(syn::Error::new(
            span,
            format!(
                "filterable `{}` conflicts with `{}`",
                field_mode_name(mode),
                field_mode_name(first)
            ),
        ));
    }
    (options.modes.len() == 1).then_some(first)
}

const fn field_mode_name(mode: FieldMode) -> &'static str {
    match mode {
        FieldMode::Scalar => "scalar",
        FieldMode::Skip => "skip",
        FieldMode::Enum => "enum",
        FieldMode::Many => "many",
        FieldMode::One => "one",
    }
}

fn for_each_filterable_meta(
    attrs: &[Attribute],
    errors: &mut Errors,
    mut visit: impl FnMut(Meta, &mut Errors),
) {
    for attr in attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("filterable"))
    {
        let Meta::List(list) = &attr.meta else {
            errors.push(syn::Error::new(
                attr.span(),
                "filterable attributes require parenthesized options",
            ));
            continue;
        };
        if list.tokens.is_empty() {
            errors.push(syn::Error::new(
                attr.span(),
                "filterable attributes require at least one option",
            ));
            continue;
        }
        if let Err(error) = attr.parse_nested_meta(|meta| {
            let item = if meta.input.peek(Token![=]) {
                Meta::NameValue(MetaNameValue {
                    path: meta.path,
                    eq_token: meta.input.parse()?,
                    value: meta.input.parse()?,
                })
            } else if meta.input.peek(syn::token::Paren) {
                let content;
                let paren = parenthesized!(content in meta.input);
                Meta::List(MetaList {
                    path: meta.path,
                    delimiter: syn::MacroDelimiter::Paren(paren),
                    tokens: content.parse()?,
                })
            } else {
                Meta::Path(meta.path)
            };
            visit(item, errors);
            Ok(())
        }) {
            errors.push(error);
        }
    }
}

fn name_value_string(meta: Meta, name: &str, errors: &mut Errors) -> Option<LitStr> {
    let Meta::NameValue(MetaNameValue { value, .. }) = meta else {
        errors.push(syn::Error::new(
            meta.span(),
            format!("filterable `{name}` requires a string value"),
        ));
        return None;
    };
    let Expr::Lit(ExprLit {
        lit: Lit::Str(value),
        ..
    }) = value
    else {
        errors.push(syn::Error::new(
            value.span(),
            format!("filterable `{name}` requires a string value"),
        ));
        return None;
    };
    Some(value)
}

pub(super) fn validate_wire_name(name: &LitStr, description: &str, errors: &mut Errors) {
    let value = name.value();
    let mut bytes = value.bytes();
    let valid = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if !valid {
        errors.push(syn::Error::new(
            name.span(),
            format!("{description} must match ASCII `[a-z][a-z0-9_]*`"),
        ));
    }
}

fn infer_scalar(ty: &Type) -> Result<FieldKind, &'static str> {
    if let Some(inner) = outer_type(ty, "Option") {
        return infer_non_optional_scalar(inner, true);
    }
    infer_non_optional_scalar(ty, false)
}

fn infer_non_optional_scalar(ty: &Type, optional: bool) -> Result<FieldKind, &'static str> {
    let Some(name) = simple_type_name(ty) else {
        return Err(unsupported_type_message());
    };
    match name.as_str() {
        "String" | "TmuxText" => Ok(FieldKind::Text { optional }),
        "bool" => Ok(FieldKind::Bool { optional }),
        "i8" | "i16" | "i32" | "i64" | "i128" => Ok(FieldKind::SignedInteger {
            ty: ty.clone(),
            optional,
            kind: Ident::new(&name.to_ascii_uppercase(), ty.span()),
        }),
        "u8" | "u16" | "u32" | "u64" | "u128" => Ok(FieldKind::UnsignedInteger {
            ty: ty.clone(),
            optional,
            kind: Ident::new(&name.to_ascii_uppercase(), ty.span()),
        }),
        _ => Err(unsupported_type_message()),
    }
}

const fn unsupported_type_message() -> &'static str {
    "unsupported filter field type; use String, TmuxText, bool, a fixed-width integer, an outer Option of one of those types, or an explicit enum/many/one annotation"
}

fn explicit_enum(ty: &Type) -> Result<FieldKind, &'static str> {
    if let Some(inner) = outer_type(ty, "Option") {
        if is_plain_type_path(inner) {
            return Ok(FieldKind::Enum {
                ty: inner.clone(),
                optional: true,
            });
        }
    } else if is_plain_type_path(ty) {
        return Ok(FieldKind::Enum {
            ty: ty.clone(),
            optional: false,
        });
    }
    Err("filterable `enum` requires a plain type path or `Option<EnumType>`")
}

fn explicit_relation(
    ty: &Type,
    wrapper: &str,
    make: impl FnOnce(Type) -> FieldKind,
) -> Result<FieldKind, &'static str> {
    outer_type(ty, wrapper)
        .map(|inner| make(inner.clone()))
        .ok_or({
            if wrapper == "Vec" {
                "filterable `many` requires exactly `Vec<Related>`"
            } else {
                "filterable `one` requires exactly `Option<Related>`"
            }
        })
}

fn outer_type<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    if segment.ident != wrapper {
        return None;
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    if arguments.args.len() != 1 {
        return None;
    }
    match arguments.args.first()? {
        GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}

fn simple_type_name(ty: &Type) -> Option<String> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    matches!(segment.arguments, PathArguments::None).then(|| segment.ident.to_string())
}

fn is_plain_type_path(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path.qself.is_none()
        && type_path
            .path
            .segments
            .last()
            .is_some_and(|segment| matches!(segment.arguments, PathArguments::None))
}

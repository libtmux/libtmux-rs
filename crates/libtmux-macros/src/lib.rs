//! Derive macros for libtmux filter expressions.
//!
//! Start from the [`libtmux` documentation][libtmux]: this crate is the
//! implementation of its `derive` feature, and a caller does not depend on it
//! directly.
//!
//! [libtmux]: https://docs.rs/libtmux

use std::collections::HashSet;

use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::ext::IdentExt as _;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, Field, Fields, GenericArgument, Ident, Lit,
    LitStr, Meta, MetaList, MetaNameValue, Path, PathArguments, Token, Type, parenthesized,
    parse_macro_input, parse_quote,
};

/// Derive a stable typed filter schema for a named struct.
///
/// The required `target` is the stable name embedded in portable expressions.
/// Every supported field produces one member of the generated `<Type>Fields`
/// companion. Bring `renamed_libtmux::query::Filterable` into scope to call
/// `filter_fields`.
///
/// Enable the core crate's `derive` feature and use its root `Filterable`
/// re-export. A deliberate direct dependency on `libtmux-macros` must exactly
/// match the `libtmux` version because the expansion uses a hidden core ABI.
///
/// # Examples
///
/// ```
/// use renamed_libtmux::Filterable;
/// use renamed_libtmux::query::{Filterable as _, QueryIteratorExt as _};
///
/// #[derive(Filterable)]
/// #[filterable(target = "task")]
/// struct Task {
///     name: String,
///     done: bool,
/// }
///
/// let values = vec![
///     Task { name: "build".into(), done: false },
///     Task { name: "test".into(), done: true },
/// ];
/// let fields = Task::filter_fields();
/// let expression = fields.name.contains("ui").and(fields.done.eq(false));
/// let selected = values.iter().matching(&expression).collect::<Vec<_>>();
/// assert_eq!(selected.len(), 1);
/// ```
#[proc_macro_derive(Filterable, attributes(filterable))]
pub fn derive_filterable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_filterable(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

#[derive(Default)]
struct Errors {
    error: Option<syn::Error>,
}

impl Errors {
    fn push(&mut self, error: syn::Error) {
        if let Some(existing) = &mut self.error {
            existing.combine(error);
        } else {
            self.error = Some(error);
        }
    }

    fn finish(self) -> syn::Result<()> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[derive(Default)]
struct ContainerOptions {
    target: Option<LitStr>,
    target_seen: bool,
    fields: Option<Ident>,
    fields_seen: bool,
    crate_path: Option<Path>,
    crate_seen: bool,
}

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

enum FieldKind {
    Text {
        optional: bool,
    },
    Bool {
        optional: bool,
    },
    SignedInteger {
        ty: Type,
        optional: bool,
        kind: Ident,
    },
    UnsignedInteger {
        ty: Type,
        optional: bool,
        kind: Ident,
    },
    Enum {
        ty: Type,
        optional: bool,
    },
    Many {
        related: Type,
    },
    One {
        related: Type,
    },
}

struct FieldSpec {
    ident: Ident,
    wire_name: LitStr,
    kind: FieldKind,
}

fn expand_filterable(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let mut errors = Errors::default();
    let container = parse_container_options(&input.attrs, &mut errors);
    let named_fields = named_fields(input, &mut errors);

    if !container.target_seen {
        errors.push(syn::Error::new(
            input.ident.span(),
            "missing required `#[filterable(target = \"stable_name\")]`",
        ));
    }
    if let Some(target) = &container.target {
        validate_wire_name(target, "filter target", &mut errors);
    }

    let mut field_specs = Vec::new();
    let mut wire_names = HashSet::new();
    if let Some(fields) = named_fields {
        for field in fields {
            if let Some(spec) = parse_field(field, &mut errors) {
                let wire_name = spec.wire_name.value();
                if !wire_names.insert(wire_name) {
                    errors.push(syn::Error::new(
                        spec.wire_name.span(),
                        "duplicate stable filter field name",
                    ));
                }
                field_specs.push(spec);
            }
        }
    }

    errors.finish()?;

    let Some(target) = container.target else {
        return Err(syn::Error::new(
            input.ident.span(),
            "missing required filter target after validation",
        ));
    };
    let fields_ident = container.fields.unwrap_or_else(|| {
        Ident::new(
            &format!("{}Fields", input.ident.unraw()),
            input.ident.span(),
        )
    });
    let core_path = resolve_core_path(container.crate_path, input.ident.span())?;

    Ok(generate(
        input,
        &fields_ident,
        &target,
        &core_path,
        &field_specs,
    ))
}

fn named_fields<'a>(
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

fn parse_container_options(attrs: &[Attribute], errors: &mut Errors) -> ContainerOptions {
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

fn parse_field(field: &Field, errors: &mut Errors) -> Option<FieldSpec> {
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

fn validate_wire_name(name: &LitStr, description: &str, errors: &mut Errors) {
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

fn resolve_core_path(override_path: Option<Path>, span: Span) -> syn::Result<TokenStream2> {
    if let Some(path) = override_path {
        return Ok(quote!(#path));
    }
    match crate_name("libtmux") {
        Ok(FoundCrate::Itself) => Ok(quote!(crate)),
        Ok(FoundCrate::Name(name)) => {
            let ident = Ident::new(&name.replace('-', "_"), Span::call_site());
            Ok(quote!(::#ident))
        }
        Err(_) => Err(syn::Error::new(
            span,
            "could not locate the `libtmux` package; use `#[filterable(crate = \"path::to::libtmux\")]` to provide it explicitly",
        )),
    }
}

// One expansion block keeps the companion and trait implementation on the
// same generic and where-clause inputs, preventing subtle bound drift.
#[allow(clippy::too_many_lines)]
fn generate(
    input: &DeriveInput,
    fields_ident: &Ident,
    target: &LitStr,
    core_path: &TokenStream2,
    fields: &[FieldSpec],
) -> TokenStream2 {
    let candidate = &input.ident;
    let visibility = &input.vis;
    let generics = &input.generics;
    let (fields_impl_generics, type_generics, fields_where_clause) = generics.split_for_impl();
    let candidate_type = quote!(#candidate #type_generics);
    let marker_ident = marker_ident(fields);

    let field_declarations = fields.iter().map(|field| {
        let ident = &field.ident;
        let ty = handle_type(&field.kind, &candidate_type, core_path);
        let doc = format!(
            "Typed filter handle for `{}::{}` with stable field name `{}`.",
            candidate,
            ident.unraw(),
            field.wire_name.value()
        );
        quote! {
            #[doc = #doc]
            pub #ident: #ty
        }
    });
    let field_initializers = fields.iter().map(|field| {
        let ident = &field.ident;
        let constructor = handle_constructor(&field.kind, core_path);
        let wire_name = &field.wire_name;
        quote!(
            #ident: #constructor(
                <Self as #core_path::query::Filterable>::FILTER_TARGET,
                #wire_name,
            )
        )
    });
    let equality = fields.iter().map(|field| {
        let ident = &field.ident;
        quote!(&& self.#ident == other.#ident)
    });
    let debug_fields = fields.iter().map(|field| {
        let ident = &field.ident;
        let wire_name = &field.wire_name;
        quote!(debug.field(#wire_name, &self.#ident);)
    });
    let match_arms = fields.iter().map(|field| match_arm(field, core_path));
    let validation_arms = fields.iter().map(|field| validation_arm(field, core_path));

    let mut filter_impl_generics = generics.clone();
    add_filter_bounds(&mut filter_impl_generics, fields, core_path);
    let (filter_impl_generics, _, filter_where_clause) = filter_impl_generics.split_for_impl();
    let companion_doc = format!("Typed filter handles generated for [`{candidate}`].");
    let companion = quote_spanned! {candidate.span()=>
        #[doc = #companion_doc]
        #visibility struct #fields_ident #generics #fields_where_clause {
            #(#field_declarations,)*
            #marker_ident: ::core::marker::PhantomData<fn() -> #candidate_type>,
        }
    };

    quote! {
        #companion

        #[automatically_derived]
        impl #fields_impl_generics ::core::marker::Copy
            for #fields_ident #type_generics #fields_where_clause
        {}

        #[automatically_derived]
        impl #fields_impl_generics ::core::clone::Clone
            for #fields_ident #type_generics #fields_where_clause
        {
            fn clone(&self) -> Self {
                *self
            }
        }

        #[automatically_derived]
        impl #fields_impl_generics ::core::cmp::PartialEq
            for #fields_ident #type_generics #fields_where_clause
        {
            fn eq(&self, other: &Self) -> bool {
                true #(#equality)*
            }
        }

        #[automatically_derived]
        impl #fields_impl_generics ::core::cmp::Eq
            for #fields_ident #type_generics #fields_where_clause
        {}

        #[automatically_derived]
        impl #fields_impl_generics ::core::fmt::Debug
            for #fields_ident #type_generics #fields_where_clause
        {
            fn fmt(&self, formatter: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                let mut debug = formatter.debug_struct(stringify!(#fields_ident));
                #(#debug_fields)*
                debug.finish()
            }
        }

        #[automatically_derived]
        impl #filter_impl_generics #core_path::query::Filterable
            for #candidate #type_generics #filter_where_clause
        {
            type Fields = #fields_ident #type_generics;

            const FILTER_TARGET: &'static str = #target;

            fn filter_fields() -> Self::Fields {
                #fields_ident {
                    #(#field_initializers,)*
                    #marker_ident: ::core::marker::PhantomData,
                }
            }

            fn __filter_matches(
                &self,
                predicate: &#core_path::query::__private::Predicate,
            ) -> bool {
                match predicate.field() {
                    #(#match_arms,)*
                    _ => false,
                }
            }

            fn __filter_validate(
                predicate: &#core_path::query::__private::Predicate,
            ) -> ::core::result::Result<(), #core_path::query::FilterExpressionError> {
                match predicate.field() {
                    #(#validation_arms,)*
                    _ => ::core::result::Result::Err(
                        #core_path::query::__private::unknown_field_error(),
                    ),
                }
            }
        }
    }
}

fn marker_ident(fields: &[FieldSpec]) -> Ident {
    let names = fields
        .iter()
        .map(|field| field.ident.unraw().to_string())
        .collect::<HashSet<_>>();
    let mut name = String::from("__libtmux_filterable_marker");
    while names.contains(&name) {
        name.push('_');
    }
    Ident::new(&name, Span::call_site())
}

fn handle_type(kind: &FieldKind, candidate: &TokenStream2, core: &TokenStream2) -> TokenStream2 {
    match kind {
        FieldKind::Text { .. } => quote!(#core::query::TextField<#candidate>),
        FieldKind::Bool { .. } => quote!(#core::query::BoolField<#candidate>),
        FieldKind::SignedInteger { ty, .. } | FieldKind::UnsignedInteger { ty, .. } => {
            quote!(#core::query::IntegerField<#candidate, #ty>)
        }
        FieldKind::Enum { ty, .. } => quote!(#core::query::EnumField<#candidate, #ty>),
        FieldKind::Many { related } => quote!(#core::query::ManyRelation<#candidate, #related>),
        FieldKind::One { related } => quote!(#core::query::OneRelation<#candidate, #related>),
    }
}

fn handle_constructor(kind: &FieldKind, core: &TokenStream2) -> TokenStream2 {
    match kind {
        FieldKind::Text { .. } => quote!(#core::query::__private::text_field),
        FieldKind::Bool { .. } => quote!(#core::query::__private::bool_field),
        FieldKind::SignedInteger { .. } | FieldKind::UnsignedInteger { .. } => {
            quote!(#core::query::__private::integer_field)
        }
        FieldKind::Enum { .. } => quote!(#core::query::__private::enum_field),
        FieldKind::Many { .. } => quote!(#core::query::__private::many_relation),
        FieldKind::One { .. } => quote!(#core::query::__private::one_relation),
    }
}

fn match_arm(field: &FieldSpec, core: &TokenStream2) -> TokenStream2 {
    let ident = &field.ident;
    let wire = &field.wire_name;
    let body = match &field.kind {
        FieldKind::Text { optional: false } => {
            quote!(predicate.matches_text(self.#ident.as_bytes()))
        }
        FieldKind::Text { optional: true } => quote!(self.#ident.as_ref().is_some_and(
            |value| predicate.matches_text(value.as_bytes())
        )),
        FieldKind::Bool { optional: false } => quote!(predicate.matches_bool(self.#ident)),
        FieldKind::Bool { optional: true } => quote!(self
            .#ident
            .as_ref()
            .is_some_and(|value| predicate.matches_bool(*value))),
        FieldKind::SignedInteger {
            ty,
            optional: false,
            ..
        } => quote!(predicate.matches_signed(
            <::core::primitive::i128 as ::core::convert::From<#ty>>::from(self.#ident)
        )),
        FieldKind::SignedInteger {
            ty, optional: true, ..
        } => quote!(self.#ident.as_ref().is_some_and(|value| predicate.matches_signed(
            <::core::primitive::i128 as ::core::convert::From<#ty>>::from(*value)
        ))),
        FieldKind::UnsignedInteger {
            ty,
            optional: false,
            ..
        } => quote!(predicate.matches_unsigned(
            <::core::primitive::u128 as ::core::convert::From<#ty>>::from(self.#ident)
        )),
        FieldKind::UnsignedInteger {
            ty, optional: true, ..
        } => quote!(self.#ident.as_ref().is_some_and(|value| predicate.matches_unsigned(
            <::core::primitive::u128 as ::core::convert::From<#ty>>::from(*value)
        ))),
        FieldKind::Enum {
            optional: false, ..
        } => quote!(predicate.matches_enum(
            #core::query::FilterEnum::filter_name(&self.#ident)
        )),
        FieldKind::Enum { optional: true, .. } => quote!(self.#ident.as_ref().is_some_and(
            |value| predicate.matches_enum(#core::query::FilterEnum::filter_name(value))
        )),
        FieldKind::Many { .. } => quote!(predicate.matches_many(&self.#ident)),
        FieldKind::One { .. } => quote!(predicate.matches_one(self.#ident.as_ref())),
    };
    quote!(#wire => #body)
}

fn validation_arm(field: &FieldSpec, core: &TokenStream2) -> TokenStream2 {
    let wire = &field.wire_name;
    let body = match &field.kind {
        FieldKind::Text { .. } => quote!(predicate.validate_text()),
        FieldKind::Bool { .. } => quote!(predicate.validate_bool()),
        FieldKind::SignedInteger { kind, .. } | FieldKind::UnsignedInteger { kind, .. } => {
            quote!(predicate.validate_integer(#core::query::__private::IntegerKind::#kind))
        }
        FieldKind::Enum { ty, .. } => quote!(predicate.validate_enum(
            <#ty as #core::query::FilterEnum>::FILTER_VARIANTS,
        )),
        FieldKind::Many { related } => quote!(predicate.validate_many::<#related>()),
        FieldKind::One { related } => quote!(predicate.validate_one::<#related>()),
    };
    quote!(#wire => #body)
}

fn add_filter_bounds(generics: &mut syn::Generics, fields: &[FieldSpec], core: &TokenStream2) {
    let mut seen = HashSet::new();
    for field in fields {
        let (ty, bound) = match &field.kind {
            FieldKind::Enum { ty, .. } => (ty, quote!(#core::query::FilterEnum)),
            FieldKind::Many { related } | FieldKind::One { related } => {
                (related, quote!(#core::query::Filterable))
            }
            FieldKind::Text { .. }
            | FieldKind::Bool { .. }
            | FieldKind::SignedInteger { .. }
            | FieldKind::UnsignedInteger { .. } => continue,
        };
        let key = quote!(#ty: #bound).to_string();
        if seen.insert(key) {
            generics
                .make_where_clause()
                .predicates
                .push(parse_quote!(#ty: #bound));
        }
    }
}

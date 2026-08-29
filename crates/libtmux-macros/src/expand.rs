use std::collections::HashSet;

use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{quote, quote_spanned};
use syn::ext::IdentExt as _;
use syn::{DeriveInput, Ident, LitStr, Path, parse_quote};

use crate::model::{Errors, FieldKind, FieldSpec};
use crate::parse::{named_fields, parse_container_options, parse_field, validate_wire_name};

pub(super) fn expand_filterable(input: &DeriveInput) -> syn::Result<TokenStream2> {
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
    let schema_fields = fields.iter().map(|field| {
        let wire_name = &field.wire_name;
        let kind = schema_kind(&field.kind, core_path);
        quote!(#core_path::query::__private::FilterFieldSchema::new(#wire_name, #kind))
    });

    let mut filter_impl_generics = generics.clone();
    add_filter_bounds(&mut filter_impl_generics, fields, core_path);
    let (filter_impl_generics, _, filter_where_clause) = filter_impl_generics.split_for_impl();
    let mut schema_impl_generics = generics.clone();
    add_schema_bounds(&mut schema_impl_generics, fields, core_path);
    let (schema_impl_generics, _, schema_where_clause) = schema_impl_generics.split_for_impl();
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

        #[automatically_derived]
        impl #schema_impl_generics #core_path::query::FilterSchema
            for #candidate #type_generics #schema_where_clause
        {
            fn __filter_schema(
            ) -> #core_path::query::__private::FilterSchemaDescriptor {
                #core_path::query::__private::FilterSchemaDescriptor::new(
                    #target,
                    ::std::vec![#(#schema_fields),*],
                )
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

fn schema_kind(kind: &FieldKind, core: &TokenStream2) -> TokenStream2 {
    match kind {
        FieldKind::Text { .. } => quote!(#core::query::__private::FilterValueSchema::Text),
        FieldKind::Bool { .. } => quote!(#core::query::__private::FilterValueSchema::Bool),
        FieldKind::SignedInteger { .. } => {
            quote!(#core::query::__private::FilterValueSchema::Signed)
        }
        FieldKind::UnsignedInteger { .. } => {
            quote!(#core::query::__private::FilterValueSchema::Unsigned)
        }
        FieldKind::Enum { ty, .. } => quote!(
            #core::query::__private::FilterValueSchema::Enum(
                <#ty as #core::query::FilterEnum>::FILTER_VARIANTS,
            )
        ),
        FieldKind::Many { related } => quote!(
            #core::query::__private::FilterValueSchema::Many(
                #core::query::__private::filter_schema::<#related>,
            )
        ),
        FieldKind::One { related } => quote!(
            #core::query::__private::FilterValueSchema::One(
                #core::query::__private::filter_schema::<#related>,
            )
        ),
    }
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

fn add_schema_bounds(generics: &mut syn::Generics, fields: &[FieldSpec], core: &TokenStream2) {
    let mut seen = HashSet::new();
    for field in fields {
        let (ty, bound) = match &field.kind {
            FieldKind::Enum { ty, .. } => (ty, quote!(#core::query::FilterEnum)),
            FieldKind::Many { related } | FieldKind::One { related } => {
                (related, quote!(#core::query::FilterSchema))
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

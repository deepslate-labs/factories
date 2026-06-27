//! `#[derive(CaptureSchema)]` — generates a [`CaptureSchema`] impl from a
//! struct's fields and their `#[capture(...)]` attributes.
//!
//! Per field it emits `begin_struct`, then (optionally gated) a
//! `field(name, interpretation)` followed by a recurse into the field's value,
//! then `end_struct`. Because the generated body lives inside
//! `capture(&self, …, config)`, both `self` (for `interpret`) and `config` (for
//! `if` predicates) are in scope.
//!
//! Field attributes:
//! - `#[capture(skip)]` — omit the field.
//! - `#[capture(rename = "name")]` — emit under `name` instead of the ident.
//! - `#[capture(if = <expr>)]` — gate on `(expr)(config)` (combinator/closure/fn); absent ⇒ always captured.
//! - `#[capture(interpret = <expr>)]` — the field's `Interpretation`; `self` is in scope (read siblings).

use proc_macro::TokenStream;
use proc_macro_error::{abort, emit_error, proc_macro_error};
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Data, DeriveInput, Expr, Fields, LitStr, Token, parse_macro_input};

mod kw {
    syn::custom_keyword!(skip);
    syn::custom_keyword!(rename);
    syn::custom_keyword!(interpret);
}

/// Parsed contents of one `#[capture(...)]` attribute.
#[derive(Default)]
struct CaptureArgs {
    skip: bool,
    rename: Option<String>,
    when: Option<Expr>,
    interpret: Option<Expr>,
}

impl Parse for CaptureArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = CaptureArgs::default();
        while !input.is_empty() {
            if input.peek(kw::skip) {
                input.parse::<kw::skip>()?;
                args.skip = true;
            } else if input.peek(kw::rename) {
                input.parse::<kw::rename>()?;
                input.parse::<Token![=]>()?;
                args.rename = Some(input.parse::<LitStr>()?.value());
            } else if input.peek(Token![if]) {
                input.parse::<Token![if]>()?;
                input.parse::<Token![=]>()?;
                args.when = Some(input.parse()?);
            } else if input.peek(kw::interpret) {
                input.parse::<kw::interpret>()?;
                input.parse::<Token![=]>()?;
                args.interpret = Some(input.parse()?);
            } else {
                return Err(input.error("expected `skip`, `rename = \"…\"`, `if = …`, or `interpret = …`"));
            }
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            } else {
                break;
            }
        }
        Ok(args)
    }
}

#[proc_macro_derive(CaptureSchema, attributes(capture))]
#[proc_macro_error]
pub fn derive_capture_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let name_str = name.to_string();
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => abort!(input.ident, "CaptureSchema requires a struct with named fields"),
        },
        _ => abort!(input.ident, "CaptureSchema can only be derived for structs"),
    };

    let mut pushes = Vec::new();
    for field in fields {
        let ident = field.ident.as_ref().expect("named field has an ident");

        // Merge every `#[capture(...)]` attribute on this field.
        let mut args = CaptureArgs::default();
        for attr in &field.attrs {
            if !attr.path().is_ident("capture") {
                continue;
            }
            match attr.parse_args::<CaptureArgs>() {
                Ok(parsed) => {
                    args.skip |= parsed.skip;
                    if parsed.rename.is_some() {
                        args.rename = parsed.rename;
                    }
                    if parsed.when.is_some() {
                        args.when = parsed.when;
                    }
                    if parsed.interpret.is_some() {
                        args.interpret = parsed.interpret;
                    }
                }
                Err(error) => emit_error!(attr, error),
            }
        }

        if args.skip {
            continue;
        }

        let field_name = args.rename.unwrap_or_else(|| ident.to_string());
        let interpret = match args.interpret {
            Some(expr) => quote!(#expr),
            None => quote!(::factories_capture_schema::Interpretation::NONE),
        };
        let push = quote! {
            ::factories_capture_schema::FieldVisitor::field(visitor, #field_name, #interpret);
            ::factories_capture_schema::CaptureSchema::capture(&self.#ident, visitor, config);
        };
        pushes.push(match args.when {
            Some(when) => quote! { if (#when)(config) { #push } },
            None => push,
        });
    }

    quote! {
        impl #impl_generics ::factories_capture_schema::CaptureSchema for #name #ty_generics #where_clause {
            fn capture<__V: ::factories_capture_schema::FieldVisitor>(
                &self,
                visitor: &mut __V,
                config: &::factories_capture_schema::CaptureConfig<'_>,
            ) {
                ::factories_capture_schema::FieldVisitor::begin_struct(visitor, #name_str);
                #(#pushes)*
                ::factories_capture_schema::FieldVisitor::end_struct(visitor);
            }
        }
    }
    .into()
}

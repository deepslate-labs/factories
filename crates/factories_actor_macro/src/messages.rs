use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::spanned::Spanned;
use syn::{Attribute, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, Meta, Pat, ReturnType, Type};

use crate::util;

/// Parsed `#[handler(...)]` configuration.
#[derive(Default)]
struct HandlerConfig {
    /// Existing message type to decompose instead of generating a new one.
    message: Option<Type>,
}

/// One handler parameter: a message field (generated message) or the name of
/// a field to decompose out of an existing message.
struct HandlerParam {
    ident: Ident,
    ty: Type,
}

pub fn messages(attrs: TokenStream, mut input: ItemImpl) -> TokenStream {
    if !attrs.is_empty() {
        proc_macro_error::emit_error!(attrs.span(), "#[messages] takes no arguments");
    }

    // Strip the `#[handler]` markers first: this keeps the dummy output (used
    // in place of the real expansion when diagnostics are emitted) free of
    // unresolvable attributes, so the impl block and its methods survive
    // errors without cascading into the rest of the crate.
    let mut handlers = Vec::new();
    for item in &mut input.items {
        match item {
            ImplItem::Fn(function) => {
                let mut markers = Vec::new();
                function.attrs.retain(|attr| {
                    if attr.path().is_ident("handler") {
                        markers.push(attr.clone());
                        false
                    } else {
                        true
                    }
                });

                if !markers.is_empty() {
                    handlers.push((function.clone(), markers));
                }
            }
            ImplItem::Const(item) => reject_misplaced_markers(&item.attrs),
            ImplItem::Type(item) => reject_misplaced_markers(&item.attrs),
            ImplItem::Macro(item) => reject_misplaced_markers(&item.attrs),
            // Verbatim/unknown items: the macro cannot see their attributes,
            // so a stray `#[handler]` would be silently ignored - reject the
            // item instead.
            other => proc_macro_error::emit_error!(
                other.span(),
                "#[messages] does not support this item, move it to a separate impl block"
            ),
        }
    }

    proc_macro_error::set_dummy(input.to_token_stream());

    if let Some((_, trait_path, _)) = &input.trait_ {
        proc_macro_error::emit_error!(
            trait_path.span(),
            "#[messages] only supports inherent impls, not trait impls"
        );
    }

    if !input.generics.params.is_empty() {
        proc_macro_error::emit_error!(
            input.generics.span(),
            "#[messages] does not support generic impls"
        );
    }

    let self_ty = &input.self_ty;
    let mut generated = TokenStream::new();
    for (function, markers) in &handlers {
        generated.extend(expand_handler(self_ty, function, markers));
    }

    // The impl block is re-emitted unchanged (markers stripped): handler
    // methods stay plain methods, the message machinery is purely additive.
    quote! {
        #input
        #generated
    }
}

/// `#[handler]` on a non-method item: emit an error. The marker is left in
/// place - the expansion is discarded once diagnostics exist, so rustc's
/// follow-up "cannot find attribute" noise is acceptable.
fn reject_misplaced_markers(attrs: &[Attribute]) {
    for attr in attrs {
        if attr.path().is_ident("handler") {
            proc_macro_error::emit_error!(attr.span(), "#[handler] is only supported on methods");
        }
    }
}

fn expand_handler(
    self_ty: &Type,
    function: &ImplItemFn,
    markers: &[Attribute],
) -> Option<TokenStream> {
    let mut config = HandlerConfig::default();
    for attr in markers {
        // Bare `#[handler]` has no keys to parse.
        if matches!(attr.meta, Meta::Path(_)) {
            continue;
        }

        let result = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("message") {
                util::set_value(&mut config.message, &meta)
            } else {
                Err(meta.error("unknown key, expected `message`"))
            }
        });

        if let Err(error) = result {
            util::emit_syn_error(error);
        }
    }

    let signature = &function.sig;

    if !signature.generics.params.is_empty() || signature.generics.where_clause.is_some() {
        proc_macro_error::emit_error!(signature.generics.span(), "handlers cannot be generic");
        return None;
    }

    if let Some(unsafety) = &signature.unsafety {
        proc_macro_error::emit_error!(unsafety.span(), "handlers cannot be `unsafe`");
        return None;
    }

    if let Some(abi) = &signature.abi {
        proc_macro_error::emit_error!(abi.span(), "handlers cannot be `extern`");
        return None;
    }

    // The receiver picks the access mode: `&self` runs under shared access,
    // `&mut self` under exclusive access. Whether the actor's lock strategy
    // actually supports the mode is checked by the generated impl's bounds.
    let Some(receiver) = signature.receiver() else {
        proc_macro_error::emit_error!(
            signature.ident.span(),
            "handlers must take `&self` or `&mut self`"
        );
        return None;
    };

    if receiver.reference.is_none() || receiver.colon_token.is_some() {
        proc_macro_error::emit_error!(
            receiver.span(),
            "handlers must take `&self` or `&mut self`"
        );
        return None;
    }

    let exclusive = receiver.mutability.is_some();

    // Parameters become message fields (or select the fields to decompose out
    // of an existing message), so they must be plain identifiers.
    let mut params = Vec::new();
    for argument in signature.inputs.iter().skip(1) {
        let FnArg::Typed(argument) = argument else {
            continue;
        };

        if let Some(attr) = argument.attrs.first() {
            proc_macro_error::emit_error!(
                attr.span(),
                "parameter attributes are not supported (yet)"
            );
            return None;
        }

        let pattern = match argument.pat.as_ref() {
            Pat::Ident(pattern) if pattern.subpat.is_none() => pattern,
            other => {
                proc_macro_error::emit_error!(
                    other.span(),
                    "handler parameters must be plain identifiers"
                );
                return None;
            }
        };

        params.push(HandlerParam {
            ident: pattern.ident.clone(),
            ty: (*argument.ty).clone(),
        });
    }

    let answer = match &signature.output {
        ReturnType::Default => quote!(()),
        ReturnType::Type(_, ty) => ty.to_token_stream(),
    };

    let param_idents: Vec<&Ident> = params.iter().map(|param| &param.ident).collect();

    let (message_ty, message_decl, destructure) = match &config.message {
        Some(existing) => {
            // Decompose an existing message: parameter names select fields by
            // name, the rest pattern defers the field check to the type
            // checker - the macro never needs to see the message definition.
            let Type::Path(path) = existing else {
                proc_macro_error::emit_error!(existing.span(), "`message` must be a type path");
                return None;
            };

            let destructure = if params.is_empty() {
                TokenStream::new()
            } else {
                quote! { let #path { #(#param_idents,)* .. } = message; }
            };

            (existing.to_token_stream(), TokenStream::new(), destructure)
        }
        None => {
            let message_ident = message_type_name(&signature.ident)?;
            let vis = &function.vis;

            let declaration = if params.is_empty() {
                quote! { #vis struct #message_ident; }
            } else {
                let fields = params
                    .iter()
                    .map(|HandlerParam { ident, ty }| quote!(pub #ident: #ty));
                quote! { #vis struct #message_ident { #(#fields,)* } }
            };

            let rtti_name = quote!(::core::stringify!(#message_ident));
            let message_impl = crate::message::implement_message(&message_ident, &answer, &rtti_name);

            let destructure = if params.is_empty() {
                TokenStream::new()
            } else {
                quote! { let #message_ident { #(#param_idents),* } = message; }
            };

            (
                message_ident.to_token_stream(),
                quote!(#declaration #message_impl),
                destructure,
            )
        }
    };

    let access = if exclusive {
        quote!(::factories_actor::runtime::lock::Exclusive)
    } else {
        quote!(::factories_actor::runtime::lock::Shared)
    };

    let guard = if exclusive {
        quote!(mut guard)
    } else {
        quote!(guard)
    };
    let message_binding = if destructure.is_empty() {
        quote!(_)
    } else {
        quote!(message)
    };
    let fn_ident = &signature.ident;
    let maybe_await = signature.asyncness.map(|_| quote!(.await));

    Some(quote! {
        #message_decl

        impl ::factories_actor::actor::MessageHandler<#message_ty> for #self_ty {
            type AccessMode = #access;

            const DISPATCHER:
                ::factories_actor::actor::dispatch::StaticDispatcher<#self_ty, #message_ty> =
                ::factories_actor::declare_static_dispatcher!(#self_ty, #message_ty);

            fn handle<'a>(
                ctx: ::factories_actor::actor::MessageHandlerContext<'a, #message_ty, Self, #access>,
            ) -> impl ::core::future::Future<Output = ()> + 'a {
                async move {
                    let (#guard, #message_binding, answer) = ctx.into_parts();
                    #destructure
                    let result = guard.#fn_ident(#(#param_idents),*)#maybe_await;
                    if let Some(answer) = answer {
                        let _ = answer.send(result);
                    }
                }
            }
        }

        ::factories_actor::register_dynamic_handler_if_enabled!(#self_ty, #message_ty);
    })
}

/// `do_something` -> `DoSomething`. The message type inherits the fn ident's
/// span, so e.g. name-collision errors point at the handler.
fn message_type_name(ident: &Ident) -> Option<Ident> {
    let snake = ident.to_string();
    let mut pascal = String::new();
    for segment in snake.trim_start_matches("r#").split('_') {
        let mut chars = segment.chars();
        if let Some(first) = chars.next() {
            pascal.extend(first.to_uppercase());
            pascal.push_str(chars.as_str());
        }
    }

    match syn::parse_str::<Ident>(&pascal) {
        Ok(mut message_ident) => {
            message_ident.set_span(ident.span());
            Some(message_ident)
        }
        Err(_) => {
            proc_macro_error::emit_error!(
                ident.span(),
                "cannot derive a message type name from `{}`, use #[handler(message = ...)]",
                ident
            );
            None
        }
    }
}

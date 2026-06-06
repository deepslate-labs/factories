use proc_macro2::{Span, TokenStream};
use quote::{ToTokens, quote};
use syn::spanned::Spanned;
use syn::{Attribute, FnArg, Ident, ImplItem, ImplItemFn, ItemImpl, Meta, Pat, ReturnType, Type};

use crate::util;

/// Parsed `#[handler(...)]` configuration.
#[derive(Default)]
struct HandlerConfig {
    /// Existing message type to decompose instead of generating a new one.
    message: Option<Type>,
    /// Answer type of a generated message whose handler answers manually -
    /// there is no return type to infer it from.
    answer: Option<Type>,
    /// `die_on_err`: a handler error fails the actor.
    die_on_err: Option<(DieMode, Span)>,
}

/// What happens to a `die_on_err` handler's error.
#[derive(Copy, Clone, PartialEq)]
enum DieMode {
    /// Bare `die_on_err`: the full result stays the answer, the error is
    /// *also* cloned into the actor's death.
    Forward,
    /// `die_on_err = consume`: the death consumes the error, the answer
    /// becomes the Ok part.
    Consume,
}

/// What a handler parameter receives.
///
/// Markers re-route dispatch machinery into the parameter; the macro never
/// inspects the parameter's *type* - the generated method call checks it.
enum ParamBinding {
    /// Plain parameter: a message field (generated message) or the name of a
    /// field to decompose out of an existing message.
    Field(Ident, Type),
    /// `#[answer]`: the answer sender (`Option<AnswerSender<M>>`).
    Answer,
    /// `#[message]`: the whole message by value.
    Message,
    /// `#[envelope]`: the sealed message envelope, e.g. for forwarding.
    Envelope,
    /// `#[context]`: the actor's own runtime services (`ActorContext`).
    Context,
}

/// Parameter markers known to the macro; stripped from the re-emitted method.
const PARAM_MARKERS: &[&str] = &["answer", "message", "envelope", "context"];

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

                    // The parameter markers belong to the macro - strip them
                    // from the re-emitted method too (rustc rejects unknown
                    // parameter attributes). The clone above keeps them for
                    // analysis.
                    for argument in &mut function.sig.inputs {
                        if let FnArg::Typed(argument) = argument {
                            argument.attrs.retain(|attr| {
                                !PARAM_MARKERS
                                    .iter()
                                    .any(|marker| attr.path().is_ident(marker))
                            });
                        }
                    }
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
            } else if meta.path.is_ident("answer") {
                util::set_value(&mut config.answer, &meta)
            } else if meta.path.is_ident("die_on_err") {
                let mode = if meta.input.peek(syn::Token![=]) {
                    let value: Ident = meta.value()?.parse()?;
                    if value == "consume" {
                        DieMode::Consume
                    } else {
                        return Err(syn::Error::new(
                            value.span(),
                            "unknown `die_on_err` mode, expected `consume`",
                        ));
                    }
                } else {
                    DieMode::Forward
                };

                if config.die_on_err.is_some() {
                    proc_macro_error::emit_error!(meta.path.span(), "duplicate key");
                } else {
                    config.die_on_err = Some((mode, meta.path.span()));
                }

                Ok(())
            } else {
                Err(meta.error(
                    "unknown key, expected one of `message`, `answer`, `die_on_err`",
                ))
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

    // Plain parameters become message fields (or select the fields to
    // decompose out of an existing message), so they must be plain
    // identifiers. Marked parameters receive a binding by position, their
    // patterns and types are entirely the method's business.
    let mut bindings = Vec::new();
    let mut answer_param: Option<Span> = None;
    let mut message_param: Option<Span> = None;
    let mut envelope_param: Option<Span> = None;
    let mut context_param: Option<Span> = None;

    for argument in signature.inputs.iter().skip(1) {
        let FnArg::Typed(argument) = argument else {
            continue;
        };

        let mut marker: Option<Span> = None;
        let mut binding = None;
        for attr in &argument.attrs {
            let marked = if attr.path().is_ident("answer") {
                ParamBinding::Answer
            } else if attr.path().is_ident("message") {
                ParamBinding::Message
            } else if attr.path().is_ident("envelope") {
                ParamBinding::Envelope
            } else if attr.path().is_ident("context") {
                ParamBinding::Context
            } else {
                proc_macro_error::emit_error!(
                    attr.span(),
                    "unknown parameter attribute, expected one of \
                     `#[answer]`, `#[message]`, `#[envelope]`, `#[context]`"
                );
                return None;
            };

            if !matches!(attr.meta, Meta::Path(_)) {
                proc_macro_error::emit_error!(attr.span(), "parameter markers take no arguments");
                return None;
            }

            if marker.is_some() {
                proc_macro_error::emit_error!(attr.span(), "only one marker per parameter");
                return None;
            }

            marker = Some(attr.span());
            binding = Some(marked);
        }

        match binding {
            Some(binding) => {
                let span = marker.expect("marker span recorded with the binding");
                let (slot, name) = match &binding {
                    ParamBinding::Answer => (&mut answer_param, "#[answer]"),
                    ParamBinding::Message => (&mut message_param, "#[message]"),
                    ParamBinding::Envelope => (&mut envelope_param, "#[envelope]"),
                    ParamBinding::Context => (&mut context_param, "#[context]"),
                    ParamBinding::Field(..) => unreachable!("markers never produce fields"),
                };

                if slot.is_some() {
                    proc_macro_error::emit_error!(span, "duplicate `{}` parameter", name);
                    return None;
                }

                *slot = Some(span);
                bindings.push(binding);
            }
            None => {
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

                bindings.push(ParamBinding::Field(
                    pattern.ident.clone(),
                    (*argument.ty).clone(),
                ));
            }
        }
    }

    let fields: Vec<(&Ident, &Type)> = bindings
        .iter()
        .filter_map(|binding| match binding {
            ParamBinding::Field(ident, ty) => Some((ident, ty)),
            _ => None,
        })
        .collect();

    // A handler that takes the answer sender (directly, or sealed inside the
    // envelope) answers manually - the automatic answer is disabled.
    let manual_answer = answer_param.is_some() || envelope_param.is_some();

    if let Some(span) = envelope_param {
        // `#[context]` is fine alongside the envelope: it is not derived from
        // the message. Everything else is.
        let conflicting = bindings
            .iter()
            .any(|binding| !matches!(binding, ParamBinding::Envelope | ParamBinding::Context));

        if conflicting {
            proc_macro_error::emit_error!(
                span,
                "#[envelope] receives the sealed envelope, it cannot be combined with \
                 message-derived parameters"
            );
            return None;
        }
    }

    if let Some(span) = message_param {
        if !fields.is_empty() {
            proc_macro_error::emit_error!(
                span,
                "#[message] receives the whole message, it cannot be combined with \
                 decomposed field parameters"
            );
            return None;
        }
    }

    if manual_answer && !matches!(signature.output, ReturnType::Default) {
        proc_macro_error::emit_error!(
            signature.output.span(),
            "this handler answers manually (#[answer]/#[envelope]), remove the return type"
        );
        return None;
    }

    if let Some((_, span)) = config.die_on_err {
        if manual_answer {
            proc_macro_error::emit_error!(
                span,
                "`die_on_err` cannot be combined with manual answering \
                 (#[answer]/#[envelope]) - fail the actor through #[context] instead"
            );
            return None;
        }

        if matches!(signature.output, ReturnType::Default) {
            proc_macro_error::emit_error!(
                span,
                "`die_on_err` requires a handler with a result-like return type"
            );
            return None;
        }
    }

    if let Some(answer_override) = &config.answer {
        if config.message.is_some() {
            proc_macro_error::emit_error!(
                answer_override.span(),
                "`answer` cannot be combined with `message = ...`, the existing message \
                 fixes its own answer type"
            );
            return None;
        }

        if !manual_answer {
            proc_macro_error::emit_error!(
                answer_override.span(),
                "`answer` requires a manually answering handler \
                 (a parameter marked #[answer] or #[envelope])"
            );
            return None;
        }
    }

    // The answer type of a generated message: inferred from the return type,
    // or - for manually answering handlers - taken from the `answer` key. A
    // `die_on_err = consume` handler answers the Ok part of its result; the
    // `ResultLike` projection extracts it without syntactically parsing the
    // return type (which would break on aliases).
    let answer = if manual_answer {
        config
            .answer
            .as_ref()
            .map_or_else(|| quote!(()), ToTokens::to_token_stream)
    } else {
        match &signature.output {
            ReturnType::Default => quote!(()),
            ReturnType::Type(_, ty) => match config.die_on_err {
                Some((DieMode::Consume, _)) => {
                    quote!(<#ty as ::factories_actor::runtime::result::ResultLike>::Ok)
                }
                _ => ty.to_token_stream(),
            },
        }
    };

    let field_idents: Vec<&Ident> = fields.iter().map(|(ident, _)| *ident).collect();

    let (message_ty, message_decl, destructure) = match &config.message {
        Some(existing) => {
            // Decompose an existing message: parameter names select fields by
            // name, the rest pattern defers the field check to the type
            // checker - the macro never needs to see the message definition.
            let Type::Path(path) = existing else {
                proc_macro_error::emit_error!(existing.span(), "`message` must be a type path");
                return None;
            };

            let destructure = if field_idents.is_empty() {
                TokenStream::new()
            } else {
                quote! { let #path { #(#field_idents,)* .. } = message; }
            };

            (existing.to_token_stream(), TokenStream::new(), destructure)
        }
        None => {
            let message_ident = message_type_name(&signature.ident)?;
            let vis = &function.vis;

            let declaration = if fields.is_empty() {
                quote! { #vis struct #message_ident; }
            } else {
                let struct_fields = fields
                    .iter()
                    .map(|(ident, ty)| quote!(pub #ident: #ty));
                quote! { #vis struct #message_ident { #(#struct_fields,)* } }
            };

            let rtti_name = quote!(::core::stringify!(#message_ident));
            let message_impl =
                crate::message::implement_message(&message_ident, &answer, &rtti_name);

            let destructure = if field_idents.is_empty() {
                TokenStream::new()
            } else {
                quote! { let #message_ident { #(#field_idents),* } = message; }
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

    let fn_ident = &signature.ident;
    let maybe_await = signature.asyncness.map(|_| quote!(.await));
    let arguments = bindings.iter().map(|binding| match binding {
        ParamBinding::Field(ident, _) => ident.to_token_stream(),
        ParamBinding::Answer => quote!(answer),
        ParamBinding::Message => quote!(message),
        ParamBinding::Envelope => quote!(envelope),
        ParamBinding::Context => quote!(actor_context),
    });
    let call = quote!(guard.#fn_ident(#(#arguments),*)#maybe_await);

    // The actor context borrows the run loop's state, not the handler
    // context - grabbed before the handler context is decomposed.
    let context_prologue = if context_param.is_some() || config.die_on_err.is_some() {
        quote!(let actor_context = ctx.actor_context();)
    } else {
        TokenStream::new()
    };

    let body = if envelope_param.is_some() {
        // The envelope stays sealed: the message is never unwrapped, the
        // answer sender travels inside. It is passed as a `SendableEnvelope`
        // because `async fn` arguments are captured into the future's initial
        // state - a raw (`!Send`) envelope argument would fail thread-safe
        // dispatch demands even if consumed before the first await. The
        // conversion cannot fail today: channels verify sendability at the
        // boundary before an envelope ever reaches a handler.
        quote! {
            #context_prologue
            let (#guard, envelope) = ctx.into_parts_with_envelope();
            let envelope = match ::factories_actor::message::envelope::SendableEnvelope::try_from_envelope(envelope) {
                ::core::result::Result::Ok(envelope) => envelope,
                ::core::result::Result::Err(_) => ::core::unreachable!(
                    "#[envelope] handlers receive a SendableEnvelope, but the dispatched envelope is not sendable"
                ),
            };
            #call;
        }
    } else {
        let message_binding = if destructure.is_empty() && message_param.is_none() {
            quote!(_)
        } else {
            quote!(message)
        };

        if manual_answer {
            quote! {
                #context_prologue
                let (#guard, #message_binding, answer) = ctx.into_parts();
                #destructure
                #call;
            }
        } else {
            let complete = match config.die_on_err {
                // The full result stays the answer; the error is *also*
                // cloned into the actor's death.
                Some((DieMode::Forward, _)) => quote! {
                    let result = #call;
                    if let ::core::result::Result::Err(error) =
                        ::factories_actor::runtime::result::ResultLike::as_result(&result)
                    {
                        actor_context.fail(::core::convert::Into::into(
                            ::core::clone::Clone::clone(error),
                        ));
                    }
                    if let Some(answer) = answer {
                        let _ = answer.send(result);
                    }
                },
                // The death consumes the error; the answer is the Ok part.
                Some((DieMode::Consume, _)) => quote! {
                    match ::factories_actor::runtime::result::ResultLike::into_result(#call) {
                        ::core::result::Result::Ok(value) => {
                            if let Some(answer) = answer {
                                let _ = answer.send(value);
                            }
                        }
                        ::core::result::Result::Err(error) => {
                            actor_context.fail(::core::convert::Into::into(error));
                        }
                    }
                },
                None => quote! {
                    let result = #call;
                    if let Some(answer) = answer {
                        let _ = answer.send(result);
                    }
                },
            };

            quote! {
                #context_prologue
                let (#guard, #message_binding, answer) = ctx.into_parts();
                #destructure
                #complete
            }
        }
    };

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
                    #body
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

//! The `#[protocol]` attribute: turns a trait whose methods name messages into
//! an actor protocol - the trait itself (the zero-cost generic-bound surface,
//! blanket-impl'd over typed handles) plus a concrete erased handle that carries
//! a cached dispatcher table (the proof the messages bind).

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{FnArg, Ident, ItemTrait, Pat, ReturnType, TraitItem, Type};

/// Whether the protocol's erased handle is `Send + Sync` (wraps an
/// `AnyActorHandle`) or thread-local (wraps an `AnyLocalActorHandle`).
#[derive(Copy, Clone, PartialEq)]
enum Locality {
    Shared,
    Local,
}

/// One protocol message: the method name that fronts it, the binding name to
/// forward, the message type, and its index in the dispatcher table.
struct ProtocolMessage {
    method: Ident,
    binding: Ident,
    message: Type,
    index: usize,
}

pub fn protocol(attrs: TokenStream, input: ItemTrait) -> TokenStream {
    let locality = parse_locality(&attrs);

    // Always emit the trait unchanged on a hard error, so downstream code that
    // names the trait still resolves while diagnostics surface.
    proc_macro_error::set_dummy(quote!(#input));

    if !input.generics.params.is_empty() {
        proc_macro_error::emit_error!(
            input.generics.span(),
            "#[protocol] does not support generic traits"
        );
    }

    let messages = collect_messages(&input);

    let trait_ident = &input.ident;
    let vis = &input.vis;
    let handle_ident = format_ident!("{}Handle", trait_ident);

    // The effective supertraits: a shared protocol is `Send + Sync` (plus
    // whatever the user wrote), so both its impls must prove `Self: Send + Sync`.
    // We forward the bound verbatim into a `where Self: ...` clause rather than
    // inspecting it - no name detection.
    let user_supertraits = &input.supertraits;
    let supertraits = match locality {
        Locality::Shared if user_supertraits.is_empty() => {
            quote!(::core::marker::Send + ::core::marker::Sync)
        }
        Locality::Shared => {
            quote!(::core::marker::Send + ::core::marker::Sync + #user_supertraits)
        }
        Locality::Local => quote!(#user_supertraits),
    };
    // A bare predicate (no `where` keyword) appended into the typed impl's where
    // clause, so it discharges the trait's supertraits without nesting a clause.
    let self_pred = if supertraits.is_empty() {
        TokenStream::new()
    } else {
        quote!(Self: #supertraits,)
    };
    let supertrait_clause = if supertraits.is_empty() {
        TokenStream::new()
    } else {
        quote!(: #supertraits)
    };

    let handle = quote!(::factories_actor::actor::handle);
    let actor = quote!(::factories_actor::actor);
    let message = quote!(::factories_actor::message::Message);

    // Per-message fragments. Trait declarations need the trailing `;`; the impls
    // carry a body instead. The return type is `MessageCall<impl Calling<…>>` -
    // a nested RPITIT, so each impl supplies its own inner: the typed impl reuses
    // `TypedActorHandle::call` (unboxed, zero-cost), the erased impl wraps an
    // `ErasedCall`. Both read as `MessageCall<…>`.
    let trait_methods = messages.iter().map(|m| {
        let sig = method_signature(m, &handle, &message);
        quote!(#sig;)
    });
    let typed_methods = messages.iter().map(|m| {
        let sig = method_signature(m, &handle, &message);
        let binding = &m.binding;
        // Reuse the typed handle's own call path - nothing erased, nothing boxed.
        quote!(#sig { self.call(#binding) })
    });
    let handle_methods = messages.iter().map(|m| {
        let sig = method_signature(m, &handle, &message);
        let binding = &m.binding;
        let index = m.index;
        quote! {
            #sig {
                // SAFETY: `dispatchers[#index]` was bound for this message on the
                //         actor behind `inner` when this handle was constructed.
                #handle::MessageCall::new(unsafe {
                    #actor::protocol::ErasedCall::new(&self.inner, self.dispatchers[#index], #binding)
                })
            }
        }
    });

    let handler_bounds = messages.iter().map(|m| {
        let msg = &m.message;
        quote!(#actor::MessageHandler<#msg>)
    });
    let handler_bounds_typed = handler_bounds.clone();

    // The cached dispatcher table - one entry per message, filled at construction.
    let table_len = messages.len();
    let inner_ty = match locality {
        Locality::Shared => quote!(#handle::AnyActorHandle),
        Locality::Local => quote!(#handle::AnyLocalActorHandle),
    };

    // `From<TypedActorHandle<A>>`: infallible, table from each `M::DISPATCHER`.
    let from_dispatchers = messages.iter().map(|m| {
        let msg = &m.message;
        quote!(<__A as #actor::MessageHandler<#msg>>::DISPATCHER.into_dispatcher())
    });
    let (erase, send_sync_bounds) = match locality {
        Locality::Shared => (
            quote!(erase_type),
            quote!(
                <__A as #actor::Actor>::Channel: ::core::marker::Send + ::core::marker::Sync,
                <__A as #actor::Actor>::Error: ::core::marker::Send + ::core::marker::Sync,
            ),
        ),
        Locality::Local => (quote!(erase_type_local), TokenStream::new()),
    };

    // `TryFrom<erased>`: registry-checked, table from `bind_dispatcher` per RTTI.
    let bind_lets = messages.iter().map(|m| {
        let msg = &m.message;
        let slot = format_ident!("__d{}", m.index);
        quote! {
            let ::core::option::Option::Some(#slot) =
                #handle::ActorHandle::bind_dispatcher(&__any, <#msg as #message>::RTTI)
            else {
                return ::core::result::Result::Err(__any);
            };
        }
    });
    let bind_idents = messages.iter().map(|m| format_ident!("__d{}", m.index));

    let handle_doc = format!(
        "Erased handle for the [`{trait_ident}`] protocol: any actor speaking it, \
         with the actor type erased but the message set guaranteed.\n\n\
         Build one infallibly from a typed handle (`From`) or fallibly from an \
         [`AnyActorHandle`](::factories_actor::actor::handle::AnyActorHandle) (`TryFrom`)."
    );

    quote! {
        #vis trait #trait_ident #supertrait_clause {
            #(#trait_methods)*
        }

        // Static, zero-cost surface: any typed handle whose actor handles every
        // protocol message. The `where Self: ...` discharges the supertraits.
        impl<__A> #trait_ident for #handle::TypedActorHandle<__A>
        where
            __A: #actor::Actor #(+ #handler_bounds_typed)*,
            Self: ::core::marker::Sized,
            #self_pred
        {
            #(#typed_methods)*
        }

        #[doc = #handle_doc]
        #[derive(::core::clone::Clone, ::core::fmt::Debug)]
        #vis struct #handle_ident {
            inner: #inner_ty,
            dispatchers: [#actor::dispatch::ActorMessageDispatcher; #table_len],
        }

        impl<__A> ::core::convert::From<#handle::TypedActorHandle<__A>> for #handle_ident
        where
            __A: #actor::Actor #(+ #handler_bounds)*,
            #send_sync_bounds
        {
            fn from(__handle: #handle::TypedActorHandle<__A>) -> Self {
                Self {
                    dispatchers: [ #(#from_dispatchers),* ],
                    inner: __handle.#erase(),
                }
            }
        }

        impl ::core::convert::TryFrom<#inner_ty> for #handle_ident {
            type Error = #inner_ty;

            fn try_from(__any: #inner_ty) -> ::core::result::Result<Self, #inner_ty> {
                #(#bind_lets)*
                ::core::result::Result::Ok(Self {
                    inner: __any,
                    dispatchers: [ #(#bind_idents),* ],
                })
            }
        }

        impl #trait_ident for #handle_ident {
            #(#handle_methods)*
        }
    }
}

/// Parse the attribute arguments: empty (shared) or `local`.
fn parse_locality(attrs: &TokenStream) -> Locality {
    if attrs.is_empty() {
        return Locality::Shared;
    }

    match syn::parse2::<Ident>(attrs.clone()) {
        Ok(ident) if ident == "local" => Locality::Local,
        Ok(ident) => {
            proc_macro_error::emit_error!(
                ident.span(),
                "unknown #[protocol] argument `{}`, expected `local` or nothing",
                ident
            );
            Locality::Shared
        }
        Err(_) => {
            proc_macro_error::emit_error!(
                attrs.span(),
                "#[protocol] takes at most one argument: `local`"
            );
            Locality::Shared
        }
    }
}

/// Collect and validate the protocol's messages from the trait's methods.
fn collect_messages(input: &ItemTrait) -> Vec<ProtocolMessage> {
    let mut messages = Vec::new();

    for item in &input.items {
        let TraitItem::Fn(function) = item else {
            proc_macro_error::emit_error!(
                item.span(),
                "#[protocol] only supports method declarations"
            );
            continue;
        };

        let sig = &function.sig;

        if function.default.is_some() {
            proc_macro_error::emit_error!(
                sig.ident.span(),
                "#[protocol] methods cannot have a body"
            );
        }
        if !sig.generics.params.is_empty() {
            proc_macro_error::emit_error!(sig.generics.span(), "#[protocol] methods cannot be generic");
        }
        if !matches!(sig.output, ReturnType::Default) {
            proc_macro_error::emit_error!(
                sig.output.span(),
                "#[protocol] methods take no return type: the answer is the message's own \
                 `Message::Answer`"
            );
        }

        // Must be `&self` plus exactly one message parameter.
        let Some(receiver) = sig.receiver() else {
            proc_macro_error::emit_error!(sig.ident.span(), "#[protocol] methods must take `&self`");
            continue;
        };
        if receiver.reference.is_none() || receiver.mutability.is_some() {
            proc_macro_error::emit_error!(receiver.span(), "#[protocol] methods must take `&self`");
        }

        let params: Vec<&FnArg> = sig.inputs.iter().filter(|a| matches!(a, FnArg::Typed(_))).collect();
        let [FnArg::Typed(param)] = params.as_slice() else {
            proc_macro_error::emit_error!(
                sig.ident.span(),
                "#[protocol] methods take exactly one parameter: the message"
            );
            continue;
        };

        let binding = match param.pat.as_ref() {
            Pat::Ident(pat) if pat.subpat.is_none() => pat.ident.clone(),
            other => {
                proc_macro_error::emit_error!(
                    other.span(),
                    "#[protocol] message parameter must be a plain identifier"
                );
                Ident::new("__msg", Span::call_site())
            }
        };

        messages.push(ProtocolMessage {
            method: sig.ident.clone(),
            binding,
            message: (*param.ty).clone(),
            index: messages.len(),
        });
    }

    messages
}

/// The shared method signature, uniform across both impls:
/// `fn name(&self, binding: Msg) -> MessageCall<impl Calling<Output = …>>`.
///
/// The nested `impl Calling` is a per-impl RPITIT, so the typed blanket impl can
/// return `TypedActorHandle::call`'s unboxed `MessageCall` while the erased impl
/// returns a `MessageCall<ErasedCall>` - both matching this written type.
fn method_signature(m: &ProtocolMessage, handle: &TokenStream, message: &TokenStream) -> TokenStream {
    let method = &m.method;
    let binding = &m.binding;
    let msg = &m.message;
    quote! {
        fn #method(
            &self,
            #binding: #msg,
        ) -> #handle::MessageCall<
            impl #handle::Calling<
                Output = ::core::result::Result<
                    <#msg as #message>::Answer,
                    #handle::AskError,
                >,
            >,
        >
    }
}

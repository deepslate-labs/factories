//! The `#[protocol]` attribute: turns a trait whose methods name messages into
//! an actor protocol - the trait itself (the zero-cost generic-bound surface,
//! blanket-impl'd over typed handles) plus a concrete erased handle that carries
//! a cached dispatcher table (the proof the messages bind).

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::spanned::Spanned;
use syn::{FnArg, Ident, ItemTrait, Pat, ReturnType, TraitItem, Type};

use crate::util;

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
    let (locality, krate) = parse_attrs(&attrs);

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

    let handle = quote!(#krate::actor::handle);
    let actor = quote!(#krate::actor);
    let message = quote!(#krate::message::Message);

    // Per-message fragments. Trait declarations need the trailing `;`; the impls
    // carry a body instead. The return type is `MessageCall<impl Calling<…>>`
    // for shared protocols and `LocalMessageCall<impl LocalCalling<…>>` for
    // `local` ones (a `!Send` answer cannot promise the `Send` futures the
    // shared surface guarantees). It is a nested RPITIT, so each impl supplies
    // its own inner: the typed impl reuses `TypedActorHandle::call` /
    // `call_local` (unboxed, zero-cost), the erased impl wraps an `ErasedCall`.
    let trait_methods = messages.iter().map(|m| {
        let sig = method_signature(m, &handle, &message, locality);
        quote!(#sig;)
    });
    let typed_methods = messages.iter().map(|m| {
        let sig = method_signature(m, &handle, &message, locality);
        let binding = &m.binding;
        // Reuse the typed handle's own call path - nothing erased, nothing boxed.
        let call = match locality {
            Locality::Shared => quote!(call),
            Locality::Local => quote!(call_local),
        };
        quote!(#sig { self.#call(#binding) })
    });
    let handle_methods = messages.iter().map(|m| {
        let sig = method_signature(m, &handle, &message, locality);
        let binding = &m.binding;
        let index = m.index;
        let call_ty = match locality {
            Locality::Shared => quote!(#handle::MessageCall),
            Locality::Local => quote!(#handle::LocalMessageCall),
        };
        quote! {
            #sig {
                // SAFETY: `dispatchers[#index]` was bound for this message on the
                //         actor behind `inner` when this handle was constructed.
                #call_ty::new(unsafe {
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

    // `From<H: DerivedHandle>`: infallible, accepts either a bare typed handle
    // or the derive's generated `…Handle` newtype. Table from each `M::DISPATCHER`.
    let actor_ty = quote!(<__H as #handle::DerivedHandle>::Actor);
    let from_dispatchers = messages.iter().map(|m| {
        let msg = &m.message;
        quote!(<#actor_ty as #actor::MessageHandler<#msg>>::DISPATCHER.into_dispatcher())
    });
    let (erase, send_sync_bounds) = match locality {
        Locality::Shared => (
            quote!(erase_type),
            quote!(
                <#actor_ty as #actor::Actor>::Channel: ::core::marker::Send + ::core::marker::Sync,
                <#actor_ty as #actor::Actor>::Error: ::core::marker::Send + ::core::marker::Sync,
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
         Build one infallibly from a typed handle (`From`/`.into()` - either a bare \
         [`TypedActorHandle`](::factories::actor::handle::TypedActorHandle) or the \
         derive's generated `…Handle`), or fallibly from an erased \
         [`AnyActorHandle`](::factories::actor::handle::AnyActorHandle) via `try_bind`."
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

        impl<__H> ::core::convert::From<__H> for #handle_ident
        where
            __H: #handle::DerivedHandle,
            #actor_ty: #actor::Actor #(+ #handler_bounds)*,
            #send_sync_bounds
        {
            fn from(__handle: __H) -> Self {
                let __handle = #handle::DerivedHandle::into_typed_handle(__handle);
                Self {
                    dispatchers: [ #(#from_dispatchers),* ],
                    inner: __handle.#erase(),
                }
            }
        }

        impl #handle_ident {
            /// Bind this protocol against an erased handle, verifying every
            /// protocol message resolves on the actor behind it. Returns the
            /// original handle unchanged on failure.
            ///
            /// (The fallible counterpart to the infallible `From`/`.into()` from a
            /// typed handle - it cannot be `TryFrom` because the blanket `From`
            /// above already occupies that conversion through the standard
            /// library's `From`/`TryFrom` bridge.)
            pub fn try_bind(__any: #inner_ty) -> ::core::result::Result<Self, #inner_ty> {
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

/// Parse the attribute arguments: `local` (a thread-local handle) and/or
/// `crate = "..."` (the crate root the generated code refers back through),
/// in any order. Empty means a shared handle through the `factories` facade.
fn parse_attrs(attrs: &TokenStream) -> (Locality, TokenStream) {
    let mut locality = Locality::Shared;
    let mut crate_override = None;

    if !attrs.is_empty() {
        let parser = syn::meta::parser(|meta| {
            if meta.path.is_ident("local") {
                locality = Locality::Local;
                Ok(())
            } else if meta.path.is_ident("crate") {
                crate_override = Some(meta.value()?.parse::<syn::LitStr>()?);
                Ok(())
            } else {
                Err(meta.error(
                    "unknown #[protocol] argument, expected `local` and/or `crate = \"...\"`",
                ))
            }
        });

        if let Err(error) = parser.parse2(attrs.clone()) {
            util::emit_syn_error(error);
        }
    }

    (locality, util::krate_path(crate_override.as_ref()))
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
/// `fn name(&self, binding: Msg) -> MessageCall<impl Calling<Output = …>>`
/// (shared protocols, `Send`-guaranteed futures) or the
/// `LocalMessageCall<impl LocalCalling<…>>` twin (`local` protocols, no `Send`
/// guarantee).
///
/// The nested `impl Calling` is a per-impl RPITIT, so the typed blanket impl
/// can return `TypedActorHandle::call`'s / `call_local`'s unboxed inner call
/// while the erased impl returns an `ErasedCall` - both matching this written
/// type.
fn method_signature(
    m: &ProtocolMessage,
    handle: &TokenStream,
    message: &TokenStream,
    locality: Locality,
) -> TokenStream {
    let method = &m.method;
    let binding = &m.binding;
    let msg = &m.message;
    let (call_ty, calling) = match locality {
        Locality::Shared => (quote!(#handle::MessageCall), quote!(#handle::Calling)),
        Locality::Local => (
            quote!(#handle::LocalMessageCall),
            quote!(#handle::LocalCalling),
        ),
    };
    quote! {
        fn #method(
            &self,
            #binding: #msg,
        ) -> #call_ty<
            impl #calling<
                Output = ::core::result::Result<
                    <#msg as #message>::Answer,
                    #handle::AskError,
                >,
            >,
        >
    }
}

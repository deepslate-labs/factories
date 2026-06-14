use proc_macro2::TokenStream;
use quote::{ToTokens, format_ident, quote};
use syn::{DeriveInput, Ident, LitStr, Type};

use crate::util;

/// Parsed `#[actor(...)]` configuration; one slot per associated type plus the
/// RTTI name override and the component template. Multiple `#[actor(...)]`
/// attributes merge, duplicate keys are rejected.
#[derive(Default)]
struct ActorConfig {
    template: Option<Type>,
    channel: Option<Type>,
    error: Option<Type>,
    binder: Option<Type>,
    lock: Option<Type>,
    run_loop: Option<Type>,
    shared: Option<Type>,
    event_driver: Option<Type>,
    name: Option<LitStr>,
}

pub fn derive_actor(input: DeriveInput) -> TokenStream {
    util::reject_generics(&input, "Actor");

    let mut config = ActorConfig::default();
    for attr in &input.attrs {
        if !attr.path().is_ident("actor") {
            continue;
        }

        // An unrecoverable parse error ends this attribute, not the derive:
        // the remaining attributes are still checked so all diagnostics
        // surface in one compiler pass.
        let result = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("template") {
                util::set_value(&mut config.template, &meta)
            } else if meta.path.is_ident("channel") {
                util::set_value(&mut config.channel, &meta)
            } else if meta.path.is_ident("error") {
                util::set_value(&mut config.error, &meta)
            } else if meta.path.is_ident("binder") {
                util::set_value(&mut config.binder, &meta)
            } else if meta.path.is_ident("lock") {
                util::set_value(&mut config.lock, &meta)
            } else if meta.path.is_ident("run_loop") {
                util::set_value(&mut config.run_loop, &meta)
            } else if meta.path.is_ident("shared") {
                util::set_value(&mut config.shared, &meta)
            } else if meta.path.is_ident("event_driver") {
                util::set_value(&mut config.event_driver, &meta)
            } else if meta.path.is_ident("name") {
                util::set_value(&mut config.name, &meta)
            } else {
                Err(meta.error(
                    "unknown key, expected one of \
                     `template`, `channel`, `error`, `binder`, `lock`, `run_loop`, `shared`, \
                     `event_driver`, `name`",
                ))
            }
        });

        if let Err(error) = result {
            util::emit_syn_error(error);
        }
    }

    let ident = &input.ident;

    // Per-key precedence: explicit key > template member > built-in default.
    // Defaults and template members are resolved as paths (not decided here)
    // because this proc macro cannot see which features of `factories_actor`
    // are enabled - the feature-gated aliases respectively the template impl
    // can.
    let defaults = quote!(::factories_actor::runtime::defaults);
    let (channel_default, error_default, binder_default, lock_default, run_loop_default) =
        match &config.template {
            Some(template) => {
                let template =
                    quote!(<#template as ::factories_actor::runtime::template::ActorTemplate>);
                (
                    quote!(#template::Channel),
                    quote!(#template::Error),
                    quote!(#template::RuntimeBinder<Self>),
                    quote!(#template::LockStrategy<Self>),
                    quote!(#template::RunLoop<Self>),
                )
            }
            None => (
                quote!(#defaults::DefaultChannel),
                quote!(#defaults::DefaultError),
                quote!(#defaults::DefaultRuntimeBinder<Self>),
                quote!(#defaults::DefaultLockStrategy<Self>),
                quote!(#defaults::DefaultRunLoop<Self>),
            ),
        };

    let channel = util::value_or_default(config.channel, channel_default);
    let error = util::value_or_default(config.error, error_default);
    let binder = util::value_or_default(config.binder, binder_default);
    let lock = util::value_or_default(config.lock, lock_default);
    let run_loop = util::value_or_default(config.run_loop, run_loop_default);
    // Not a template member: the shared-state extension defaults to `()`.
    let shared = util::value_or_default(config.shared, quote!(()));
    let rtti_name = util::rtti_name(config.name, ident);

    let vis = &input.vis;

    // The event driver defaults to a generated loop that autoref-detects an
    // `#[event_source]` impl on the actor (pulling the mailbox when there is
    // none); an explicit `event_driver = ...` takes full control and skips it.
    // Like the handle, the generated loop is a module-scope newtype (it is named
    // by the public `EventDriver` associated type, so it cannot be private); its
    // impls stay in the `const _` block.
    let (event_driver, event_loop_decl, event_loop_items) = match config.event_driver {
        Some(explicit) => (
            explicit.into_token_stream(),
            TokenStream::new(),
            TokenStream::new(),
        ),
        None => {
            let loop_ident = format_ident!("{}EventLoop", ident);
            let loop_doc = format!(
                "Generated event driver for the [`{ident}`] actor: pulls its mailbox, \
                 dispatching to an `#[event_source]` impl when one is present."
            );
            let decl = quote! {
                #[doc = #loop_doc]
                #[derive(::core::clone::Clone, ::core::fmt::Debug)]
                #vis struct #loop_ident;
            };
            let items = generated_event_loop(ident, &loop_ident);
            (loop_ident.into_token_stream(), decl, items)
        }
    };

    // The generated typed-handle newtype. Lives at module scope (not inside the
    // `const _` block) so it is nameable, and inherits the actor's visibility.
    // It is the actor's `TypedHandle`, and `#[messages]` adds the per-message
    // methods to it as inherent impls.
    let handle_ident = format_ident!("{}Handle", ident);
    let handle_ty = quote!(::factories_actor::actor::handle::TypedActorHandle<#ident>);
    let handle_doc = format!(
        "Typed handle for the [`{ident}`] actor, returned when it is spawned.\n\n\
         Derefs to [`TypedActorHandle`](::factories_actor::actor::handle::TypedActorHandle); \
         the per-message methods are added by `#[messages]`."
    );

    // The `unsafe impl` is sound by construction: the RTTI and the impl are
    // emitted for the same type token in one expansion. (If diagnostics were
    // emitted above, proc_macro_error discards this output.)
    quote! {
        #[doc = #handle_doc]
        #[derive(::core::clone::Clone, ::core::fmt::Debug)]
        #vis struct #handle_ident(#handle_ty);

        impl ::core::convert::From<#handle_ty> for #handle_ident {
            fn from(handle: #handle_ty) -> Self {
                Self(handle)
            }
        }

        impl ::core::convert::From<#handle_ident> for #handle_ty {
            fn from(handle: #handle_ident) -> Self {
                handle.0
            }
        }

        impl ::core::ops::Deref for #handle_ident {
            type Target = #handle_ty;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl #handle_ident {
            /// Type-erase into a shared untyped handle (forwards to
            /// [`TypedActorHandle::erase_type`](::factories_actor::actor::handle::TypedActorHandle::erase_type)).
            pub fn erase_type(self) -> ::factories_actor::actor::handle::AnyActorHandle
            where
                <#ident as ::factories_actor::actor::Actor>::Channel:
                    ::core::marker::Send + ::core::marker::Sync,
                <#ident as ::factories_actor::actor::Actor>::Error:
                    ::core::marker::Send + ::core::marker::Sync,
            {
                self.0.erase_type()
            }

            /// Type-erase into a local untyped handle (forwards to
            /// [`TypedActorHandle::erase_type_local`](::factories_actor::actor::handle::TypedActorHandle::erase_type_local)).
            pub fn erase_type_local(self) -> ::factories_actor::actor::handle::AnyLocalActorHandle {
                self.0.erase_type_local()
            }
        }

        #event_loop_decl

        const _: () = {
            ::factories_actor::declare_actor_rtti!(__DERIVED_ACTOR_RTTI, #ident, #rtti_name);

            #event_loop_items

            unsafe impl ::factories_actor::actor::Actor for #ident {
                const RTTI: &'static ::factories_actor::actor::rtti::ActorRtti =
                    __DERIVED_ACTOR_RTTI;

                type Channel = #channel;
                type Error = #error;
                type RuntimeBinder = #binder;
                type LockStrategy = #lock;
                type RunLoop = #run_loop;
                type TypedHandle = #handle_ident;
                type SharedStateExtension = #shared;
                type EventDriver = #event_driver;
            }
        };
    }
}

/// The default event driver emitted by the derive: an [`EventDriver`] that
/// autoref-specializes at the concrete actor type. When the actor implements
/// `ActorEventSource` (via `#[event_source]`), the `&__Probe` arm applies - one
/// fewer autoderef step, so method resolution picks it; otherwise the bare
/// `__Probe` arm pulls straight from the mailbox, exactly like
/// `DefaultMailboxDriver`.
///
/// The specialization *must* be inlined at this concrete site: autoref
/// resolution only fires once `Self` is a known type, so it cannot live behind a
/// generic helper. The two `__Via*` traits and the `__Probe` carrier are nested
/// inside `next` so they never leak into the actor's namespace.
fn generated_event_loop(ident: &Ident, loop_ident: &Ident) -> TokenStream {
    let dispatched = quote!(::factories_actor::actor::dispatch::DispatchedActorMessage);
    let event_context = quote!(::factories_actor::actor::event::EventContext);
    let mailbox = quote!(::factories_actor::spawn::ActorMailbox);
    let event_source = quote!(::factories_actor::actor::event::ActorEventSource);
    let actor = quote!(::factories_actor::actor::Actor);
    let send = quote!(::core::marker::Send);
    let output = quote!(::core::option::Option<#dispatched>);
    let future = quote!(::core::future::Future<Output = #output>);

    // The loop yields `Send` futures (the standard loops are work-stealing). The
    // mailbox arm pulls from a `Send` mailbox via `receive()` (whose future is
    // `Send`-readable); the source arm is generic in `__A` (so the gating `__A:
    // ActorEventSource` is a real bound, not trivial) and forwards `next_event`'s
    // already-`Send` future. `__A` resolves to `#ident` at the call.

    quote! {
        impl ::core::convert::From<&#ident> for #loop_ident {
            fn from(_actor: &#ident) -> Self {
                Self
            }
        }

        impl<__M> ::factories_actor::actor::event::EventDriver<#ident, __M> for #loop_ident
        where
            __M: #mailbox + ::core::marker::Send,
        {
            fn next<'__event>(
                &'__event mut self,
                cx: #event_context<'__event, #ident>,
                mailbox: &'__event mut __M,
            ) -> impl #future + #send + '__event {
                // Higher priority (impl for `&__Probe`, one fewer autoderef):
                // gated on the actor having an event source. Generic in `__A`,
                // forwarding `next_event`'s declared demand bound.
                trait __ViaSource<__A: #actor> {
                    fn __select<'__a, __MB: #mailbox + ::core::marker::Send>(
                        self,
                        cx: #event_context<'__a, __A>,
                        mailbox: &'__a mut __MB,
                    ) -> impl #future + #send + '__a;
                }

                // Fallback (impl for bare `__Probe`): pull from the mailbox.
                // Concrete in `#ident` so `receive()`'s `Send` future discharges
                // the demand bound against the *concrete* demand.
                trait __ViaMailbox {
                    fn __select<'__a, __MB: #mailbox + ::core::marker::Send>(
                        self,
                        cx: #event_context<'__a, #ident>,
                        mailbox: &'__a mut __MB,
                    ) -> impl #future + #send + '__a;
                }

                // A zero-sized carrier for the actor type. Copy by hand so the
                // bare-`__Probe` arm can be reached by autoderef without forcing
                // `#ident: Copy`.
                struct __Probe<__A>(::core::marker::PhantomData<__A>);
                impl<__A> ::core::clone::Clone for __Probe<__A> {
                    fn clone(&self) -> Self {
                        *self
                    }
                }
                impl<__A> ::core::marker::Copy for __Probe<__A> {}

                impl<__A> __ViaSource<__A> for &__Probe<__A>
                where
                    __A: #event_source,
                {
                    fn __select<'__a, __MB: #mailbox + ::core::marker::Send>(
                        self,
                        cx: #event_context<'__a, __A>,
                        mailbox: &'__a mut __MB,
                    ) -> impl #future + #send + '__a {
                        <__A as #event_source>::next_event(cx, mailbox)
                    }
                }

                impl __ViaMailbox for __Probe<#ident> {
                    fn __select<'__a, __MB: #mailbox + ::core::marker::Send>(
                        self,
                        _cx: #event_context<'__a, #ident>,
                        mailbox: &'__a mut __MB,
                    ) -> impl #future + #send + '__a {
                        mailbox.receive()
                    }
                }

                // A `&const` probe so the borrowed-self lifetime captured by the
                // `&__Probe` arm's RPITIT future is `'static` (harmless), not a
                // temporary's - otherwise the returned future would not outlive
                // the call.
                const __PROBE: __Probe<#ident> = __Probe(::core::marker::PhantomData);
                (&__PROBE).__select(cx, mailbox)
            }
        }
    }
}

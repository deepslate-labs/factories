use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{DeriveInput, LitStr, Type};

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
                let template = quote!(<#template as ::factories_actor::runtime::template::ActorTemplate>);
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
    // Not template members: the shared-state extension defaults to `()`, the
    // event driver to the plain mailbox-pulling `DefaultMailboxDriver`.
    let shared = util::value_or_default(config.shared, quote!(()));
    let event_driver = util::value_or_default(
        config.event_driver,
        quote!(::factories_actor::actor::event::DefaultMailboxDriver),
    );
    let rtti_name = util::rtti_name(config.name, ident);

    // The generated typed-handle newtype. Lives at module scope (not inside the
    // `const _` block) so it is nameable, and inherits the actor's visibility.
    // It is the actor's `TypedHandle`, and `#[messages]` adds the per-message
    // methods to it as inherent impls.
    let vis = &input.vis;
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

        const _: () = {
            ::factories_actor::declare_actor_rtti!(__DERIVED_ACTOR_RTTI, #ident, #rtti_name);

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

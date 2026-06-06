use proc_macro2::TokenStream;
use quote::quote;
use syn::{DeriveInput, LitStr, Type};

use crate::util;

/// Parsed `#[actor(...)]` configuration; one slot per associated type plus the
/// RTTI name override. Multiple `#[actor(...)]` attributes merge, duplicate
/// keys are rejected.
#[derive(Default)]
struct ActorConfig {
    channel: Option<Type>,
    error: Option<Type>,
    binder: Option<Type>,
    lock: Option<Type>,
    run_loop: Option<Type>,
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
            if meta.path.is_ident("channel") {
                util::set_value(&mut config.channel, &meta)
            } else if meta.path.is_ident("error") {
                util::set_value(&mut config.error, &meta)
            } else if meta.path.is_ident("binder") {
                util::set_value(&mut config.binder, &meta)
            } else if meta.path.is_ident("lock") {
                util::set_value(&mut config.lock, &meta)
            } else if meta.path.is_ident("run_loop") {
                util::set_value(&mut config.run_loop, &meta)
            } else if meta.path.is_ident("name") {
                util::set_value(&mut config.name, &meta)
            } else {
                Err(meta.error(
                    "unknown key, expected one of \
                     `channel`, `error`, `binder`, `lock`, `run_loop`, `name`",
                ))
            }
        });

        if let Err(error) = result {
            util::emit_syn_error(error);
        }
    }

    let ident = &input.ident;
    let defaults = quote!(::factories_actor::runtime::defaults);

    let channel = util::value_or_default(config.channel, quote!(#defaults::DefaultChannel));
    let error = util::value_or_default(config.error, quote!(#defaults::DefaultError));
    let binder = util::value_or_default(config.binder, quote!(#defaults::DefaultRuntimeBinder<Self>));
    let lock = util::value_or_default(config.lock, quote!(#defaults::DefaultLockStrategy<Self>));
    let run_loop = util::value_or_default(config.run_loop, quote!(#defaults::DefaultRunLoop<Self>));
    let rtti_name = util::rtti_name(config.name, ident);

    // The `unsafe impl` is sound by construction: the RTTI and the impl are
    // emitted for the same type token in one expansion. (If diagnostics were
    // emitted above, proc_macro_error discards this output.)
    quote! {
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
            }
        };
    }
}

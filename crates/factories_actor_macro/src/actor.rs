use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::meta::ParseNestedMeta;
use syn::spanned::Spanned;
use syn::{DeriveInput, LitStr, Result, Type};

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

/// Parse `= <Type>` into the slot.
///
/// Duplicates are emitted instead of returned so parsing recovers: the value
/// is consumed either way, keeping the cursor aligned for the keys that
/// follow.
fn set_type(slot: &mut Option<Type>, meta: &ParseNestedMeta) -> Result<()> {
    let value = meta.value()?.parse()?;

    if slot.is_some() {
        proc_macro_error::emit_error!(meta.path.span(), "duplicate key");
    } else {
        *slot = Some(value);
    }

    Ok(())
}

/// Emit the explicitly configured type, or the given path into
/// `factories_actor::runtime::defaults`. Defaults are resolved as paths (not
/// decided here) because this proc macro cannot see which features of
/// `factories_actor` are enabled - the feature-gated aliases over there can.
fn type_or_default(explicit: Option<Type>, default: TokenStream) -> TokenStream {
    match explicit {
        Some(ty) => ty.into_token_stream(),
        None => default,
    }
}

pub fn derive_actor(input: DeriveInput) -> TokenStream {
    if !input.generics.params.is_empty() {
        proc_macro_error::emit_error!(
            input.generics.span(),
            "#[derive(Actor)] does not support generic actors: actor RTTI is a `static`, \
             which generic contexts share across all instantiations, breaking per-type identity"
        );
    }

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
                set_type(&mut config.channel, &meta)
            } else if meta.path.is_ident("error") {
                set_type(&mut config.error, &meta)
            } else if meta.path.is_ident("binder") {
                set_type(&mut config.binder, &meta)
            } else if meta.path.is_ident("lock") {
                set_type(&mut config.lock, &meta)
            } else if meta.path.is_ident("run_loop") {
                set_type(&mut config.run_loop, &meta)
            } else if meta.path.is_ident("name") {
                let value = meta.value()?.parse()?;

                if config.name.is_some() {
                    proc_macro_error::emit_error!(meta.path.span(), "duplicate key");
                } else {
                    config.name = Some(value);
                }

                Ok(())
            } else {
                Err(meta.error(
                    "unknown key, expected one of \
                     `channel`, `error`, `binder`, `lock`, `run_loop`, `name`",
                ))
            }
        });

        if let Err(error) = result {
            for error in error {
                proc_macro_error::emit_error!(error.span(), "{}", error);
            }
        }
    }

    let ident = &input.ident;
    let defaults = quote!(::factories_actor::runtime::defaults);

    let channel = type_or_default(config.channel, quote!(#defaults::DefaultChannel));
    let error = type_or_default(config.error, quote!(#defaults::DefaultError));
    let binder = type_or_default(config.binder, quote!(#defaults::DefaultRuntimeBinder<Self>));
    let lock = type_or_default(config.lock, quote!(#defaults::DefaultLockStrategy<Self>));
    let run_loop = type_or_default(config.run_loop, quote!(#defaults::DefaultRunLoop<Self>));

    let rtti_name = match &config.name {
        Some(name) => name.to_token_stream(),
        None => quote!(::core::stringify!(#ident)),
    };

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

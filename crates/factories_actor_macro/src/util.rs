//! Shared helpers for the derive macros: diagnostics-first attribute parsing
//! that recovers where possible, so every problem surfaces in one compiler
//! pass.

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::meta::ParseNestedMeta;
use syn::parse::Parse;
use syn::spanned::Spanned;
use syn::{DeriveInput, Ident, LitStr, Result};

/// Parse `= <value>` into the slot.
///
/// Duplicates are emitted instead of returned so parsing recovers: the value
/// is consumed either way, keeping the cursor aligned for the keys that
/// follow.
pub fn set_value<T: Parse>(slot: &mut Option<T>, meta: &ParseNestedMeta) -> Result<()> {
    let value = meta.value()?.parse()?;

    if slot.is_some() {
        proc_macro_error::emit_error!(meta.path.span(), "duplicate key");
    } else {
        *slot = Some(value);
    }

    Ok(())
}

/// Emit every individual error in a (possibly combined) `syn::Error`.
pub fn emit_syn_error(error: syn::Error) {
    for error in error {
        proc_macro_error::emit_error!(error.span(), "{}", error);
    }
}

/// Reject generic types up front: the generated RTTI is a `static`, which
/// generic contexts share across all instantiations, breaking per-type
/// identity. Emits (instead of aborting) so the remaining checks still run.
pub fn reject_generics(input: &DeriveInput, derive_name: &str) {
    if !input.generics.params.is_empty() {
        proc_macro_error::emit_error!(
            input.generics.span(),
            "#[derive({})] does not support generic types: the generated RTTI is a `static`, \
             which generic contexts share across all instantiations, breaking per-type identity",
            derive_name
        );
    }
}

/// Emit the explicitly configured value, or the given default tokens.
///
/// For the feature-dependent defaults this is always a path into
/// `factories_actor::runtime::defaults`: the defaults are resolved as paths
/// (not decided here) because a proc macro cannot see which features of
/// `factories_actor` are enabled - the feature-gated aliases over there can.
pub fn value_or_default(explicit: Option<impl ToTokens>, default: TokenStream) -> TokenStream {
    match explicit {
        Some(value) => value.into_token_stream(),
        None => default,
    }
}

/// The RTTI debug name: the configured override, or the stringified type name.
pub fn rtti_name(explicit: Option<LitStr>, ident: &Ident) -> TokenStream {
    match explicit {
        Some(name) => name.into_token_stream(),
        None => quote!(::core::stringify!(#ident)),
    }
}

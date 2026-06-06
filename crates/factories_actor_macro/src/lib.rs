//! Proc macros for `factories_actor`.
//!
//! Don't depend on this crate directly - enable the `derive` feature of
//! `factories_actor` and use the re-exports instead.

mod actor;
mod message;
mod util;

/// Derive [`Actor`] for a type, including its RTTI declaration.
///
/// Every associated type of the `Actor` trait can be configured through the
/// `#[actor(...)]` attribute; omitted keys fall back to the type aliases in
/// `factories_actor::runtime::defaults` (some of which are feature-gated, see
/// the module documentation there).
///
/// | key        | associated type | default                    |
/// |------------|-----------------|----------------------------|
/// | `channel`  | `Channel`       | `DefaultChannel`           |
/// | `error`    | `Error`         | `DefaultError`             |
/// | `binder`   | `RuntimeBinder` | `DefaultRuntimeBinder<Self>` |
/// | `lock`     | `LockStrategy`  | `DefaultLockStrategy<Self>` |
/// | `run_loop` | `RunLoop`       | `DefaultRunLoop<Self>`     |
///
/// Additionally `name = "..."` overrides the debug name baked into the RTTI
/// (defaults to the stringified type name).
///
/// ```ignore
/// #[derive(Actor)]
/// #[actor(
///     error = InitError,
///     lock = TokioRwLock<Self>,
/// )]
/// struct Mirror;
/// ```
///
/// Generic actors are rejected: actor RTTI is a `static`, which generic
/// contexts share across all instantiations, breaking per-type identity.
#[proc_macro_error::proc_macro_error]
#[proc_macro_derive(Actor, attributes(actor))]
pub fn derive_actor(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    actor::derive_actor(syn::parse_macro_input!(input as syn::DeriveInput)).into()
}

/// Derive `Message` for a type, including its RTTI declaration.
///
/// Configured through the `#[message(...)]` attribute:
///
/// | key      | meaning           | default                |
/// |----------|-------------------|------------------------|
/// | `answer` | `Message::Answer` | `()` (tell-style)      |
/// | `name`   | RTTI debug name   | stringified type name  |
///
/// ```ignore
/// #[derive(Message)]
/// #[message(answer = u32)]
/// struct GetValue;
/// ```
///
/// The generated `unsafe impl Message` is sound by construction: the RTTI and
/// the implementation are emitted for the same type in one expansion.
///
/// Generic messages are rejected: message RTTI is a `static`, which generic
/// contexts share across all instantiations, breaking per-type identity.
#[proc_macro_error::proc_macro_error]
#[proc_macro_derive(Message, attributes(message))]
pub fn derive_message(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    message::derive_message(syn::parse_macro_input!(input as syn::DeriveInput)).into()
}

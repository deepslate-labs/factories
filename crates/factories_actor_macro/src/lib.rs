//! Proc macros for `factories_actor`.
//!
//! Don't depend on this crate directly - enable the `derive` feature of
//! `factories_actor` and use the re-exports instead.

mod actor;
mod message;
mod messages;
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

/// Method-style message handlers on an inherent impl block.
///
/// Every method marked `#[handler]` additionally becomes a message handler.
/// The macro is additive: the impl block is re-emitted unchanged (markers
/// stripped), so handler methods stay plain, directly callable methods.
///
/// For each handler the macro generates:
///
/// - a message struct named after the method (`do_something` -> `DoSomething`)
///   with one public field per parameter, `Answer` = return type and the
///   method's visibility - unless `#[handler(message = Existing)]` reuses an
///   existing message, in which case the parameter names select the fields of
///   that message to decompose (checked by the type system, extra fields are
///   ignored).
/// - a `MessageHandler` impl that calls the method through the actor guard.
///   The receiver picks the access mode: `&self` -> `Shared`, `&mut self` ->
///   `Exclusive`. `async fn` is awaited. A non-`()` return value is sent as
///   the answer if one was requested.
/// - a dynamic-dispatch registration (expands to nothing when the
///   `dynamic-dispatch` feature of `factories_actor` is disabled, mirroring
///   the default runtime binder).
///
/// ```ignore
/// #[factories_actor::messages]
/// impl Calc {
///     #[handler]
///     fn add_value(&mut self, value: u32) {
///         self.value += value;
///     }
///
///     #[handler(message = SetConfig)]
///     fn set_config(&mut self, name: String, value: u32) { /* ... */ }
/// }
/// ```
#[proc_macro_error::proc_macro_error]
#[proc_macro_attribute]
pub fn messages(
    attrs: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    messages::messages(attrs.into(), syn::parse_macro_input!(input as syn::ItemImpl)).into()
}

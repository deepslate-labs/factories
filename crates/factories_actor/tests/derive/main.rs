#![cfg(all(
    feature = "derive",
    feature = "dynamic-dispatch",
    feature = "kanal-runtime",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

//! Macro-layer tests. The derives must produce exactly the hand-written
//! shapes in `spawn.rs` and `dynamic_dispatch.rs` - nothing here may rely on
//! a capability that those tests don't spell out manually.
//!
//! Organized by feature area; fixtures live in the module that tests them and
//! are shared across modules where the scenarios overlap:
//!
//! - [`actor`]: `#[derive(Actor)]` - defaults, overrides, RTTI names
//! - [`message`]: `#[derive(Message)]` - answer types, RTTI names
//! - [`handlers`]: `#[messages]` basics - generation, decomposition,
//!   dynamic registration
//! - [`markers`]: parameter markers - `#[answer]`, `#[message]`, `#[envelope]`
//! - [`event_source`]: `#[event_source]` - the derive's autoref-detected driver
//! - [`failure`]: actor failure - `die_on_err` modes, `#[context]` fail
//! - [`template`]: `ActorTemplate` bundles

mod actor;
mod event_source;
mod failure;
mod handlers;
mod lifecycle;
mod markers;
mod message;
mod template;
mod util;

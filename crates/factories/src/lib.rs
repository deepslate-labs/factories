#![no_std]
#![cfg_attr(docsrs, feature(doc_cfg))]
//! `factories` is an actor framework for Rust built on one rule: **everything is
//! explicit, and every convenience decomposes into public primitives**. There is
//! no hidden runtime and no magic global - an actor is an ordinary type, its
//! handlers are ordinary methods, and the machinery that carries messages to it
//! is assembled from parts you can see and replace.
//!
//! # A first actor
//!
//! ```no_run
//! use factories::prelude::*;
//!
//! #[derive(Actor)]
//! struct Counter {
//!     value: u64,
//! }
//!
//! #[factories::messages]
//! impl Counter {
//!     /// Fire-and-forget: `Inc` with answer `()`.
//!     #[handler]
//!     fn inc(&mut self) {
//!         self.value += 1;
//!     }
//!
//!     /// Request/response: `Get` with answer `u64`.
//!     #[handler]
//!     async fn get(&self) -> u64 {
//!         self.value
//!     }
//! }
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let counter = ActorLauncher::default()
//!     .spawn_ready(&TokioTaskSpawner::current(), Counter { value: 0 })
//!     .await?;
//!
//! counter.inc().tell().await?;
//! assert_eq!(counter.get().await?, 1);
//! # Ok(())
//! # }
//! ```
//!
//! # How the crate is organized
//!
//! The surface mirrors the three layers of the framework, from the contract an
//! actor must satisfy down to the concrete parts you spawn it with:
//!
//! - [`actor`] - the core contract: the [`Actor`](actor::Actor) trait, message
//!   handlers, handles, lifecycle and supervision. *What it means to be an actor.*
//! - [`message`] - message types, envelopes, and the answer channel.
//! - [`spawn`] - the assembly contracts: how a running actor is constructed
//!   (the [`ActorLauncher`](spawn::ActorLauncher) builder, channels, spawners).
//! - [`runtime`] - the concrete, feature-gated parts you pick: the tokio
//!   channel and task spawner, lock strategies, run loops.
//!
//! Most code only needs the [`prelude`]. Reach into the modules above when you
//! configure an actor explicitly (`#[actor(run_loop = …, lock = …)]`) or assemble
//! one by hand.
//!
//! # Features
//!
//! `default = ["derive", "full-runtime"]`. Pare them back for `no_std` or to drop
//! tokio; see the crate's `Cargo.toml` for the full list. The framework core is
//! `no_std`; the tokio runtime is one (default) choice of parts, not a requirement.

// The full tiered surface, re-exported 1:1 from the implementation crate. These
// are the lego bricks - combine the pre-defined parts, or craft your own.
pub use factories_actor::{actor, message, runtime, spawn};

/// Method-style message handlers: marks an inherent impl block so every
/// `#[handler]` method additionally becomes a message handler. See
/// [`messages`](macro@messages) for the full attribute reference.
#[cfg(feature = "derive")]
pub use factories_actor::messages;

/// Declare an actor protocol: a trait whose methods name messages, plus a
/// concrete erased handle guaranteeing those messages bind. See
/// [`protocol`](macro@protocol) for details.
#[cfg(feature = "derive")]
pub use factories_actor::protocol;

pub mod prelude {
    //! The common surface for writing and spawning actors.
    //!
    //! `use factories::prelude::*;` brings the derive-first essentials into
    //! scope: the [`Actor`] and [`Message`] traits (and their derives), the
    //! `#[messages]` / `#[protocol]` attributes, handles, lifecycle and
    //! supervision types, and - with the tokio runtime - the launcher and task
    //! spawner.

    #[cfg(feature = "derive")]
    pub use crate::{messages, protocol};

    pub use crate::actor::handle::{
        AnyActorHandle, AnyLocalActorHandle, TypedActorHandle, WeakActorHandle,
    };
    pub use crate::actor::lifecycle::{StopReason, TerminationKind, TerminationReason};
    pub use crate::actor::supervision::{ActorId, Terminated};
    pub use crate::actor::{Actor, ActorContext};
    pub use crate::message::Message;
    pub use crate::spawn::ActorLauncher;

    #[cfg(feature = "tokio-answer")]
    pub use crate::message::channel::{AnswerReceiver, AnswerSender};

    #[cfg(feature = "tokio-runtime")]
    pub use crate::runtime::tokio::TokioTaskSpawner;
}

// Macro support: re-exported so the generated code (which refers back
// through `::factories::…`) and the hand-written declaration macros resolve
// through this one crate. Not public API.

#[doc(hidden)]
pub use factories_actor::factories_rtti;

#[cfg(feature = "dynamic-dispatch")]
#[doc(hidden)]
pub use factories_actor::factories_collect;

// The `#[macro_export]` declaration macros live at the implementation crate's
// root; re-export them here so `::factories::name!` resolves.
#[doc(hidden)]
pub use factories_actor::{
    declare_actor_rtti, declare_message, declare_message_rtti,
    declare_static_async_dispatcher, implement_message_handler,
    register_dynamic_handler_if_enabled, typed_handle_methods_if_enabled,
};

#[cfg(feature = "dynamic-dispatch")]
#[doc(hidden)]
pub use factories_actor::register_dynamic_handler;

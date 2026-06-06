//! Assembly contracts: what generic construction of a running actor requires.
//!
//! Everything in this module is opt-in. Custom-crafted actors may ignore it
//! entirely and assemble from the primitives in [`crate::actor`] - the builder
//! does nothing that cannot be done by hand with public API.

mod builder;
mod channel;
mod run_loop;
mod task;

pub use builder::*;
pub use channel::*;
pub use run_loop::*;
pub use task::*;

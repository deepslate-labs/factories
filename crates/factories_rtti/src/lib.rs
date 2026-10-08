#![no_std]

extern crate alloc;

// Public so the declaration macros can reference items via `$crate::rtti::...`
// from foreign crates; the wildcard re-exports below stay the primary surface.
pub mod autoref_check;
pub mod rtti;

pub use autoref_check::*;
pub use rtti::*;

pub use factories_types_macro::sequential_trait;

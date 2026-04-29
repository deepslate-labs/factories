#![no_std]

extern crate alloc;

mod rtti;
mod autoref_check;

pub use rtti::*;
pub use autoref_check::*;

pub use factories_types_macro::sequential_trait;

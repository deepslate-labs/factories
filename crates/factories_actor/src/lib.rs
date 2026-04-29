#![no_std]

extern crate alloc;

pub mod message;
mod actor;

#[allow(unused)] // Used by macros, not sure why this is not detected...
pub(crate) use factories_rtti;

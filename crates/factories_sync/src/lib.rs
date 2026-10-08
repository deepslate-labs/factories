//! Locking primitives that adapt to the platform factories runs on.
//!
//! Factories mostly runs on std targets, where fine-grained OS locks are
//! cheap. However, the core crates stay `no_std` capable, where
//! [`critical-section`](https://docs.rs/critical-section) is the portable
//! primitive. This crate abstracts over both behind one closure-scoped API so
//! the rest of the factories ecosystem never picks a backend directly:
//!
//! - `std` feature: locking is based on std primitives and as fine grained as
//!   the std locks on that platform.
//! - `critical-section` feature (default): locking enters the global critical
//!   section and the final binary supplies the implementation as usual.
//!
//! `std` wins when both features are enabled.
//!
//! Factories does not employ poisoning: Leaving a sync primitive with a panic
//! is the same as unlocking it normally. Poisoning is often complex and even
//! fairly often discussed in the Rust std implementation
//! (<https://github.com/rust-lang/rust/issues/149359>), as such factories makes no
//! attempt to guarantee poisoning and instead opts to release locks fully on
//! unwind.

#![no_std]

#[cfg(feature = "std")]
extern crate std;

#[cfg(not(any(feature = "std", feature = "critical-section")))]
compile_error!(
    "factories_sync needs a locking backend: enable the `std` or `critical-section` feature"
);

mod mutex;
mod once;
mod raw;

pub use mutex::Mutex;
pub use once::OnceCell;
#[cfg(feature = "critical-section")]
pub use raw::CriticalSectionRawMutex;
#[cfg(feature = "std")]
pub use raw::StdRawMutex;
pub use raw::{DefaultRawMutex, RawScopedMutex};

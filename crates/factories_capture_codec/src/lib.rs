//! Sans-I/O codec for the factories capture log.
//!
//! A pure, dependency-free state machine that turns observation events into a
//! compact, append-only byte stream and back. It performs **no I/O** and reads
//! **no clock**: callers feed it tick values and byte slices, and it yields
//! bytes and decoded records. All actual writing, threading, and clock-reading
//! lives in the layer above (the reference sink), which keeps this crate
//! trivially testable, runtime-agnostic, and independent of the actor
//! framework it currently serves.
//!
//! The stream is a sequence of self-contained *segments* (one per producer
//! buffer), each carrying its own string table and timestamp anchor, so a
//! segment decodes standalone with no cross-segment state.
#![no_std]

extern crate alloc;

pub mod error;
pub mod segment;
pub mod stream;
pub mod varint;

pub use error::DecodeError;

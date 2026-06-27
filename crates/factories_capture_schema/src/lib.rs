//! Self-describing capture schema: how a message (or any value) describes its
//! own structure and emits display-oriented values for the capture log.
//!
//! A value implements [`CaptureSchema`] and, when captured, *pushes* itself into
//! a [`FieldVisitor`] (the encoder, or a test recorder) — primitives emit a leaf
//! kind, structs frame their fields, containers recurse. Because each field is
//! pushed only if the [`CaptureConfig`] admits it, the schema *emerges* from
//! what a type chooses to reveal at a given verbosity rather than being a fixed
//! description. Each field can carry an [`Interpretation`] — an open `(name,
//! params)` hint (e.g. `audio { rate, channels }`) the viewer renders.
//!
//! This crate is the type→description *mechanism* only: it reads no clock, does
//! no I/O, and knows nothing of the wire format (a later layer drives a concrete
//! [`FieldVisitor`] that encodes into the capture codec).
#![no_std]

extern crate alloc;

pub mod interpretation;
pub mod schema;

pub use interpretation::{Interpretation, Scalar};
pub use schema::{CaptureConfig, CaptureSchema, FieldVisitor};

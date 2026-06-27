//! Per-field interpretation hints: how a captured value should be displayed.
//!
//! Open by design — a `name` plus scalar `params` the viewer interprets. The
//! vocabulary is a contract between message authors and the viewer (Corvidae),
//! not something this crate enumerates, so messages attach whatever they like
//! and the viewer renders names it knows (falling back to a generic view).

/// A scalar carried in an interpretation's params.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Scalar {
    /// An unsigned integer.
    Uint(u64),
    /// A signed integer.
    Int(i64),
    /// A floating-point number.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A string constant.
    Str(&'static str),
}

/// A display/semantic hint attached to a field: a `name` (e.g. `"audio"`) plus
/// scalar `params` (e.g. `[("rate", 48000), ("channels", 2)]`).
///
/// Params borrow a (typically stack-built) slice, so attaching an interpretation
/// allocates nothing on the capture path; the visitor copies what it needs.
#[derive(Debug, Copy, Clone)]
pub struct Interpretation<'a> {
    /// The interpretation name; `""` means "no interpretation".
    pub name: &'static str,
    /// Scalar parameters, keyed by name.
    pub params: &'a [(&'static str, Scalar)],
}

impl Interpretation<'_> {
    /// No interpretation — a plain field.
    pub const NONE: Interpretation<'static> = Interpretation {
        name: "",
        params: &[],
    };

    /// Whether this is the absence of an interpretation.
    pub const fn is_none(&self) -> bool {
        self.name.is_empty()
    }
}

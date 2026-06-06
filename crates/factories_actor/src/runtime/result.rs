//! Result vocabulary for handler outcomes.

/// A type that can be viewed as a `Result`.
///
/// Used by the `die_on_err` handlers of the `#[messages]`.
#[diagnostic::on_unimplemented(
    message = "`die_on_err` requires a result-like return type, but `{Self}` is not",
    note = "handlers with `die_on_err` must return `Result<T, E>` (or a type implementing `ResultLike`)"
)]
pub trait ResultLike {
    /// The success value.
    type Ok;

    /// The error value.
    type Error;

    /// View the outcome by reference.
    fn as_result(&self) -> Result<&Self::Ok, &Self::Error>;

    /// Decompose into the outcome.
    fn into_result(self) -> Result<Self::Ok, Self::Error>;
}

impl<T, E> ResultLike for Result<T, E> {
    type Ok = T;
    type Error = E;

    fn as_result(&self) -> Result<&T, &E> {
        self.as_ref()
    }

    fn into_result(self) -> Result<T, E> {
        self
    }
}

use core::fmt::{Debug, Formatter};

#[doc(hidden)]
pub mod _imp {
    pub use factories_types_macro::match_specialize;
}

/// Perform compile-time specialization based on trait bounds using the auto-ref trick.
///
/// This macro selects a branch at compile time depending on which trait bounds a concrete type
/// satisfies. It works like a `match` over trait bounds: the first arm whose bounds are met wins.
/// A wildcard `_` arm can be used as a fallback. If no arm matches and no wildcard is present,
/// compilation fails.
///
/// The type to test must be known statically and cannot be a generic type parameter.
///
/// Returns an [`AutorefSpecialized<T>`] that lazily resolves the selected branch via a function
/// pointer.
///
/// # Syntax
///
/// ```text
/// autoref_specialize!(Type -> ReturnType {
///     Bound1 + Bound2 => expr,
///     T @ Bound3 => expr_using_T,
///     _ => fallback_expr,
/// })
/// ```
///
/// - **`Type -> ReturnType`**: The concrete type to test and the return type of each arm.
/// - **`Bound1 + Bound2 => expr`**: Matches if `Type: Bound1 + Bound2`. Arms are tried in order;
///   the first match wins.
/// - **`T @ Bound => expr`**: Binds the type as `T` inside the arm body, allowing generic usage
///   (e.g. `T` can be used in turbofish or trait method calls).
/// - **`Pat1 | Pat2 => expr`**: OR patterns - matches if any selector's bounds are satisfied.
///   All selectors in an OR pattern must use the same binding name.
/// - **`_ => expr`**: Wildcard fallback that matches any type. Must be the last arm.
///
/// # Examples
///
/// Check if a type is `Clone` and return its `CloneRtti`:
/// ```ignore
/// let specialized = autoref_specialize!(MyType -> Option<CloneRtti> {
///     T @ Clone => Some(CloneRtti::new::<T>()),
///     _ => None,
/// });
///
/// let result: Option<CloneRtti> = specialized.resolve();
/// ```
///
/// Multi-arm priority - first matching arm wins:
/// ```ignore
/// let specialized = autoref_specialize!(String -> &'static str {
///     Copy => "copy",
///     Clone => "clone but not copy",
///     _ => "neither",
/// });
///
/// assert_eq!(specialized.resolve(), "clone but not copy");
/// ```
#[macro_export]
macro_rules! autoref_specialize {
    ($($input:tt)*) => {
        $crate::AutorefSpecialized::new(
            $crate::_imp::match_specialize!($($input)*)
        )
    };
}

#[derive(Copy, Clone)]
pub struct AutorefSpecialized<T> {
    resolve: fn() -> T,
}

impl<T> AutorefSpecialized<T> {
    /// Create a new autoref specialized with the given resolver
    pub const fn new(resolve: fn() -> T) -> Self {
        Self { resolve }
    }

    /// Resolve the actual data.
    pub fn resolve(&self) -> T {
        (self.resolve)()
    }
}

impl<T> Debug for AutorefSpecialized<T> where T: Debug {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AutorefSpecialized")
            .field("resolver", &(self.resolve as *const ()))
            .field("value", &self.resolve())
            .finish()
    }
}

impl<T> PartialEq for AutorefSpecialized<T> where T: PartialEq {
    fn eq(&self, other: &Self) -> bool {
        self.resolve() == other.resolve()
    }
}

impl<T> Eq for AutorefSpecialized<T> where T: Eq {}

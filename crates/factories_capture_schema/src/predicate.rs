//! Field-gating combinators for `#[capture(if = …)]`.
//!
//! The derive emits `if (#expr)(config) { … }`, so the `if` value is anything
//! callable with a `&CaptureConfig` — a `|config| …` closure, a fn path, or a
//! composition of these combinators. They're the common vocabulary; nothing is
//! special-cased in the derive (`min_verbosity(2)` is just a combinator call).

use factories_extension::Extension;

use crate::schema::CaptureConfig;

/// Admit a field at or above verbosity `level`.
pub fn min_verbosity(level: u8) -> impl Fn(&CaptureConfig<'_>) -> bool {
    move |config| config.verbosity() >= level
}

/// Admit a field when the config carries the extension `ext`.
pub fn has<T: 'static>(ext: &'static Extension<T>) -> impl Fn(&CaptureConfig<'_>) -> bool {
    move |config| config.get(ext).is_some()
}

/// Admit when both `a` and `b` admit.
pub fn and<A, B>(a: A, b: B) -> impl Fn(&CaptureConfig<'_>) -> bool
where
    A: Fn(&CaptureConfig<'_>) -> bool,
    B: Fn(&CaptureConfig<'_>) -> bool,
{
    move |config| a(config) && b(config)
}

/// Admit when either `a` or `b` admits.
pub fn or<A, B>(a: A, b: B) -> impl Fn(&CaptureConfig<'_>) -> bool
where
    A: Fn(&CaptureConfig<'_>) -> bool,
    B: Fn(&CaptureConfig<'_>) -> bool,
{
    move |config| a(config) || b(config)
}

/// Admit when `a` does not.
pub fn not<A>(a: A) -> impl Fn(&CaptureConfig<'_>) -> bool
where
    A: Fn(&CaptureConfig<'_>) -> bool,
{
    move |config| !a(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use factories_extension::{ExtensionSet, declare_extension};

    #[test]
    fn min_verbosity_gates_on_the_dial() {
        let config = CaptureConfig::new(2);
        assert!(min_verbosity(1)(&config));
        assert!(min_verbosity(2)(&config));
        assert!(!min_verbosity(3)(&config));
    }

    #[test]
    fn combinators_compose() {
        let config = CaptureConfig::new(2);
        assert!(and(min_verbosity(1), min_verbosity(2))(&config));
        assert!(!and(min_verbosity(1), min_verbosity(3))(&config));
        assert!(or(min_verbosity(3), min_verbosity(1))(&config));
        assert!(not(min_verbosity(3))(&config));
    }

    #[test]
    fn has_checks_the_extension_set() {
        declare_extension!(POLICY: u32, local);
        declare_extension!(ABSENT: u32, local);

        let mut set = ExtensionSet::new();
        set.insert(POLICY, 7u32);
        let config = CaptureConfig::with_extensions(1, &set);

        assert!(has(POLICY)(&config));
        assert!(!has(ABSENT)(&config));
    }
}

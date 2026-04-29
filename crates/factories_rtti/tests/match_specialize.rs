use factories_types_macro::match_specialize;

#[test]
fn single_arm_matches() {
    let resolver = match_specialize!(String -> &'static str {
        Clone => "is clone",
    });
    assert_eq!(resolver(), "is clone");
}

#[test]
fn single_arm_with_wildcard_fallback() {
    // i32 is Clone, so the first arm matches
    let resolver = match_specialize!(i32 -> &'static str {
        Clone => "clone",
        _ => "fallback",
    });
    assert_eq!(resolver(), "clone");
}

struct NotClone;

#[test]
fn wildcard_catches_non_matching() {
    let resolver = match_specialize!(NotClone -> &'static str {
        Clone => "clone",
        _ => "fallback",
    });
    assert_eq!(resolver(), "fallback");
}

#[test]
fn multiple_arms_priority() {
    // String is both Clone and Display - first arm should win
    let resolver = match_specialize!(String -> u32 {
        Clone => 1,
        core::fmt::Display => 2,
        _ => 0,
    });
    assert_eq!(resolver(), 1);
}

#[test]
fn second_arm_matches_when_first_doesnt() {
    // A type that is Display but not Copy
    let resolver = match_specialize!(String -> u32 {
        Copy => 1,
        core::fmt::Display => 2,
        _ => 0,
    });
    assert_eq!(resolver(), 2);
}

#[test]
fn or_patterns() {
    // String is Clone, so the first selector matches
    let resolver = match_specialize!(String -> &'static str {
        Clone | Copy => "clone or copy",
        _ => "neither",
    });
    assert_eq!(resolver(), "clone or copy");

    // i32 is Copy, so the second selector matches
    let resolver = match_specialize!(i32 -> &'static str {
        Clone | Copy => "clone or copy",
        _ => "neither",
    });
    assert_eq!(resolver(), "clone or copy");
}

#[test]
fn type_variable_binding() {
    use core::any::type_name;

    let resolver = match_specialize!(String -> &'static str {
        T @ Clone => type_name::<T>(),
        _ => "not clone",
    });
    // The binding T should resolve to String
    assert_eq!(resolver(), type_name::<String>());
}

#[test]
fn wildcard_only() {
    let resolver = match_specialize!(String -> u32 {
        _ => 42,
    });
    assert_eq!(resolver(), 42);
}

#[test]
fn unsized_type() {
    let resolver = match_specialize!(str -> &'static str {
        core::fmt::Display => "display",
        _ => "not display",
    });
    assert_eq!(resolver(), "display");
}

#[test]
fn unsized_type_wildcard() {
    // [u8] is not Clone
    let resolver = match_specialize!([u8] -> &'static str {
        Clone => "clone",
        _ => "fallback",
    });
    assert_eq!(resolver(), "fallback");
}

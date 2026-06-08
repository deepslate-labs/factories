//! Inline value-carrying form of `match_specialize!`: selection returns a
//! *specialized* type per arm (no single erased return type), with an in-scope
//! `&mut` value handed to each arm. Mirrors the event-source driver pattern.

use factories_types_macro::match_specialize;

trait HasDriver {
    type Driver: Driver;
    fn into_driver(&mut self) -> Self::Driver;
}

trait Driver {
    fn tick(&mut self) -> u64;
}

struct NoDriver;
impl Driver for NoDriver {
    fn tick(&mut self) -> u64 {
        0
    }
}

// Two concrete actors.
struct Eventful {
    seen: u64,
}
impl HasDriver for Eventful {
    type Driver = TickDriver;
    fn into_driver(&mut self) -> TickDriver {
        self.seen += 1;
        TickDriver { n: 0 }
    }
}
struct Plain;

struct TickDriver {
    n: u64,
}
impl Driver for TickDriver {
    fn tick(&mut self) -> u64 {
        self.n += 1;
        self.n
    }
}

// Concrete per-actor selection sites - what a derive would emit.
fn select_eventful(actor: &mut Eventful) -> impl Driver + use<> {
    match_specialize!(actor: &mut Eventful {
        T @ HasDriver : T::Driver => actor.into_driver(),
        _             : NoDriver  => NoDriver,
    })
}
fn select_plain(actor: &mut Plain) -> impl Driver + use<> {
    match_specialize!(actor: &mut Plain {
        T @ HasDriver : T::Driver => actor.into_driver(),
        _             : NoDriver  => NoDriver,
    })
}

#[test]
fn present_arm_returns_specialized_driver() {
    let mut e = Eventful { seen: 0 };
    let mut d = select_eventful(&mut e);
    // into_driver ran (present arm), and the concrete TickDriver was returned.
    assert_eq!(e.seen, 1);
    assert_eq!(d.tick(), 1);
    assert_eq!(d.tick(), 2);
}

#[test]
fn absent_arm_falls_through_to_wildcard() {
    let mut p = Plain;
    let mut d = select_plain(&mut p);
    // Plain has no HasDriver impl -> wildcard -> NoDriver.
    assert_eq!(d.tick(), 0);
}

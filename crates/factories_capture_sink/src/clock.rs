//! The per-event clock the sink stamps with.
//!
//! Selection is decided once, at sink construction, by [`Clock::detect`]:
//!
//! 1. x86-64 with an invariant TSC → `rdtsc`,
//! 2. AArch64 → `cntvct_el0`,
//! 3. anything else, or a non-invariant TSC → portable `CLOCK_MONOTONIC`.
//!
//! The architecture-specific counters are compiled only with the `arch-clock`
//! feature; without it, only the monotonic path exists. No frequency calibration
//! is needed: every segment records open/close clock readings, and the reader
//! interpolates event times between them (see `factories_capture_codec`).
//!
//! Timestamps here are display-only - ordering and causality come from explicit
//! event links, never from these ticks - which is why a cheap, slightly skewable
//! counter like `rdtsc` is acceptable.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use factories_capture_codec::segment::ClockMode;

/// A monotonic tick source plus the anchors needed to map ticks to wall-clock.
pub struct Clock {
    mode: ClockMode,
    /// Reference instant for the monotonic axis (and the tick source in
    /// `Monotonic` mode).
    epoch: Instant,
}

impl Clock {
    /// The portable clock: `CLOCK_MONOTONIC`, ticks in nanoseconds. Always
    /// available, every platform.
    pub fn monotonic() -> Self {
        Self {
            mode: ClockMode::Monotonic,
            epoch: Instant::now(),
        }
    }

    /// Pick the best clock this machine offers (see the module docs).
    pub fn detect() -> Self {
        let epoch = Instant::now();

        // An invariant TSC ticks at a constant rate, so a segment's open/close
        // readings interpolate cleanly; a non-invariant one would drift within a
        // segment, so fall back to the monotonic clock there.
        #[cfg(all(feature = "arch-clock", target_arch = "x86_64"))]
        if x86::invariant_tsc() {
            return Self {
                mode: ClockMode::Tsc,
                epoch,
            };
        }

        #[cfg(all(feature = "arch-clock", target_arch = "aarch64"))]
        {
            // The generic timer is architecturally invariant.
            return Self {
                mode: ClockMode::Cntvct,
                epoch,
            };
        }

        Self {
            mode: ClockMode::Monotonic,
            epoch,
        }
    }

    /// Which clock this is - written into the segment header.
    pub fn mode(&self) -> ClockMode {
        self.mode
    }

    /// The current tick. Cheap and called per event on the actor loop thread.
    pub fn now(&self) -> u64 {
        match self.mode {
            #[cfg(all(feature = "arch-clock", target_arch = "x86_64"))]
            ClockMode::Tsc => x86::rdtsc(),
            #[cfg(all(feature = "arch-clock", target_arch = "aarch64"))]
            ClockMode::Cntvct => arm::cntvct(),
            // Monotonic (and any mode whose counter wasn't compiled in, which
            // `detect` therefore never selects) reads the nanosecond clock.
            _ => self.epoch.elapsed().as_nanos() as u64,
        }
    }

    /// Read the segment-open anchors together: `(unix_micros, mono_micros, tick)`.
    /// Called once per segment, off the per-event path.
    pub fn anchors(&self) -> (u64, u64, u64) {
        let unix_micros = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0);
        let mono_micros = self.epoch.elapsed().as_micros() as u64;
        let tick = self.now();
        (unix_micros, mono_micros, tick)
    }
}

#[cfg(all(feature = "arch-clock", target_arch = "x86_64"))]
mod x86 {
    /// Read the timestamp counter.
    #[inline]
    pub fn rdtsc() -> u64 {
        // SAFETY: `rdtsc` is unconditionally available on x86-64 and has no
        // preconditions or side effects - it only reads the counter register.
        unsafe { core::arch::x86_64::_rdtsc() }
    }

    /// Whether the CPU advertises an invariant TSC (`CPUID.80000007H:EDX[8]`),
    /// i.e. a counter that ticks at a constant rate across frequency scaling and
    /// is synchronized across cores.
    pub fn invariant_tsc() -> bool {
        // 0x8000_0007 is a standard extended leaf, callable on any x86-64 CPU;
        // an unsupported leaf reads back as zero, i.e. "not invariant".
        let leaf = core::arch::x86_64::__cpuid(0x8000_0007);
        leaf.edx & (1 << 8) != 0
    }
}

#[cfg(all(feature = "arch-clock", target_arch = "aarch64"))]
mod arm {
    /// Read the generic timer counter (`cntvct_el0`).
    #[inline]
    pub fn cntvct() -> u64 {
        let value: u64;
        // SAFETY: `cntvct_el0` is readable from EL0 on Linux (the kernel sets
        // `CNTKCTL_EL1.EL0VCTEN`); the read has no memory effects or side effects.
        unsafe {
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) value, options(nomem, nostack));
        }
        value
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_clock_increases_and_anchors_are_plausible() {
        let clock = Clock::monotonic();
        assert_eq!(clock.mode(), ClockMode::Monotonic);

        let a = clock.now();
        let b = clock.now();
        assert!(b >= a, "ticks are non-decreasing: {a} then {b}");

        let (unix_micros, _mono_micros, tick) = clock.anchors();
        assert!(
            unix_micros > 1_700_000_000_000_000,
            "wall-clock anchor is a plausible post-2023 micros value: {unix_micros}"
        );
        assert!(clock.now() >= tick, "tick anchor precedes a later reading");
    }

    #[test]
    fn detected_clock_is_monotonic() {
        let clock = Clock::detect();
        let a = clock.now();
        let b = clock.now();
        assert!(b >= a, "detected clock is non-decreasing: {a} then {b}");

        let (unix_micros, _mono, _tick) = clock.anchors();
        assert!(
            unix_micros > 1_700_000_000_000_000,
            "wall-clock anchor is plausible: {unix_micros}"
        );
    }
}

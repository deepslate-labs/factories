//! The default access-mode vocabulary: [`Exclusive`] and [`Shared`].
//!
//! This is one possible vocabulary built on the core
//! [`AccessMode`]/[`LockStrategy`] contract, not part of it - custom locking
//! schemes are free to define their own mode types (e.g. distinguishing
//! read-only-exclusive from read-write-exclusive) at equal standing.
//!
//! A single mode type cannot have per-strategy [`AccessMode`] impls
//! (coherence), so the modes here route through the [`ExclusiveLockStrategy`]
//! and [`SharedLockStrategy`] capability traits: strategies opt in by
//! implementing them, and handlers using [`Exclusive`]/[`Shared`] stay
//! portable across all such strategies.

use crate::actor::{AccessMode, Actor, LockStrategy};
use core::ops::{Deref, DerefMut};

/// Lock strategy capability: exclusive (mutable) access to the actor state.
///
/// Implementing this plugs the strategy into the [`Exclusive`] access mode.
pub trait ExclusiveLockStrategy<A: Actor + ?Sized>: LockStrategy<A> {
    /// The guard granting mutable access to the actor state.
    type ExclusiveGuard<'a>: DerefMut<Target = A>
    where
        Self: 'a;

    /// Acquire exclusive access to the actor state.
    fn acquire_exclusive(&self) -> impl Future<Output = Self::ExclusiveGuard<'_>>;
}

/// Lock strategy capability: shared (read-only) access to the actor state.
///
/// Implementing this plugs the strategy into the [`Shared`] access mode.
pub trait SharedLockStrategy<A: Actor + ?Sized>: LockStrategy<A> {
    /// The guard granting shared access to the actor state.
    type SharedGuard<'a>: Deref<Target = A>
    where
        Self: 'a;

    /// Acquire shared access to the actor state.
    fn acquire_shared(&self) -> impl Future<Output = Self::SharedGuard<'_>>;
}

/// Access mode for handlers that mutate the actor state.
///
/// Available for every actor whose lock strategy implements
/// [`ExclusiveLockStrategy`].
#[derive(Debug, Default, Copy, Clone)]
pub struct Exclusive;

impl<A: Actor + ?Sized> AccessMode<A> for Exclusive
where
    A::LockStrategy: ExclusiveLockStrategy<A>,
{
    type Guard<'a>
        = <A::LockStrategy as ExclusiveLockStrategy<A>>::ExclusiveGuard<'a>
    where
        Self: 'a;

    fn acquire<'a>(lock_strategy: &'a A::LockStrategy) -> impl Future<Output = Self::Guard<'a>>
    where
        Self: 'a,
    {
        lock_strategy.acquire_exclusive()
    }
}

/// Access mode for handlers that only read the actor state.
///
/// Available for every actor whose lock strategy implements
/// [`SharedLockStrategy`]. Whether shared handlers actually run concurrently
/// is up to the strategy and the run loop.
#[derive(Debug, Default, Copy, Clone)]
pub struct Shared;

impl<A: Actor + ?Sized> AccessMode<A> for Shared
where
    A::LockStrategy: SharedLockStrategy<A>,
{
    type Guard<'a>
        = <A::LockStrategy as SharedLockStrategy<A>>::SharedGuard<'a>
    where
        Self: 'a;

    fn acquire<'a>(lock_strategy: &'a A::LockStrategy) -> impl Future<Output = Self::Guard<'a>>
    where
        Self: 'a,
    {
        lock_strategy.acquire_shared()
    }
}

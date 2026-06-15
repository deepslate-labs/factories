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

use crate::actor::{AccessMode, Actor, LockStrategy, SerializedDispatch};
use core::cell::UnsafeCell;
use core::fmt::{Debug, Formatter};
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

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

/// Lock strategy that elides synchronization, relying on the run loop for
/// exclusivity.
///
/// Usable only with run loops that implement
/// [`SerializedDispatch`]: dispatches never overlap, so the actor state needs
/// no lock. There is no waiting, no queueing and no waker bookkeeping -
/// acquisition is a single atomic flag transition.
///
/// The flag is not a lock but a violation detector: it is what keeps this type
/// safe to construct and share. Without it, two safe `acquire` calls outside
/// the run loop could alias exclusive access. Overlapping acquisition panics
/// instead of handing out aliased state; under a correctly implemented
/// [`SerializedDispatch`] loop it never fires.
pub struct UnguardedLock<A: ?Sized> {
    /// Violation detector: set while a guard is live.
    borrowed: AtomicBool,
    state: UnsafeCell<A>,
}

impl<A> UnguardedLock<A> {
    /// Create the lock strategy around the actor state.
    pub const fn new(state: A) -> Self {
        Self {
            borrowed: AtomicBool::new(false),
            state: UnsafeCell::new(state),
        }
    }

    /// Unwrap the actor state.
    pub fn into_inner(self) -> A {
        self.state.into_inner()
    }
}

impl<A: ?Sized> UnguardedLock<A> {
    /// Access the actor state through an exclusive borrow of the strategy.
    pub fn get_mut(&mut self) -> &mut A {
        self.state.get_mut()
    }

    /// Acquire the state, panicking on overlap.
    fn acquire(&self) -> UnguardedGuard<'_, A> {
        if self.borrowed.swap(true, Ordering::Acquire) {
            panic!(
                "overlapping acquisition of an UnguardedLock: the actor state was \
                 acquired while another guard was live. UnguardedLock requires all \
                 acquisitions to be serialized (see SerializedDispatch)."
            );
        }

        // SAFETY: The borrowed flag just transitioned false -> true, so no other
        //         guard is live and this exclusive reference is unique. The flag
        //         is reset when the returned guard drops.
        let state = unsafe { &mut *self.state.get() };

        UnguardedGuard {
            state,
            borrowed: &self.borrowed,
        }
    }
}

impl<A> From<A> for UnguardedLock<A> {
    fn from(state: A) -> Self {
        Self::new(state)
    }
}

impl<A: ?Sized> Debug for UnguardedLock<A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UnguardedLock")
            .field("borrowed", &self.borrowed)
            .finish_non_exhaustive()
    }
}

// SAFETY: Sending the lock sends the contained state.
unsafe impl<A: Send + ?Sized> Send for UnguardedLock<A> {}

// SAFETY: All shared-reference access to the state goes through `acquire`,
//         whose atomic borrowed flag guarantees mutual exclusion (panicking
//         instead of waiting) - the same argument that makes `Mutex<A>: Sync`
//         for `A: Send`.
unsafe impl<A: Send + ?Sized> Sync for UnguardedLock<A> {}

impl<A: Actor + ?Sized> LockStrategy<A> for UnguardedLock<A> {
    fn into_inner(self) -> A
    where
        A: Sized,
    {
        self.state.into_inner()
    }
}

impl<A: Actor + ?Sized> ExclusiveLockStrategy<A> for UnguardedLock<A>
where
    A::RunLoop: SerializedDispatch<A>,
{
    type ExclusiveGuard<'a> = UnguardedGuard<'a, A>;

    async fn acquire_exclusive(&self) -> Self::ExclusiveGuard<'_> {
        self.acquire()
    }
}

impl<A: Actor + ?Sized> SharedLockStrategy<A> for UnguardedLock<A>
where
    A::RunLoop: SerializedDispatch<A>,
{
    // Under serialized dispatch shared handlers cannot overlap anyway, so the
    // exclusive guard serves double duty.
    type SharedGuard<'a> = UnguardedGuard<'a, A>;

    async fn acquire_shared(&self) -> Self::SharedGuard<'_> {
        self.acquire()
    }
}

/// Guard of an [`UnguardedLock`].
pub struct UnguardedGuard<'a, A: ?Sized> {
    state: &'a mut A,
    borrowed: &'a AtomicBool,
}

impl<A: ?Sized> Deref for UnguardedGuard<'_, A> {
    type Target = A;

    fn deref(&self) -> &A {
        self.state
    }
}

impl<A: ?Sized> DerefMut for UnguardedGuard<'_, A> {
    fn deref_mut(&mut self) -> &mut A {
        self.state
    }
}

impl<A: Debug + ?Sized> Debug for UnguardedGuard<'_, A> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("UnguardedGuard").field(&self.state).finish()
    }
}

impl<A: ?Sized> Drop for UnguardedGuard<'_, A> {
    fn drop(&mut self) {
        self.borrowed.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::channel::{ActorChannel, ActorChannelSendResult, ActorChannelSendable};
    use crate::actor::dispatch::DispatchedActorMessage;
    use crate::actor::event::DefaultMailboxDriver;
    use crate::actor::handle::TypedActorHandle;
    use crate::actor::rtti::ActorRtti;
    use crate::actor::work::SendFutureConverter;
    use crate::actor::{ActorRunLoop, ActorRunLoopDispatchContext, StaticOnlyBinder};
    use futures::FutureExt;
    // Minimal actor whose run loop (unsafely) claims serialized dispatch. It is
    // never spawned; the tests only exercise the strategy directly.

    struct LockActor {
        value: u32,
    }

    struct LockActorChannel;

    impl ActorChannel for LockActorChannel {
        fn prepare_send(&self, _message: DispatchedActorMessage) -> impl ActorChannelSendable<'_> {
            LockActorSendable
        }
    }

    struct LockActorSendable;

    impl ActorChannelSendable<'_> for LockActorSendable {
        fn send(self) -> impl Future<Output = ActorChannelSendResult> + Send {
            async { unimplemented!("the lock test actor cannot be messaged") }
        }

        fn blocking_send(self) -> ActorChannelSendResult {
            unimplemented!("the lock test actor cannot be messaged")
        }

        fn try_send(self) -> ActorChannelSendResult {
            unimplemented!("the lock test actor cannot be messaged")
        }
    }

    struct LockActorLoop;

    impl ActorRunLoop<LockActor> for LockActorLoop {
        type DispatchContext = LockActorLoopContext;
        type WorkConverter = SendFutureConverter;
    }

    // SAFETY: This loop never dispatches anything at all - trivially serialized.
    unsafe impl SerializedDispatch<LockActor> for LockActorLoop {}

    struct LockActorLoopContext;

    impl ActorRunLoopDispatchContext<LockActor> for LockActorLoopContext {
        fn lock_strategy(&self) -> &UnguardedLock<LockActor> {
            unimplemented!("the lock test actor is never driven")
        }

        fn shared_state(&self) -> &crate::actor::state::SharedActorState<LockActor> {
            unimplemented!("the lock test actor is never driven")
        }

        fn self_ref(&self) -> &crate::actor::handle::WeakActorHandle<LockActor> {
            unimplemented!("the lock test actor is never driven")
        }
    }

    crate::declare_actor_rtti!(LOCK_ACTOR_RTTI, LockActor);

    // SAFETY: The RTTI is declared for exactly this type.
    unsafe impl Actor for LockActor {
        const RTTI: &'static ActorRtti = LOCK_ACTOR_RTTI;

        type Channel = LockActorChannel;
        type Error = ();
        type RuntimeBinder = StaticOnlyBinder;
        type LockStrategy = UnguardedLock<LockActor>;
        type RunLoop = LockActorLoop;
        type TypedHandle = TypedActorHandle<Self>;
        type SharedStateExtension = ();
        type EventDriver = DefaultMailboxDriver;
    }

    fn acquire(lock: &UnguardedLock<LockActor>) -> UnguardedGuard<'_, LockActor> {
        <UnguardedLock<LockActor> as ExclusiveLockStrategy<LockActor>>::acquire_exclusive(lock)
            .now_or_never()
            .expect("unguarded acquisition must be immediate")
    }

    #[test]
    fn sequential_acquisitions_share_state() {
        let lock = UnguardedLock::new(LockActor { value: 5 });

        let mut guard = acquire(&lock);
        guard.value += 1;
        drop(guard);

        let guard = acquire(&lock);
        assert_eq!(guard.value, 6);
        drop(guard);

        assert_eq!(lock.into_inner().value, 6);
    }

    #[test]
    #[should_panic(expected = "overlapping acquisition")]
    fn overlapping_acquisition_panics() {
        let lock = UnguardedLock::new(LockActor { value: 0 });

        let _live = acquire(&lock);
        let _overlap = acquire(&lock);
    }

    #[test]
    fn guard_drop_during_unwind_releases() {
        let lock = UnguardedLock::new(LockActor { value: 0 });

        let result = std::panic::catch_unwind(core::panic::AssertUnwindSafe(|| {
            let _guard = acquire(&lock);
            panic!("handler panic");
        }));
        assert!(result.is_err());

        // The flag was cleared during unwind, the state stays acquirable.
        let guard = acquire(&lock);
        assert_eq!(guard.value, 0);
    }
}

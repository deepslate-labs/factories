use crate::actor::{Actor, LockStrategy};
use crate::runtime::lock::{ExclusiveLockStrategy, SharedLockStrategy};

/// Lock strategy guarding the actor state with a [`tokio::sync::Mutex`].
///
/// Supports the [`Exclusive`](crate::runtime::lock::Exclusive) access mode.
/// For actors that also want concurrent read-only handlers, use
/// [`TokioRwLock`].
#[derive(Debug)]
pub struct TokioMutexLock<A: ?Sized> {
    mutex: tokio::sync::Mutex<A>,
}

impl<A> TokioMutexLock<A> {
    /// Create the lock strategy around the actor state.
    pub fn new(actor: A) -> Self {
        Self {
            mutex: tokio::sync::Mutex::new(actor),
        }
    }

    /// Unwrap the actor state.
    pub fn into_inner(self) -> A {
        self.mutex.into_inner()
    }
}

impl<A> From<A> for TokioMutexLock<A> {
    fn from(actor: A) -> Self {
        Self::new(actor)
    }
}

impl<A: Actor + ?Sized> LockStrategy<A> for TokioMutexLock<A> {}

impl<A: Actor + ?Sized> ExclusiveLockStrategy<A> for TokioMutexLock<A> {
    type ExclusiveGuard<'a> = tokio::sync::MutexGuard<'a, A>;

    fn acquire_exclusive(&self) -> impl Future<Output = Self::ExclusiveGuard<'_>> {
        self.mutex.lock()
    }
}

/// Lock strategy guarding the actor state with a [`tokio::sync::RwLock`].
///
/// Supports the [`Exclusive`](crate::runtime::lock::Exclusive) access mode
/// through write locking and the [`Shared`](crate::runtime::lock::Shared)
/// access mode through read locking, so read-only handlers can run
/// concurrently.
#[derive(Debug)]
pub struct TokioRwLock<A: ?Sized> {
    lock: tokio::sync::RwLock<A>,
}

impl<A> TokioRwLock<A> {
    /// Create the lock strategy around the actor state.
    pub fn new(actor: A) -> Self {
        Self {
            lock: tokio::sync::RwLock::new(actor),
        }
    }

    /// Unwrap the actor state.
    pub fn into_inner(self) -> A {
        self.lock.into_inner()
    }
}

impl<A> From<A> for TokioRwLock<A> {
    fn from(actor: A) -> Self {
        Self::new(actor)
    }
}

impl<A: Actor + ?Sized> LockStrategy<A> for TokioRwLock<A> {}

impl<A: Actor + ?Sized> ExclusiveLockStrategy<A> for TokioRwLock<A> {
    type ExclusiveGuard<'a> = tokio::sync::RwLockWriteGuard<'a, A>;

    fn acquire_exclusive(&self) -> impl Future<Output = Self::ExclusiveGuard<'_>> {
        self.lock.write()
    }
}

impl<A: Actor + ?Sized> SharedLockStrategy<A> for TokioRwLock<A> {
    type SharedGuard<'a> = tokio::sync::RwLockReadGuard<'a, A>;

    fn acquire_shared(&self) -> impl Future<Output = Self::SharedGuard<'_>> {
        self.lock.read()
    }
}

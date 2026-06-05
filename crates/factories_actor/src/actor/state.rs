use crate::actor::Actor;
use alloc::sync::Arc;
use core::fmt::{Debug, Formatter};
use once_cell::sync::OnceCell;
use crate::actor::task::ActorTaskHandle;

#[derive(Debug)]
struct InnerSharedActorState<A: Actor + ?Sized> {
    error: OnceCell<A::Error>,
    task: ActorTaskHandle,
}

impl<A: Actor + ?Sized> InnerSharedActorState<A> {
    fn new(task: ActorTaskHandle) -> Self {
        Self {
            error: OnceCell::new(),
            task,
        }
    }
}

/// State shared between the identity and run loop.
pub struct SharedActorState<A: Actor + ?Sized> {
    inner: Arc<InnerSharedActorState<A>>,
}

impl<A: Actor + ?Sized> SharedActorState<A> {
    pub fn new(task: ActorTaskHandle) -> Self {
        Self {
            inner: Arc::new(InnerSharedActorState::new(task)),
        }
    }

    /// Set the error this actor has failed with.
    pub fn set_error(&self, error: A::Error) -> Result<(), A::Error> {
        self.inner.error.set(error)
    }

    /// Get the error this actor has failed with, if any.
    pub fn get_error(&self) -> Option<&A::Error> {
        self.inner.error.get()
    }
}

impl<A: Actor + ?Sized> Debug for SharedActorState<A>
where
    Arc<InnerSharedActorState<A>>: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SharedActorState")
            .field("inner", &self.inner)
            .finish()
    }
}

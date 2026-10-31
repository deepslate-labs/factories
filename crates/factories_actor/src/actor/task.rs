use alloc::boxed::Box;
use core::ops::Deref;
use core::pin::Pin;

pub type WaitForTerminationFut = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Abstraction over the runtime task handle.
#[derive(Debug)]
pub struct ActorTaskHandle {
    handle: *mut core::ffi::c_void,
    abort: unsafe fn(*const *mut core::ffi::c_void),
    wait_for_termination: unsafe fn(*const *mut core::ffi::c_void) -> WaitForTerminationFut,
    drop: unsafe fn(*mut core::ffi::c_void),
}

// SAFETY: During construction it was guaranteed that the handle is Send
unsafe impl Send for ActorTaskHandle {}

// SAFETY: During construction it was guaranteed that the handle is Sync
unsafe impl Sync for ActorTaskHandle {}

impl ActorTaskHandle {
    /// Create a task handle from the raw function pointers.
    ///
    /// # Safety
    /// The caller must ensure that the provided function pointers are valid
    /// and correctly accept the pointer given by value as the first argument.
    /// Moreover, all access to the handle must be Send + Sync, including the handle itself.
    pub const unsafe fn from_raw(
        handle: *mut core::ffi::c_void,
        abort: unsafe fn(*const *mut core::ffi::c_void),
        wait_for_termination: unsafe fn(*const *mut core::ffi::c_void) -> WaitForTerminationFut,
        drop: unsafe fn(*mut core::ffi::c_void),
    ) -> Self {
        Self {
            handle,
            abort,
            wait_for_termination,
            drop,
        }
    }

    /// Create a task handle from an [`ActorTaskHandleVTable`] impl.
    ///
    /// `value` is moved to the heap, so `T` may be of any size. It must be
    /// `'static` because the handle carries no lifetime, so a backend that
    /// borrows is rejected:
    ///
    /// ```compile_fail,E0597
    /// use core::sync::atomic::{AtomicBool, Ordering};
    /// use factories_actor::actor::task::{
    ///     ActorTaskHandle, ActorTaskHandleVTable, WaitForTerminationFut,
    /// };
    ///
    /// struct Borrowed<'a>(&'a AtomicBool);
    ///
    /// impl ActorTaskHandleVTable for Borrowed<'_> {
    ///     fn abort(&self) {
    ///         self.0.store(true, Ordering::Relaxed);
    ///     }
    ///
    ///     fn wait_for_termination(&self) -> WaitForTerminationFut {
    ///         Box::pin(async {})
    ///     }
    /// }
    ///
    /// let handle = {
    ///     let flag = AtomicBool::new(false);
    ///     ActorTaskHandle::from_vtable(Borrowed(&flag))
    /// };
    /// handle.abort();
    /// ```
    pub fn from_vtable<T: ActorTaskHandleVTable + Send + Sync + 'static>(value: T) -> Self {
        // Always boxed, even if `T` fits in a pointer, storing it inline would
        // turn its padding bytes into uninitialized pointer bits.
        let handle = Box::into_raw(Box::new(value)).cast();

        Self {
            handle,
            abort: Self::vtable_abort_trampoline::<Box<T>>,
            wait_for_termination: Self::vtable_wait_for_termination_trampoline::<Box<T>>,
            drop: Self::vtable_drop_trampoline::<Box<T>>,
        }
    }

    unsafe fn vtable_abort_trampoline<T: ActorTaskHandleVTable>(
        handle: *const *mut core::ffi::c_void,
    ) {
        // SAFETY: The caller has guaranteed that handle points to the pointer this task handle
        //         was created from.
        let value = unsafe { handle.cast::<T>().as_ref_unchecked() };
        value.abort();
    }

    unsafe fn vtable_wait_for_termination_trampoline<T: ActorTaskHandleVTable>(
        handle: *const *mut core::ffi::c_void,
    ) -> WaitForTerminationFut {
        // SAFETY: The caller has guaranteed that the handle is the pointer that this task handle
        //         was created from.
        let value = unsafe { handle.cast::<T>().as_ref_unchecked() };
        value.wait_for_termination()
    }

    unsafe fn vtable_drop_trampoline<T: ActorTaskHandleVTable>(handle: *mut core::ffi::c_void) {
        let value = unsafe { core::ptr::from_ref(&handle).cast::<T>().read() };
        drop(value);
    }

    /// Abort the running task.
    ///
    /// This does not block or wait for the task to finish.
    /// Calling this multiple times does not have any effect, and any abort is best-effort.
    pub fn abort(&self) {
        // SAFETY: At creation time the caller has ensured that abort takes a pointer to the handle
        unsafe { (self.abort)(&self.handle) };
    }

    /// Wait for the running task to terminate.
    ///
    /// This does not initiate termination. The returned future is independent
    /// of this handle - but note that *dropping* the handle aborts the task,
    /// so keep it alive while waiting for a graceful termination.
    pub fn wait_for_termination(&self) -> WaitForTerminationFut {
        // SAFETY: At creation time the caller has ensured that wait_for_termination takes handle
        unsafe { (self.wait_for_termination)(&self.handle) }
    }
}

impl Drop for ActorTaskHandle {
    fn drop(&mut self) {
        // SAFETY: At creation time the caller has ensured that drop takes handle
        unsafe { (self.drop)(self.handle) };
    }
}

/// Vtable implementation for task handles.
///
/// [`ActorTaskHandle::from_vtable`] stores the implementation on the heap.
pub trait ActorTaskHandleVTable {
    /// Attempt to abort the task.
    ///
    /// This function must not block until the task is terminated
    /// but just mark it for cancellation/abortion.
    fn abort(&self);

    /// Wait for the task to terminate.
    ///
    /// This must not start forceful termination.
    fn wait_for_termination(&self) -> WaitForTerminationFut;
}

impl<T: ActorTaskHandleVTable> ActorTaskHandleVTable for Box<T> {
    fn abort(&self) {
        self.deref().abort();
    }

    fn wait_for_termination(&self) -> WaitForTerminationFut {
        self.deref().wait_for_termination()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    /// Fits in a pointer and has a padding byte at offset 1.
    #[repr(C)]
    struct Padded {
        tag: u8,
        id: u16,
    }

    static PADDED_ABORTED: AtomicU32 = AtomicU32::new(0);

    impl ActorTaskHandleVTable for Padded {
        fn abort(&self) {
            PADDED_ABORTED.store(
                (u32::from(self.tag) << 16) | u32::from(self.id),
                Ordering::Relaxed,
            );
        }

        fn wait_for_termination(&self) -> WaitForTerminationFut {
            Box::pin(async {})
        }
    }

    // Under miri this used to report UB in `from_vtable`: the padding byte was
    // read as part of a pointer.
    #[test]
    fn pointer_sized_backend_with_padding() {
        let handle = ActorTaskHandle::from_vtable(Padded {
            tag: 0xab,
            id: 0x1234,
        });
        handle.abort();
        assert_eq!(PADDED_ABORTED.load(Ordering::Relaxed), 0x00ab_1234);
    }

    static DROPS: AtomicUsize = AtomicUsize::new(0);

    struct CountsDrops;

    impl Drop for CountsDrops {
        fn drop(&mut self) {
            DROPS.fetch_add(1, Ordering::Relaxed);
        }
    }

    impl ActorTaskHandleVTable for CountsDrops {
        fn abort(&self) {}

        fn wait_for_termination(&self) -> WaitForTerminationFut {
            Box::pin(async {})
        }
    }

    #[test]
    fn backend_is_dropped_once_with_the_handle() {
        let handle = ActorTaskHandle::from_vtable(CountsDrops);
        assert_eq!(DROPS.load(Ordering::Relaxed), 0);
        drop(handle);
        assert_eq!(DROPS.load(Ordering::Relaxed), 1);
    }
}

use alloc::boxed::Box;
use core::mem::MaybeUninit;
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

    pub fn from_vtable<T: ActorTaskHandleVTable + Send + Sync>(value: T) -> Self {
        if size_of::<T>() <= size_of::<*mut core::ffi::c_void>()
            && align_of::<T>() <= align_of::<*mut core::ffi::c_void>()
        {
            // Can store this inline
            let handle = unsafe {
                let mut handle = MaybeUninit::<*mut core::ffi::c_void>::zeroed();
                handle.as_mut_ptr().cast::<T>().write(value);

                handle.assume_init()
            };

            Self {
                handle,
                abort: Self::vtable_abort_trampoline::<T>,
                wait_for_termination: Self::vtable_wait_for_termination_trampoline::<T>,
                drop: Self::vtable_drop_trampoline::<T>,
            }
        } else {
            let handle = Box::into_raw(Box::new(value)).cast();

            Self {
                handle,
                abort: Self::vtable_abort_trampoline::<Box<T>>,
                wait_for_termination: Self::vtable_wait_for_termination_trampoline::<Box<T>>,
                drop: Self::vtable_drop_trampoline::<Box<T>>,
            }
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
        self.abort();

        // SAFETY: At creation time the caller has ensured that drop takes handle
        unsafe { (self.drop)(self.handle) };
    }
}

/// Vtable implementation for task handles.
///
/// Task handles must be sized equal or smaller to a pointer.
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

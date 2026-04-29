use alloc::boxed::Box;
use core::any::TypeId;

/// Basic type information required by some library functions.
#[derive(Debug, Copy, Clone)]
pub struct BasicTypeRtti {
    size: usize,
    type_id: TypeId,
    drop_in_place: unsafe fn(*mut core::ffi::c_void),
    drop_boxed_ptr: unsafe fn(*mut core::ffi::c_void),
}

impl BasicTypeRtti {
    /// Create a new basic type information.
    pub const fn new<T: 'static>() -> Self {
        Self {
            size: size_of::<T>(),
            type_id: TypeId::of::<T>(),
            drop_in_place: Self::trampoline_drop_erased_in_place::<T>,
            drop_boxed_ptr: Self::trampoline_drop_erased_boxed_ptr::<T>,
        }
    }

    /// Drop an erased pointer in place.
    ///
    /// # Safety:
    /// The caller must ensure that ptr is a pointer to T.
    unsafe fn trampoline_drop_erased_in_place<T>(ptr: *mut core::ffi::c_void) {
        let ptr = ptr.cast::<T>();

        // SAFETY: The caller has ensured that this is a valid pointer to T.
        unsafe { core::ptr::drop_in_place(ptr) }
    }

    /// Drop an erased boxed pointer.
    ///
    /// # Safety:
    /// The caller must ensure that ptr was created using `Box::<T>::into_raw`.
    unsafe fn trampoline_drop_erased_boxed_ptr<T>(ptr: *mut core::ffi::c_void) {
        // SAFETY: The caller has ensured that ptr was created using `Box::<T>::into_raw()`.
        let boxed = unsafe { Box::<T>::from_raw(ptr.cast()) };

        drop(boxed);
    }

    /// Retrieves the size of the type.
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Retrieves the type id of the type.
    pub const fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Drop a pointer of the type in place.
    ///
    /// # Safety
    /// The caller must ensure that ptr points to a valid pointer of the type this RTTI
    /// is associated with.
    pub unsafe fn drop_in_place(&self, ptr: *mut core::ffi::c_void) {
        // SAFETY: The caller has ensured that ptr points to a valid pointer of this RTTI
        //         reflected type.
        unsafe { (self.drop_in_place)(ptr) };
    }

    /// Drop a pointer of the type that was created using `Box::<T>::into_raw()`.
    ///
    /// # Safety:
    /// The caller must ensure that ptr points to a valid pointer that was obtained using
    /// `Box::<T>::into_raw`, where `T` is the type this RTTI is associated with.
    pub unsafe fn drop_boxed_ptr(&self, ptr: *mut core::ffi::c_void) {
        unsafe { (self.drop_boxed_ptr)(ptr) };
    }
}

// Comparing the type id is enough.
impl PartialEq for BasicTypeRtti {
    fn eq(&self, other: &Self) -> bool {
        PartialEq::eq(&self.type_id, &other.type_id)
    }

    fn ne(&self, other: &Self) -> bool {
        PartialEq::ne(&self.type_id, &other.type_id)
    }
}

impl Eq for BasicTypeRtti {}

/// RTTI for cloning a value of a type.
#[derive(Debug, Copy, Clone)]
pub struct CloneRtti {
    clone_into: unsafe fn(*const core::ffi::c_void, *mut core::ffi::c_void, bool),
    clone_into_box: unsafe fn(*const core::ffi::c_void) -> *mut core::ffi::c_void,
}

impl CloneRtti {
    /// Creates the clone information for T.
    pub const fn new<T: Clone>() -> Self {
        Self {
            clone_into: Self::clone_into_trampoline::<T>,
            clone_into_box: Self::clone_into_box_trampoline::<T>,
        }
    }

    /// Clone a value of the type this is associated with.
    ///
    /// If `dest_initialized` is true, the destination will be dropped first.
    ///
    /// # Safety
    /// `T` shall in the following be the type that this RTTI is associated with:
    ///
    /// The caller must ensure that `src` is a valid pointer to T. Moreover, `dest` must be valid for
    /// writing a T, and if `dest_initialized` is true, dest must also be a valid pointer to T
    pub unsafe fn clone_into(
        &self,
        src: *const core::ffi::c_void,
        dest: *mut core::ffi::c_void,
        dest_initialized: bool,
    ) {
        // SAFETY: The caller has ensured that src is valid, dest is valid for writing, and dest
        //         is valid for dropping if dest_initialized is true.
        unsafe { (self.clone_into)(src, dest, dest_initialized) }
    }

    /// Clone a value into a box.
    ///
    /// # Safety
    /// The caller must ensure that `src` is a valid pointer to T.
    pub unsafe fn clone_into_box(
        &self,
        src: *const core::ffi::c_void
    ) -> *mut core::ffi::c_void {
        // SAFETY: The caller has ensured that src is valid.
        unsafe { (self.clone_into_box)(src) }
    }

    /// Clone a value.
    ///
    /// If `dest_initialized` is true, the destination will be dropped first.
    ///
    /// # Safety
    /// The caller must ensure that `src` is a valid pointer to T. Moreover, `dest` must be valid for
    /// writing a T, and if `dest_initialized` is true, dest must also be a valid pointer to T.
    unsafe fn clone_into_trampoline<T: Clone>(
        src: *const core::ffi::c_void,
        dest: *mut core::ffi::c_void,
        dest_initialized: bool,
    ) {
        // SAFETY: Called has ensured that src is a valid T.
        let src = unsafe { &*src.cast::<T>() };
        let dest = dest.cast::<T>();

        if dest_initialized {
            // SAFETY: Caller has ensured that dest is an initialized T
            let dest = unsafe { &mut *dest };

            dest.clone_from(src);
        } else {
            let cloned = src.clone();

            // SAFETY: Caller has ensured that dest is valid for writing a T
            unsafe { dest.write(cloned) };
        }
    }

    /// Clone a value into a box.
    ///
    /// # Safety
    /// The caller must ensure that `src` is a valid pointer to T.
    unsafe fn clone_into_box_trampoline<T: Clone>(
        src: *const core::ffi::c_void,
    ) -> *mut core::ffi::c_void {
        // SAFETY: Called has ensured that src is a valid T.
        let src = unsafe { &*src.cast::<T>() };

        Box::into_raw(Box::new(src.clone())).cast()
    }
}

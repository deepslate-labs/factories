use core::mem::MaybeUninit;

struct PartialArray<T, const N: usize> {
    data: [MaybeUninit<T>; N],
    initialized: usize,
}

impl<T, const N: usize> PartialArray<T, N> {
    pub fn new() -> Self {
        Self {
            data: MaybeUninit::<[T; N]>::uninit().into(),
            initialized: 0,
        }
    }

    /// Push a new value into the array, appending.
    ///
    /// # Safety
    /// The caller must ensure there is capacity left in the array.
    pub unsafe fn push_unchecked(&mut self, value: T) {
        debug_assert!(self.initialized < N, "PartialArray overflow");

        unsafe {
            self.data
                .as_mut_ptr()
                .add(self.initialized)
                .write(MaybeUninit::new(value))
        };
        self.initialized += 1;
    }

    /// Assume that all elements in the array are initialized.
    ///
    /// # Safety
    /// The caller must ensure that the array has been filled
    pub unsafe fn assume_init(self) -> [T; N] {
        debug_assert!(self.initialized == N, "PartialArray underflow");

        // SAFETY: We read data and then immediately forget self, so that data can't be dropped twice
        let data = unsafe { core::ptr::read(&self.data) };
        core::mem::forget(self);

        unsafe { MaybeUninit::<[T; N]>::from(data).assume_init() }
    }
}

impl<T, const N: usize> Drop for PartialArray<T, N> {
    fn drop(&mut self) {
        for i in 0..self.initialized {
            // SAFETY: We know that the element at index `i` is initialized because `i < self.initialized`.
            unsafe { self.data.as_mut_ptr().add(i).drop_in_place() };
        }
    }
}

/// Generate 2 arrays consisting of pairs of elements.
pub fn generate_pair_arrays<A, B, const ARRAY_LEN: usize>(
    mut builder: impl FnMut(usize) -> (A, B),
) -> ([A; ARRAY_LEN], [B; ARRAY_LEN]) {
    let mut a_array = PartialArray::<A, ARRAY_LEN>::new();
    let mut b_array = PartialArray::<B, ARRAY_LEN>::new();

    for i in 0..ARRAY_LEN {
        let (a, b) = builder(i);

        // SAFETY: We know that the array has still space because i < ARRAY_LEN.
        unsafe {
            a_array.push_unchecked(a);
            b_array.push_unchecked(b);
        }
    }

    // SAFETY: We know that the arrays are full because we've filled them up to ARRAY_LEN.
    unsafe { (a_array.assume_init(), b_array.assume_init()) }
}

use core::pin::Pin;
use core::task::{Context, Poll};

#[pin_project::pin_project]
pub struct BiasedFutureSelect<const COUNT: usize, F: Future> {
    #[pin]
    futures: [F; COUNT],
}

impl<const COUNT: usize, F: Future> BiasedFutureSelect<COUNT, F> {
    const fn new(futures: [F; COUNT]) -> Self {
        Self { futures }
    }
}

impl<const COUNT: usize, F: Future> Future for BiasedFutureSelect<COUNT, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        for future in pin_iter(this.futures) {
            if let Poll::Ready(output) = future.poll(cx) {
                return Poll::Ready(output);
            }
        }

        Poll::Pending
    }
}

fn pin_iter<T>(slice: Pin<&mut [T]>) -> impl Iterator<Item = Pin<&mut T>> {
    // SAFETY: We structurally project into the iterator and re-pin its elements
    let unwrapped_slice = unsafe { slice.get_unchecked_mut() };

    unwrapped_slice.iter_mut().map(|item| {
        // SAFETY: We are re-pinning the same element, which is safe as long as we don't move it
        unsafe { Pin::new_unchecked(item) }
    })
}

/// Select a future from the array of futures, biased towards lower indices.
pub const fn select_biased<const COUNT: usize, F: Future>(futures: [F; COUNT]) -> BiasedFutureSelect<COUNT, F> {
    BiasedFutureSelect::new(futures)
}

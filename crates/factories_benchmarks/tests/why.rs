//! Investigates *why* factories out-benchmarks kameo: counts heap allocations
//! per message for each framework, and verifies the throughput benchmark is
//! fair (the `ask` drains every preceding `tell` before returning).
//!
//! Run with `--test-threads=1 --nocapture` (the counting allocator is
//! process-global).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use factories_actor::actor::channel::ActorChannelSendable;
use factories_benchmarks::{fac, kam};

struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const WARMUP: u64 = 2_000;
const M: u64 = 100_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn factories_allocs_per_message() {
    let h = fac::spawn(0).await;
    for _ in 0..WARMUP {
        h.tell(fac::Inc).send().await.unwrap();
    }
    let base = h.ask(fac::Get).exchange().await.unwrap();

    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..M {
        h.tell(fac::Inc).send().await.unwrap();
    }
    let value = h.ask(fac::Get).exchange().await.unwrap();
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;

    // Fairness: the ask drained every tell — the bench measures real work.
    assert_eq!(value, base + M, "ask is not a barrier — bench would be unfair");
    println!(
        "factories: {allocs} allocs for {M} msgs = {:.2} allocs/msg",
        allocs as f64 / M as f64
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kameo_allocs_per_message() {
    let k = kam::spawn(0);
    for _ in 0..WARMUP {
        k.tell(kam::Inc).await.unwrap();
    }
    let base = k.ask(kam::Get).await.unwrap();

    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..M {
        k.tell(kam::Inc).await.unwrap();
    }
    let value = k.ask(kam::Get).await.unwrap();
    let allocs = ALLOCS.load(Ordering::Relaxed) - before;

    assert_eq!(value, base + M);
    println!(
        "kameo: {allocs} allocs for {M} msgs = {:.2} allocs/msg",
        allocs as f64 / M as f64
    );
}

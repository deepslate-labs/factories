//! Concurrency is opt-in — and this is what opting in actually buys you.
//!
//! The derive default is *serial*: one message handled to completion before the
//! next. Switching to a `ConcurrentRunLoop` lets several handler futures be in
//! flight at once — but only handlers that can hold their lock *simultaneously*
//! actually overlap. That means **shared (`&self`) reads under a `TokioRwLock`**:
//! many readers hold the read lock together, so their awaits run in parallel.
//! (Exclusive `&mut self` handlers still take turns — see the chapter.)
//!
//! This `Cache` proves the overlap with a gauge: each `lookup` records how many
//! lookups are in flight at once. On the serial default the peak would be 1.
//!
//! Run with: `cargo run -p factories --example concurrency`

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use factories::prelude::*;
use factories::runtime::concurrent_loop::ConcurrentRunLoop;
use factories::runtime::tokio::TokioRwLock;

/// Lock-free shared state: how many `lookup`s are running right now, and the
/// most we ever saw at once. The peak is the proof the reads overlapped.
#[derive(Default)]
struct Gauge {
    in_flight: AtomicUsize,
    peak: AtomicUsize,
}

// Concurrency is explicit: the `ConcurrentRunLoop` admits many handler futures
// at once, and `TokioRwLock` lets the `&self` readers among them share the lock.
#[derive(Actor)]
#[actor(run_loop = ConcurrentRunLoop<Self>, lock = TokioRwLock<Self>, shared = Gauge)]
struct Cache {
    entries: HashMap<u32, u64>,
}

#[factories::messages]
impl Cache {
    /// Exclusive write: `&mut self` takes the write lock, so it runs alone — no
    /// reader or other writer overlaps it. Writes are the rare case here.
    #[handler]
    fn insert(&mut self, key: u32, value: u64) {
        self.entries.insert(key, value);
    }

    /// Shared read: `&self` takes the read lock. Under the concurrent loop many
    /// of these hold the read lock at the same time, so the simulated-slow reads
    /// happen in parallel rather than one after another.
    #[handler]
    async fn lookup(&self, key: u32, #[context] cx: ActorContext<'_, Self>) -> Option<u64> {
        let gauge = cx.shared_data();
        let now = gauge.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        gauge.peak.fetch_max(now, Ordering::AcqRel);

        // Stand in for a slow read — a database round-trip, a disk seek. The read
        // lock is held across this await, but RwLock readers don't exclude one
        // another, so the sleeps overlap.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let value = self.entries.get(&key).copied();

        gauge.in_flight.fetch_sub(1, Ordering::AcqRel);
        value
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cache = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Cache { entries: HashMap::new() })
        .await?;

    // Populate the cache. Each `insert` is an exclusive write, so these serialize
    // — exactly what you want for a mutation.
    for key in 0..8u32 {
        cache.insert(key, u64::from(key) * 10).tell().await?;
    }
    println!("inserted 8 entries (writes run one at a time)");

    // Now fire 8 reads *concurrently*. We build all the ask futures, then await
    // them together, so they reach the actor as a burst.
    const N: u32 = 8;
    let started = Instant::now();
    let answers = futures::future::join_all((0..N).map(|key| cache.lookup(key).ask())).await;
    let elapsed = started.elapsed();

    let values: Vec<u64> = answers
        .into_iter()
        .map(|r| r.expect("ask").expect("entry present"))
        .collect();
    println!("8 lookups returned {values:?} in {elapsed:?}");

    // The proof: how many lookups were ever in flight at once?
    let peak = cache.state().shared_data().peak.load(Ordering::Acquire);
    println!("peak concurrent lookups: {peak}");

    // Each lookup sleeps 50ms. Serially that's ~400ms; overlapped it's ~50ms.
    assert!(peak > 1, "the concurrent loop + RwLock should overlap the reads");
    assert!(
        elapsed < Duration::from_millis(8 * 50),
        "overlapped reads should finish far faster than running them one by one"
    );
    println!("done — the reads overlapped instead of running one by one");

    Ok(())
}

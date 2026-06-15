//! Regression coverage at the actor level for the concurrent-loop stall.
//!
//! `concurrent_processes_all_messages` used to fail: the old default
//! `ConcurrentRunLoop` + kanal channel stalled under a burst of `tell`s (kanal's
//! async `recv` starved the work set when raced in the loop's `select`). With
//! the default mailbox switched to the cancellation-safe `tokio::sync::mpsc`, it
//! passes.

use factories_actor::actor::channel::ActorChannelSendable;
use factories_benchmarks::{fac, fac_seq, kam};

const N: u64 = 200_000;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_processes_all_messages() {
    let h = fac::spawn(0).await;
    for _ in 0..N {
        h.tell(fac::Inc).send().await.unwrap();
    }
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
    assert_eq!(h.ask(fac::Get).exchange().await.unwrap(), N);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequential_processes_all_messages() {
    let h = fac_seq::spawn(0).await;
    for _ in 0..N {
        h.tell(fac_seq::Inc).send().await.unwrap();
    }
    assert_eq!(h.ask(fac_seq::Get).exchange().await.unwrap(), N);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn kameo_processes_all_messages() {
    let k = kam::spawn(0);
    for _ in 0..N {
        k.tell(kam::Inc).await.unwrap();
    }
    assert_eq!(k.ask(kam::Get).await.unwrap(), N);
}

//! Dispatch-path comparison. Each iteration pushes a batch of `tell`s through
//! the mailbox and then drains with a single `ask` (a FIFO barrier — once the
//! ask returns, every preceding message has been handled). Reported per-element
//! so the number is the amortized cost of getting one message dispatched and
//! handled.
//!
//! Three paths:
//! - `factories/static`  — typed handle, sender-side devirtualized dispatch
//! - `factories/dynamic` — type-erased handle, the registry-lookup dispatch
//! - `kameo`             — kameo's typed `tell`/`ask`
//!
//! All three use a bounded(64) mailbox, so backpressure is part of the cost on
//! every contender equally.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use factories_actor::actor::channel::ActorChannelSendable;
use factories_actor::actor::handle::ActorHandle;
use factories_actor::message::envelope::MessageEnvelope;
use factories_benchmarks::{fac, kam, runtime};

const BATCH: u64 = 1000;

fn dispatch(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("dispatch");
    group.throughput(Throughput::Elements(BATCH));

    // factories — static typed-handle path.
    let stat = rt.block_on(fac::spawn(0));
    group.bench_function("factories/static", |b| {
        b.to_async(&rt).iter(|| async {
            for _ in 0..BATCH {
                stat.tell(fac::Inc).send().await.unwrap();
            }
            black_box(stat.ask(fac::Get).exchange().await.unwrap());
        });
    });

    // factories — type-erased dynamic path (registry lookup per send).
    let typed = rt.block_on(fac::spawn(0));
    let erased = typed.clone().erase_type();
    group.bench_function("factories/dynamic", |b| {
        b.to_async(&rt).iter(|| async {
            for _ in 0..BATCH {
                let envelope = MessageEnvelope::new(fac::Inc, None);
                erased
                    .prepare_send_dynamic(envelope)
                    .expect("Inc binds dynamically")
                    .send()
                    .await
                    .unwrap();
            }
            black_box(typed.ask(fac::Get).exchange().await.unwrap());
        });
    });

    // kameo — typed path.
    let kam_handle = rt.block_on(async { kam::spawn(0) });
    group.bench_function("kameo", |b| {
        b.to_async(&rt).iter(|| async {
            for _ in 0..BATCH {
                kam_handle.tell(kam::Inc).await.unwrap();
            }
            black_box(kam_handle.ask(kam::Get).await.unwrap());
        });
    });

    group.finish();
}

criterion_group!(benches, dispatch);
criterion_main!(benches);

//! Sustained throughput: how many messages/sec one actor's loop can absorb and
//! handle. Each iteration fills the actor with a large batch of `tell`s and
//! drains with a single `ask` barrier; reported per-element.
//!
//! The extra axis here is factories' loop strategy:
//! - `factories/concurrent` — `ConcurrentRunLoop` + `TokioMutexLock` (default)
//! - `factories/sequential` — `SequentialRunLoop` + `UnguardedLock` (lock-elided)
//! - `kameo`                — for reference

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use factories_actor::actor::channel::ActorChannelSendable;
use factories_benchmarks::{fac, fac_seq, kam, runtime};

const BATCH: u64 = 10_000;

fn throughput(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("throughput");
    group.throughput(Throughput::Elements(BATCH));

    let conc = rt.block_on(fac::spawn(0));
    group.bench_function("factories/concurrent", |b| {
        b.to_async(&rt).iter(|| async {
            for _ in 0..BATCH {
                conc.tell(fac::Inc).send().await.unwrap();
            }
            black_box(conc.ask(fac::Get).exchange().await.unwrap());
        });
    });

    let seq = rt.block_on(fac_seq::spawn(0));
    group.bench_function("factories/sequential", |b| {
        b.to_async(&rt).iter(|| async {
            for _ in 0..BATCH {
                seq.tell(fac_seq::Inc).send().await.unwrap();
            }
            black_box(seq.ask(fac_seq::Get).exchange().await.unwrap());
        });
    });

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

criterion_group!(benches, throughput);
criterion_main!(benches);

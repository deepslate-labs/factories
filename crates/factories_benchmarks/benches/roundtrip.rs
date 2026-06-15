//! Single-message latency: one `tell` (fire-and-forget) and one `ask`
//! (request/response) through a live actor, measured end-to-end including
//! runtime scheduling. factories (static typed handle) vs kameo.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use factories_actor::actor::channel::ActorChannelSendable;
use factories_benchmarks::{fac, kam, runtime};

fn roundtrip(c: &mut Criterion) {
    let rt = runtime();
    let mut group = c.benchmark_group("roundtrip");

    let fac_handle = rt.block_on(fac::spawn(0));
    group.bench_function("factories/tell", |b| {
        b.to_async(&rt)
            .iter(|| async { fac_handle.tell(fac::Inc).send().await.unwrap() });
    });
    group.bench_function("factories/ask", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(fac_handle.ask(fac::Get).exchange().await.unwrap()) });
    });

    let kam_handle = rt.block_on(async { kam::spawn(0) });
    group.bench_function("kameo/tell", |b| {
        b.to_async(&rt)
            .iter(|| async { kam_handle.tell(kam::Inc).await.unwrap() });
    });
    group.bench_function("kameo/ask", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(kam_handle.ask(kam::Get).await.unwrap()) });
    });

    group.finish();
}

criterion_group!(benches, roundtrip);
criterion_main!(benches);

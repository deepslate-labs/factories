//! Focused profiling target: spawn one actor, send N `tell`s, drain with one
//! `ask`. No criterion/harness overhead, so `perf stat`/`perf record` see only
//! the framework's send+consume path.
//!
//! Usage: `profile <factories|kameo> [N]`

use std::env;

use factories::actor::channel::ActorChannelSendable;
use factories_benchmarks::{fac, fac_seq, kam};

fn main() {
    let which = env::args().nth(1).unwrap_or_else(|| "factories".into());
    let n: u64 = env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2_000_000);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async move {
        match which.as_str() {
            "factories" => {
                let h = fac::spawn(0).await;
                for _ in 0..n {
                    h.tell(fac::Inc).send().await.unwrap();
                }
                let v = h.ask(fac::Get).exchange().await.unwrap();
                assert_eq!(v, n);
            }
            "factories-seq" => {
                let h = fac_seq::spawn(0).await;
                for _ in 0..n {
                    h.tell(fac_seq::Inc).send().await.unwrap();
                }
                let v = h.ask(fac_seq::Get).exchange().await.unwrap();
                assert_eq!(v, n);
            }
            "kameo" => {
                let k = kam::spawn(0);
                for _ in 0..n {
                    k.tell(kam::Inc).await.unwrap();
                }
                let v = k.ask(kam::Get).await.unwrap();
                assert_eq!(v, n);
            }
            "factories-big" => {
                let h = fac::spawn(0).await;
                for _ in 0..n {
                    h.tell(fac::IncBig { _payload: [0; 32] }).send().await.unwrap();
                }
                let v = h.ask(fac::Get).exchange().await.unwrap();
                assert_eq!(v, n);
            }
            "kameo-big" => {
                let k = kam::spawn(0);
                for _ in 0..n {
                    k.tell(kam::IncBig([0; 32])).await.unwrap();
                }
                let v = k.ask(kam::Get).await.unwrap();
                assert_eq!(v, n);
            }
            other => panic!("unknown target: {other}"),
        }
        println!("{which}: {n} messages done");
    });
}

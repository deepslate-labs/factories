//! Lifecycle hooks on a *derived* actor: `#[on_start]` / `#[on_stop]` written in
//! a `#[messages]` block. The derive routes them to `Actor::on_start` /
//! `on_stop` via `match_specialize!`; this is the macro counterpart of the
//! hand-written `Hooked` actor in `spawn.rs`.

use std::sync::{Arc, Mutex};

use factories_actor::actor::lifecycle::StopReason;
use factories_actor::actor::{Actor, ActorContext};
use factories_actor::runtime::lock::UnguardedLock;
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::runtime::tokio::TokioTaskSpawner;
use factories_actor::spawn::ActorLauncher;

/// Shared event log so the test can assert hook ordering.
#[derive(Default, Clone)]
pub struct Log(Arc<Mutex<Vec<&'static str>>>);

impl Log {
    fn record(&self, event: &'static str) {
        self.0.lock().expect("log mutex").push(event);
    }

    fn snapshot(&self) -> Vec<&'static str> {
        self.0.lock().expect("log mutex").clone()
    }
}

#[derive(Actor)]
#[actor(
    lock = UnguardedLock<Self>,
    run_loop = SequentialRunLoop<Self>,
    shared = Log,
)]
struct Hooked;

#[factories_actor::messages]
impl Hooked {
    #[on_start]
    async fn start(&mut self, cx: ActorContext<'_, Self>) {
        cx.extension().record("start");
    }

    #[on_stop]
    async fn stop(self, reason: StopReason<'_, Self>, cx: ActorContext<'_, Self>) {
        cx.extension().record(match reason {
            StopReason::Finished => "stop:finished",
            StopReason::Failed(_) => "stop:failed",
        });
    }

    #[handler]
    async fn ping(&self, #[context] cx: ActorContext<'_, Self>) {
        cx.extension().record("ping");
    }
}

#[tokio::test]
async fn derived_lifecycle_hooks_run_in_order() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Hooked)
        .await
        .expect("hooked init is infallible");

    // `on_start` ran before `Running` became observable.
    let log = handle.state().extension().clone();
    assert_eq!(log.snapshot(), ["start"], "on_start runs before Running");

    handle.ping().await.expect("ask");
    assert_eq!(log.snapshot(), ["start", "ping"]);

    let state = handle.state().clone();
    drop(handle);

    state.wait_for_terminal().await;

    assert_eq!(
        log.snapshot(),
        ["start", "ping", "stop:finished"],
        "on_stop runs on a clean drain with the Finished reason"
    );
}

// -- die_on_err on a hook ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartBoom;

#[derive(Actor)]
#[actor(error = StartBoom, lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct FailingStart;

#[factories_actor::messages]
impl FailingStart {
    /// `die_on_err`: returning `Err` fails the actor (routed to `cx.fail`), which
    /// aborts startup before the loop runs.
    #[on_start(die_on_err)]
    async fn start(&mut self, _cx: ActorContext<'_, Self>) -> Result<(), StartBoom> {
        Err(StartBoom)
    }
}

#[tokio::test]
async fn on_start_die_on_err_aborts_startup() {
    let spawner = TokioTaskSpawner::current();

    let result = ActorLauncher::default()
        .spawn_ready(&spawner, FailingStart)
        .await;

    assert_eq!(
        result.err(),
        Some(StartBoom),
        "a failing #[on_start(die_on_err)] aborts startup with the recorded error"
    );
}

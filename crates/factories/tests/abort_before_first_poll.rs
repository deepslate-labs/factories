//! An actor whose task is aborted before its first poll still reaches `Dead`
//! with [`TerminationReason::Aborted`] and its watchers receive `Terminated`.
//!
//! The tests run on a current-thread runtime and do not await between `spawn`
//! and `abort`, so the actor future has never been polled when it is aborted.

#![cfg(all(
    feature = "derive",
    feature = "dynamic-dispatch",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

use std::time::Duration;

use factories::actor::Actor;
use factories::actor::channel::ActorChannelSendError;
use factories::actor::lifecycle::{TerminationKind, TerminationReason};
use factories::actor::supervision::Terminated;
use factories::runtime::concurrent_loop::ConcurrentRunLoop;
use factories::runtime::tokio::{
    TokioMpscChannelCapacity, TokioMpscChannelOptions, TokioRwLock, TokioTaskSpawner,
};
use factories::spawn::ActorLauncher;
use tokio::sync::mpsc;

/// Only bounds waits that never finish without the fix.
const WATCHDOG: Duration = Duration::from_secs(5);

#[derive(Actor)]
struct Serial;

#[factories::messages]
impl Serial {
    #[handler]
    fn ping_serial(&self) {}
}

#[derive(Actor)]
#[actor(run_loop = ConcurrentRunLoop<Self>, lock = TokioRwLock<Self>)]
struct Concurrent;

#[factories::messages]
impl Concurrent {
    #[handler]
    fn ping_concurrent(&self) {}
}

macro_rules! abort_before_first_poll {
    ($test:ident, $actor:ident, $ping:ident) => {
        #[tokio::test]
        async fn $test() {
            let actor = ActorLauncher::default().spawn(&TokioTaskSpawner::current(), $actor);
            let task = actor.state().task().unwrap();
            // No await since `spawn`: the actor future has not been polled.
            task.abort();
            task.wait_for_termination().await;

            tokio::time::timeout(WATCHDOG, actor.state().wait_for_terminal())
                .await
                .expect("the aborted actor reaches Dead");
            assert!(matches!(
                actor.state().termination_reason(),
                Some(TerminationReason::Aborted)
            ));
            assert!(matches!(
                actor.$ping().try_tell(),
                Err(ActorChannelSendError::ActorDead)
            ));
        }
    };
}

abort_before_first_poll!(sequential_loop_reaches_dead, Serial, ping_serial);
abort_before_first_poll!(concurrent_loop_reaches_dead, Concurrent, ping_concurrent);

/// Forwards the kind of every `Terminated` it receives to the test.
#[derive(Actor)]
struct Watcher {
    events: mpsc::UnboundedSender<TerminationKind>,
}

#[factories::messages]
impl Watcher {
    #[handler(message = Terminated)]
    fn terminated(&mut self, #[message] event: Terminated) {
        let _ = self.events.send(event.kind());
    }
}

#[tokio::test]
async fn watcher_is_notified() {
    let spawner = TokioTaskSpawner::current();
    let (events, mut observed) = mpsc::unbounded_channel();
    // Unbounded: with a bounded mailbox the guard's synchronous delivery goes
    // through `block_in_place`, which panics on a current-thread runtime.
    let watcher = ActorLauncher::<Watcher>::builder()
        .channel_options(TokioMpscChannelOptions {
            capacity: TokioMpscChannelCapacity::Unbounded,
            ..Default::default()
        })
        .build()
        .spawn_ready(&spawner, Watcher { events })
        .await
        .unwrap();

    let worker = ActorLauncher::default().spawn(&spawner, Serial);
    watcher.watch(&worker, 0);
    let task = worker.state().task().unwrap();
    // No await since `spawn`: the worker future has not been polled.
    task.abort();
    task.wait_for_termination().await;

    let event = tokio::time::timeout(WATCHDOG, observed.recv())
        .await
        .expect("the watcher receives Terminated");
    assert!(matches!(event, Some(TerminationKind::Aborted)));
}

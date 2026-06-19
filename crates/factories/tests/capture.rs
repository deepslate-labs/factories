#![cfg(feature = "capture")]

//! End-to-end capture: a mesh configured with a [`CaptureSink`] records actor
//! births and deaths. (Message edges are exercised separately.)

use std::sync::{Arc, Mutex};

use factories::actor::Actor;
use factories::capture::{CaptureEvent, CaptureSink, CAPTURE_SINK};
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::ActorLauncher;

#[derive(Default)]
struct Collected(Mutex<Vec<CaptureEvent>>);

impl CaptureSink for Collected {
    fn record(&self, event: CaptureEvent) {
        self.0.lock().expect("sink mutex").push(event);
    }
}

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Node;

#[factories::messages]
impl Node {
    #[handler]
    async fn ping(&self) {}
}

#[tokio::test]
async fn births_and_deaths_are_captured() {
    let spawner = TokioTaskSpawner::current();
    let sink = Arc::new(Collected::default());
    let sink_dyn: Arc<dyn CaptureSink> = sink.clone();

    let handle = ActorLauncher::default()
        .with_extension(CAPTURE_SINK, sink_dyn)
        .spawn_ready(&spawner, Node)
        .await
        .expect("infallible init");

    let state = handle.state().clone();
    drop(handle); // close the mailbox so the actor drains and stops
    state.wait_for_terminal().await;

    let events = sink.0.lock().expect("sink mutex");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CaptureEvent::Spawned { actor_type: "Node", .. })),
        "expected a Spawned event, got {events:?}",
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, CaptureEvent::Died { actor_type: "Node", .. })),
        "expected a Died event, got {events:?}",
    );
}

#![cfg(feature = "capture")]

//! End-to-end capture: a mesh configured with a [`CaptureSink`] records actor
//! births and deaths. (Message edges are exercised separately.)

use std::sync::{Arc, Mutex};

use factories::actor::channel::ActorChannelSendable;
use factories::actor::{Actor, ActorContext};
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

// -- message edges + causality ------------------------------------------------

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Downstream;

#[factories::messages]
impl Downstream {
    #[handler]
    async fn note(&self) {}
    /// Sync point: a FIFO drain so the test can await prior messages being handled.
    #[handler]
    async fn drain(&self) {}
}

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Upstream {
    downstream: <Downstream as Actor>::TypedHandle,
}

#[factories::messages]
impl Upstream {
    #[handler]
    async fn relay(&self) {
        // Raw `tell(Msg).send()` is the Send-compatible path from inside a handler
        // (the generated `MessageCall::tell()` future is not Send).
        let _ = self.downstream.tell(Note).send().await;
    }
}

#[tokio::test]
async fn message_edges_and_causality_are_captured() {
    let spawner = TokioTaskSpawner::current();
    let sink = Arc::new(Collected::default());
    let sink_dyn: Arc<dyn CaptureSink> = sink.clone();

    let downstream = ActorLauncher::default()
        .with_extension(CAPTURE_SINK, sink_dyn.clone())
        .spawn_ready(&spawner, Downstream)
        .await
        .expect("init");
    let upstream = ActorLauncher::default()
        .with_extension(CAPTURE_SINK, sink_dyn)
        .spawn_ready(&spawner, Upstream { downstream: downstream.clone() })
        .await
        .expect("init");

    let up_id = upstream.state().id();
    let down_id = downstream.state().id();

    upstream.relay().await.expect("ask"); // external -> upstream; relay tells downstream.note()
    downstream.drain().await.expect("ask"); // FIFO: returns after downstream handled note()

    let events = sink.0.lock().expect("sink mutex");

    // external -> upstream (the relay ask): from = None
    let relay_evt = events
        .iter()
        .find_map(|e| match e {
            CaptureEvent::Message { id, from: None, to, .. } if *to == up_id => Some(*id),
            _ => None,
        })
        .unwrap_or_else(|| panic!("external relay edge (from=None) not captured: {events:?}"));

    // upstream -> downstream (the note tell): from = Some(upstream), caused by the relay
    let note_edge = events
        .iter()
        .find(|e| {
            matches!(e,
                CaptureEvent::Message { from: Some(f), to, .. } if *f == up_id && *to == down_id)
        })
        .unwrap_or_else(|| panic!("upstream->downstream edge not captured: {events:?}"));

    if let CaptureEvent::Message { caused_by, .. } = note_edge {
        assert_eq!(
            *caused_by,
            Some(relay_evt),
            "the edge is causally linked to the message whose handling produced it",
        );
    }
}

// -- spawn parent linkage -----------------------------------------------------

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Leaf;

#[factories::messages]
impl Leaf {
    #[handler]
    async fn poke(&self) {}
}

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Root;

#[factories::messages]
impl Root {
    #[handler]
    async fn sprout(&self, #[context] cx: ActorContext<'_, Self>) {
        let spawner = TokioTaskSpawner::current();
        // The child inherits the capture sink from us; its `Spawned` event should
        // record us as the parent and this `sprout` as the cause.
        let _child = ActorLauncher::default()
            .inherit_from(cx.extensions())
            .spawn_ready(&spawner, Leaf)
            .await
            .expect("child init");
    }
}

#[tokio::test]
async fn a_childs_spawn_records_its_parent() {
    let spawner = TokioTaskSpawner::current();
    let sink = Arc::new(Collected::default());
    let sink_dyn: Arc<dyn CaptureSink> = sink.clone();

    let root = ActorLauncher::default()
        .with_extension(CAPTURE_SINK, sink_dyn)
        .spawn_ready(&spawner, Root)
        .await
        .expect("init");

    let root_id = root.state().id();
    root.sprout().await.expect("ask"); // spawn_ready inside awaited Running, so Spawned is recorded

    let events = sink.0.lock().expect("sink mutex");
    assert!(
        events.iter().any(|e| matches!(e,
            CaptureEvent::Spawned { actor_type: "Leaf", parent: Some(p), .. } if *p == root_id)),
        "the child's Spawned should record root as its parent: {events:?}",
    );
}

// -- abnormal termination (panic / abort) -------------------------------------

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Doomed;

#[factories::messages]
impl Doomed {
    #[handler]
    async fn boom(&self) {
        panic!("boom");
    }
}

#[tokio::test]
async fn an_aborted_actor_emits_exactly_one_died() {
    use factories::actor::lifecycle::TerminationKind;

    let spawner = TokioTaskSpawner::current();
    let sink = Arc::new(Collected::default());
    let sink_dyn: Arc<dyn CaptureSink> = sink.clone();

    let handle = ActorLauncher::default()
        .with_extension(CAPTURE_SINK, sink_dyn)
        .spawn_ready(&spawner, Doomed)
        .await
        .expect("init");

    let state = handle.state().clone();
    let _ = handle.boom().tell().await; // the handler panics -> the task aborts
    state.wait_for_terminal().await;

    let events = sink.0.lock().expect("sink mutex");
    let deaths: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, CaptureEvent::Died { .. }))
        .collect();
    assert_eq!(deaths.len(), 1, "exactly one Died, no double-emit: {events:?}");
    assert!(
        matches!(deaths[0], CaptureEvent::Died { reason: TerminationKind::Aborted, .. }),
        "the abort path records Aborted: {:?}",
        deaths[0],
    );
}

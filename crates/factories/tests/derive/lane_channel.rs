//! `#[actor(channel = TokioLaneChannel<...>)]`: a priority-routed multi-lane
//! mailbox wired through the derive layer.
//!
//! The consumer motivation (corvidae node actors) is control-class messages
//! delivered ahead of data. This test proves that shape end to end: an actor
//! whose channel routes one message type to the high-priority lane, with data
//! and priority messages enqueued *before* the actor starts draining, sees the
//! priority message handled first.

use std::sync::{Arc, Mutex};

use factories::actor::channel::ActorChannelSendable;
use factories::actor::{Actor, StaticOnlyBinder};
use factories::implement_message_handler;
use factories::message::Message;
use factories::runtime::lock::{self, UnguardedLock};
use factories::runtime::routing::ActorMessagePriorityRouter;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::{
    TokioMpscMultiLineActorChannel, TokioMpscChannelCapacity, TokioMpscChannelOptions, TokioTaskSpawner,
};
use factories::spawn::ActorLauncher;

// A router that puts the control-class message on lane 0 (highest priority) and
// everything else on lane 1. Routing keys purely on the message RTTI - exactly
// how a real consumer declares "this message type jumps the queue".
#[derive(Default, Clone, Copy, Debug)]
struct ControlFirstRouter;

impl ActorMessagePriorityRouter for ControlFirstRouter {
    fn priority(&self, dispatched: &factories::actor::dispatch::DispatchedActorMessage) -> usize {
        if dispatched.envelope().rtti() == Priority::RTTI {
            0
        } else {
            1
        }
    }
}

type LaneChannel = TokioMpscMultiLineActorChannel<2, ControlFirstRouter>;

#[derive(Actor)]
#[actor(
    channel = LaneChannel,
    binder = StaticOnlyBinder,
    lock = UnguardedLock<Self>,
    run_loop = SequentialRunLoop<Self>,
)]
struct Recorder {
    // The order in which handlers ran, by message tag.
    order: Arc<Mutex<Vec<&'static str>>>,
}

#[derive(Debug, Message)]
struct Data;

#[derive(Debug, Message)]
struct Priority;

implement_message_handler!(Recorder, Data, lock::Exclusive, |ctx| async move {
    let (guard, _, _) = ctx.into_parts();
    guard.order.lock().unwrap().push("data");
});

implement_message_handler!(Recorder, Priority, lock::Exclusive, |ctx| async move {
    let (guard, _, _) = ctx.into_parts();
    guard.order.lock().unwrap().push("priority");
});

#[tokio::test]
async fn priority_message_is_handled_before_earlier_data() {
    let spawner = TokioTaskSpawner::current();
    let order = Arc::new(Mutex::new(Vec::new()));

    // A gate the actor's init awaits, so we can fill the mailbox before it
    // starts draining. Messages sent before init completes queue in the mailbox.
    let (release, gated) = tokio::sync::oneshot::channel::<()>();

    let order_for_actor = order.clone();
    let handle = ActorLauncher::<Recorder>::builder()
        .channel_options(TokioMpscChannelOptions {
            router: ControlFirstRouter,
            capacity: TokioMpscChannelCapacity::Bounded(16),
        })
        .build()
        .spawn(&spawner, || async move {
            // Block init until the mailbox has been filled and released.
            let _ = gated.await;
            Ok(Recorder {
                order: order_for_actor,
            })
        });

    // Enqueue three data messages, then the priority message. All land in the
    // mailbox while init is still parked.
    handle.tell(Data).send().await.expect("send data");
    handle.tell(Data).send().await.expect("send data");
    handle.tell(Data).send().await.expect("send data");
    handle.tell(Priority).send().await.expect("send priority");

    // Let init finish; the run loop now drains, lane 0 first.
    release.send(()).unwrap();

    // Drain by asking a synchronising message would need another handler; instead
    // poll until all four handlers have run.
    loop {
        if order.lock().unwrap().len() == 4 {
            break;
        }
        tokio::task::yield_now().await;
    }

    let order = order.lock().unwrap().clone();
    assert_eq!(
        order.first().copied(),
        Some("priority"),
        "priority (lane 0) must be handled before the earlier-sent data messages: {order:?}"
    );
}

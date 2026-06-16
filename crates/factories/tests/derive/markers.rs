//! Parameter markers: deferred answers (`#[answer]`), whole-message
//! passthrough (`#[message]`) and sealed envelope forwarding (`#[envelope]`).

use factories::actor::channel::ActorChannelSendable;
use factories::actor::handle::{ActorHandle, AnyActorHandle};
use factories::actor::{Actor, StaticOnlyBinder};
use factories::message::channel::{AnswerSender, answer_channel};
use factories::message::envelope::{MessageEnvelope, SendableEnvelope};
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::ActorLauncher;

use crate::actor::Defaulted;
use crate::handlers::{AddBoth, Probe};

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Deferring {
    pending: Option<AnswerSender<Defer>>,
}

#[factories::messages]
impl Deferring {
    /// Manual answering: stash the sender, answer on `Release`. No return
    /// type to infer the answer type from, hence the `answer` key.
    #[handler(answer = u32)]
    fn defer(&mut self, #[answer] reply: Option<AnswerSender<Defer>>) {
        self.pending = reply;
    }

    #[handler]
    fn release(&mut self) {
        if let Some(pending) = self.pending.take() {
            let _ = pending.send(42);
        }
    }

    /// Whole-message passthrough, composing with the automatic answer.
    #[handler(message = AddBoth)]
    fn sum(&mut self, #[message] whole: AddBoth) -> u32 {
        whole.left + whole.right
    }
}

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>, binder = StaticOnlyBinder)]
struct Relay {
    target: AnyActorHandle,
}

#[factories::messages]
impl Relay {
    /// Forward the sealed envelope - the answer sender travels inside, so the
    /// target answers the original asker directly. `SendableEnvelope` (not
    /// the raw envelope) because async fn arguments live in the future.
    #[handler(message = Probe)]
    async fn relay(&mut self, #[envelope] envelope: SendableEnvelope) {
        self.target
            .prepare_send_dynamic(envelope.into_inner())
            .expect("probe must bind on the target")
            .send()
            .await
            .expect("forward must succeed");
    }
}

// -- Tests ----------------------------------------------------------------------

#[tokio::test]
async fn deferred_answer_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Deferring { pending: None })
        .await
        .expect("deferring init is infallible");
    let erased = handle.clone().erase_type();

    // Ask without awaiting the answer: a dynamic send carrying the sender.
    let (answer_sender, answer_receiver) = answer_channel::<Defer>();
    erased
        .prepare_send_dynamic(MessageEnvelope::new(Defer, Some(answer_sender)))
        .expect("Defer must bind")
        .send()
        .await
        .expect("send must succeed");

    // The handler stashed the sender instead of answering; the mailbox is
    // FIFO, so Release arrives after Defer and triggers the deferred answer.
    handle.tell(Release).send().await.expect("tell");
    assert_eq!(answer_receiver.recv().await.expect("deferred answer"), 42);
}

#[tokio::test]
async fn whole_message_passthrough() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Deferring { pending: None })
        .await
        .expect("deferring init is infallible");

    assert_eq!(
        handle
            .ask(AddBoth { left: 4, right: 5 })
            .exchange()
            .await
            .expect("ask"),
        9
    );
}

#[tokio::test]
async fn envelope_forwarding_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let target = ActorLauncher::default()
        .spawn_ready(&spawner, Defaulted { value: 7 })
        .await
        .expect("defaulted init is infallible")
        .erase_type();

    let relay = ActorLauncher::default()
        .spawn_ready(&spawner, Relay { target })
        .await
        .expect("relay init is infallible");

    // Ask the relay: it forwards the sealed envelope, the target answers the
    // original asker directly.
    assert_eq!(relay.ask(Probe).exchange().await.expect("ask"), 7);
}

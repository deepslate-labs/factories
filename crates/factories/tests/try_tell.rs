//! `try_tell`: the synchronous, non-blocking fire-and-forget on the call
//! surface.
//!
//! [`MailboxFull`]: factories::actor::channel::ActorChannelSendError::MailboxFull

#![cfg(all(
    feature = "derive",
    feature = "dynamic-dispatch",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

use factories::actor::Actor;
use factories::actor::channel::ActorChannelSendError;
use factories::protocol;
use factories::runtime::tokio::{
    TokioMpscChannelCapacity, TokioMpscChannelOptions, TokioTaskSpawner,
};
use factories::spawn::ActorLauncher;

#[derive(Actor)]
struct Tally {
    count: u32,
}

#[factories::messages]
impl Tally {
    #[handler]
    fn bump(&mut self) {
        self.count += 1;
    }

    #[handler]
    fn count(&mut self) -> u32 {
        self.count
    }
}

/// Erased surface over the generated `Bump` message.
#[protocol]
trait Bumping {
    fn bump(&self, msg: Bump);
}

async fn spawn_tally() -> TallyHandle {
    ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Tally { count: 0 })
        .await
        .expect("tally init is infallible")
}

#[tokio::test]
async fn typed_try_tell_delivers() {
    let tally = spawn_tally().await;

    tally.bump().try_tell().expect("room in a fresh mailbox");

    // The mailbox is FIFO: the ask is enqueued after the bump, so its answer
    // observes the delivered message.
    assert_eq!(tally.count().await.expect("ask"), 1);
}

#[tokio::test]
async fn erased_protocol_try_tell_delivers() {
    let tally = spawn_tally().await;
    let bumper: BumpingHandle = tally.clone().into();

    bumper.bump(Bump).try_tell().expect("room in a fresh mailbox");

    assert_eq!(tally.count().await.expect("ask"), 1);
}

#[tokio::test]
async fn try_tell_reports_mailbox_full() {
    // Park init so nothing drains: messages sent before init completes queue
    // in the mailbox. With a bounded(1) mailbox the first try_tell fills the
    // only slot and the second must fail fast instead of waiting.
    let (release, gated) = tokio::sync::oneshot::channel::<()>();

    let tally = ActorLauncher::<Tally>::builder()
        .channel_options(TokioMpscChannelOptions {
            capacity: TokioMpscChannelCapacity::Bounded(1),
            ..Default::default()
        })
        .build()
        .spawn(&TokioTaskSpawner::current(), || async move {
            let _ = gated.await;
            Ok(Tally { count: 0 })
        });

    tally.bump().try_tell().expect("the single slot is free");

    let error = tally
        .bump()
        .try_tell()
        .expect_err("a full mailbox must fail fast");
    assert!(matches!(error, ActorChannelSendError::MailboxFull));

    // Unpark: the queued bump drains and the mailbox accepts traffic again.
    release.send(()).expect("actor init is parked on this");
    assert_eq!(tally.count().await.expect("ask"), 1);
}

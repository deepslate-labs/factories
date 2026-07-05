//! The `Send` guarantee on the call surface: `Calling`/`MessageCall` declare
//! `Send` tell and ask futures, so calls are awaitable inside `tokio::spawn` -
//! for typed handles and erased protocol handles alike (the shape work-stealing
//! runtimes demand). The thread-local twin (`LocalCalling`/`LocalMessageCall`,
//! via `call_local` and `#[protocol(local)]`) keeps `!Send` answers
//! expressible with today's unbounded semantics.

#![cfg(all(
    feature = "derive",
    feature = "dynamic-dispatch",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

use core::future::IntoFuture;
use std::rc::Rc;

use factories::actor::handle::TypedActorHandle;
use factories::actor::{Actor, MessageHandler};
use factories::message::Message;
use factories::protocol;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::ActorLauncher;

#[derive(Actor)]
struct Counter {
    value: u32,
}

#[factories::messages]
impl Counter {
    #[handler]
    fn add(&mut self, amount: u32) {
        self.value += amount;
    }

    #[handler]
    fn total(&mut self) -> u32 {
        self.value
    }
}

#[protocol]
trait Counting {
    fn add(&self, msg: Add);
    fn total(&self, msg: Total);
}

async fn spawn_counter(value: u32) -> CounterHandle {
    ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Counter { value })
        .await
        .expect("counter init is infallible")
}

/// Compile-time `Send` assertion: fails to compile if `T` is not `Send`.
fn assert_send<T: Send>(value: T) -> T {
    value
}

#[tokio::test]
async fn typed_call_futures_are_send() {
    let counter = spawn_counter(10).await;

    assert_send(counter.add(1).tell()).await.expect("tell");
    assert_eq!(assert_send(counter.total().ask()).await.expect("ask"), 11);
    // The bare-`.await` path (`IntoFuture`) yields a `Send` future too.
    assert_eq!(
        assert_send(counter.total().into_future())
            .await
            .expect("ask"),
        11
    );
}

#[tokio::test]
async fn erased_protocol_call_futures_are_send() {
    let counter: CountingHandle = spawn_counter(3).await.into();

    assert_send(counter.add(Add { amount: 1 }).tell())
        .await
        .expect("tell");
    assert_eq!(
        assert_send(counter.total(Total).ask()).await.expect("ask"),
        4
    );
    assert_eq!(
        assert_send(counter.total(Total).into_future())
            .await
            .expect("ask"),
        4
    );
}

#[tokio::test]
async fn typed_calls_run_inside_tokio_spawn() {
    let counter = spawn_counter(0).await;

    let total = tokio::spawn(async move {
        counter.add(2).tell().await.expect("tell");
        counter.total().await.expect("ask")
    })
    .await
    .expect("task join");

    assert_eq!(total, 2);
}

#[tokio::test]
async fn erased_protocol_calls_run_inside_tokio_spawn() {
    let counter: CountingHandle = spawn_counter(0).await.into();

    let total = tokio::spawn(async move {
        counter.add(Add { amount: 2 }).tell().await.expect("tell");
        counter.total(Total).await.expect("ask")
    })
    .await
    .expect("task join");

    assert_eq!(total, 2);
}

/// A message whose answer is `!Send`: inexpressible on the `Send`-guaranteed
/// [`Calling`] surface (`call` requires `M::Answer: Send`), carried by the
/// unbounded local twin instead.
#[derive(Debug, Message)]
#[message(answer = Rc<u32>)]
struct GetRc;

/// A `local` protocol over the `!Send`-answer message: its generated methods
/// return `LocalMessageCall<impl LocalCalling<…>>`, which carries no `Send`
/// bounds anywhere.
#[protocol(local)]
trait RcAsking {
    fn get_rc(&self, msg: GetRc);
}

// Compile-only: never run (a `!Send` answer cannot cross the channel boundary
// at runtime - `MessageEnvelope::is_sendable` rejects it on cross-thread
// channels).
#[allow(dead_code)]
async fn local_surface_expresses_non_send_answers<A>(handle: &TypedActorHandle<A>)
where
    A: Actor + MessageHandler<GetRc>,
{
    let _ = handle.call_local(GetRc).await;
}

#[allow(dead_code)]
async fn local_protocol_expresses_non_send_answers(handle: RcAskingHandle) {
    let _rc = handle.get_rc(GetRc).await;
    let handle2 = handle.clone();
    let _ = handle2.get_rc(GetRc).tell().await;
}

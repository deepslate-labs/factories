#![cfg(all(
    feature = "derive",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

//! The `crate = "..."` override on the proc macros.
//!
//! The macros emit paths through the `factories` facade by default. Pointing
//! `crate` at `crate` instead makes the generated code refer back through
//! `factories_actor`'s own root - so this crate can exercise its own derives
//! without depending on the facade, and a downstream crate that re-exports the
//! facade under a different name has a way to retarget the codegen.
//!
//! All four proc macros are covered: `#[derive(Actor)]`, `#[messages]`,
//! `#[derive(Message)]`, and `#[protocol]`.

use factories_actor::actor::Actor;
use factories_actor::actor::handle::TypedActorHandle;
use factories_actor::message::Message;
use factories_actor::runtime::tokio::TokioTaskSpawner;
use factories_actor::spawn::ActorLauncher;

#[derive(Actor)]
#[actor(crate = "::factories_actor")]
struct Counter {
    value: u64,
}

#[factories_actor::messages(crate = "::factories_actor")]
impl Counter {
    #[handler]
    fn inc(&mut self) {
        self.value += 1;
    }

    #[handler]
    async fn get(&self) -> u64 {
        self.value
    }
}

/// `#[derive(Message)]` routed through the override; the answer type proves the
/// generated `impl Message` resolved against `crate`'s root.
#[derive(Message)]
#[message(crate = "::factories_actor", answer = u32)]
#[allow(dead_code)]
struct Standalone;

const _: () = {
    // Forces the generated `Message` impl to be named, so a regression in the
    // override's path emission is a compile error, not a silent skip.
    fn _answer_is_wired(answer: <Standalone as Message>::Answer) -> u32 {
        answer
    }
};

#[factories_actor::protocol(crate = "::factories_actor")]
trait Counting {
    fn bump(&self, msg: Inc);
    fn read(&self, msg: Get);
}

async fn drive(counter: impl Counting) -> u64 {
    counter.bump(Inc).tell().await.expect("tell");
    counter.read(Get).await.expect("ask")
}

#[tokio::test]
async fn crate_override_targets_factories_actor_root() {
    let counter = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Counter { value: 0 })
        .await
        .expect("infallible init");

    // Generated handle methods (the `#[messages]` override).
    counter.inc().tell().await.expect("tell");
    assert_eq!(counter.get().await.expect("ask"), 1);

    // Protocol generic-bound surface (the `#[protocol]` override).
    let typed: TypedActorHandle<Counter> = counter.clone().into();
    assert_eq!(drive(typed).await, 2);
}

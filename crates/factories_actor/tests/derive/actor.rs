//! `#[derive(Actor)]`: feature-dependent defaults, explicit overrides,
//! attribute merging and RTTI names.
//!
//! `Defaulted` and `Customized` double as fixtures for the other modules.

use factories_actor::actor::dispatch::StaticDispatcher;
use factories_actor::actor::work::IntoRunLoopWork;
use factories_actor::actor::{
    Actor, ActorRunLoop, MessageHandler, MessageHandlerContext, StaticOnlyBinder,
};
use factories_actor::declare_static_dispatcher;
use factories_actor::message::Message;
use factories_actor::runtime::concurrent_loop::ConcurrentRunLoop;
use factories_actor::runtime::tokio::TokioMpscActorChannel;
use factories_actor::runtime::lock::{self, UnguardedLock};
use factories_actor::runtime::registry::RegistryBinder;
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::runtime::tokio::{TokioMutexLock, TokioTaskSpawner};
use factories_actor::spawn::ActorLauncher;

use crate::util::assert_type_eq;

// ---------------------------------------------------------------------------
// Defaulted: every component comes from `runtime::defaults`.
// ---------------------------------------------------------------------------

#[derive(Actor)]
pub struct Defaulted {
    pub value: u32,
}

#[derive(Debug, Message)]
#[message(answer = u32)]
pub struct Get;

/// A hand-written handler on a derived actor: the derive must interoperate
/// with the manual path.
impl MessageHandler<Get> for Defaulted {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Defaulted, Get> = declare_static_dispatcher!(Defaulted, Get);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Get, Self, lock::Exclusive>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        async move {
            let (guard, _, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send(guard.value);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Customized: every component overridden, including `Self`-referential types
// and the RTTI debug name.
// ---------------------------------------------------------------------------

// Clone because the spawn machinery fans the init error out to every waiter.
#[derive(Debug, Clone)]
pub struct CustomError;

#[derive(Actor)]
#[actor(
    channel = TokioMpscActorChannel,
    error = CustomError,
    binder = StaticOnlyBinder,
    lock = UnguardedLock<Self>,
    run_loop = SequentialRunLoop<Self>,
    name = "custom-actor",
)]
pub struct Customized {
    pub hits: u32,
}

#[derive(Debug, Message)]
#[message(answer = u32, name = "hit")]
pub struct Hit;

impl MessageHandler<Hit> for Customized {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Customized, Hit> =
        declare_static_dispatcher!(Customized, Hit);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Hit, Self, lock::Exclusive>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        async move {
            let (mut guard, _, answer) = ctx.into_parts();
            guard.hits += 1;
            if let Some(answer) = answer {
                let _ = answer.send(guard.hits);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Partially: one key overridden per attribute, the rest defaulted - exercises
// merging of multiple `#[actor(...)]` attributes.
// ---------------------------------------------------------------------------

#[derive(Actor)]
#[actor(error = CustomError)]
#[actor(name = "partial")]
struct Partially;

// -- Tests ----------------------------------------------------------------------

#[test]
fn defaults_are_the_documented_components() {
    assert_type_eq::<<Defaulted as Actor>::Channel, TokioMpscActorChannel>();
    assert_type_eq::<<Defaulted as Actor>::Error, core::convert::Infallible>();
    assert_type_eq::<<Defaulted as Actor>::RuntimeBinder, RegistryBinder<Defaulted>>();
    assert_type_eq::<<Defaulted as Actor>::LockStrategy, TokioMutexLock<Defaulted>>();
    assert_type_eq::<<Defaulted as Actor>::RunLoop, ConcurrentRunLoop<Defaulted>>();
}

#[test]
fn overrides_replace_components() {
    assert_type_eq::<<Customized as Actor>::Error, CustomError>();
    assert_type_eq::<<Customized as Actor>::RuntimeBinder, StaticOnlyBinder>();
    assert_type_eq::<<Customized as Actor>::LockStrategy, UnguardedLock<Customized>>();
    assert_type_eq::<<Customized as Actor>::RunLoop, SequentialRunLoop<Customized>>();
}

#[test]
fn partial_overrides_merge_with_defaults() {
    assert_type_eq::<<Partially as Actor>::Error, CustomError>();
    assert_type_eq::<<Partially as Actor>::Channel, TokioMpscActorChannel>();
    assert_type_eq::<<Partially as Actor>::RuntimeBinder, RegistryBinder<Partially>>();
}

#[test]
fn rtti_names() {
    assert_eq!(<Defaulted as Actor>::RTTI.name(), "Defaulted");
    assert_eq!(<Customized as Actor>::RTTI.name(), "custom-actor");
    assert_eq!(<Partially as Actor>::RTTI.name(), "partial");
}

#[tokio::test]
async fn derived_default_kit_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Defaulted { value: 7 })
        .await
        .expect("defaulted init is infallible");

    assert_eq!(handle.ask(Get).exchange().await.expect("ask"), 7);
}

#[tokio::test]
async fn spawn_returns_the_generated_typed_handle() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Defaulted { value: 9 })
        .await
        .expect("defaulted init is infallible");

    // The newtype derefs to the full `TypedActorHandle` API.
    assert_eq!(handle.ask(Get).exchange().await.expect("ask"), 9);
}

#[tokio::test]
async fn derived_custom_kit_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Customized { hits: 0 })
        .await
        .expect("customized init is infallible");

    assert_eq!(handle.ask(Hit).exchange().await.expect("ask"), 1);
    assert_eq!(handle.ask(Hit).exchange().await.expect("ask"), 2);
}

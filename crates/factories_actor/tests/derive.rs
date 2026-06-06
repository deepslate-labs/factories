#![cfg(all(
    feature = "derive",
    feature = "dynamic-dispatch",
    feature = "kanal-runtime",
    feature = "tokio-runtime",
    feature = "tokio-lock",
    feature = "tokio-answer"
))]

//! The derive must produce exactly the hand-written shapes in `spawn.rs` and
//! `dynamic_dispatch.rs` - nothing here may rely on a capability that those
//! tests don't spell out manually.

use core::any::TypeId;

use factories_actor::actor::dispatch::StaticDispatcher;
use factories_actor::actor::{
    Actor, ActorInit, IdentityActorInit, MessageHandler, MessageHandlerContext, StaticOnlyBinder,
};
use factories_actor::runtime::concurrent_loop::ConcurrentRunLoop;
use factories_actor::runtime::kanal::SimpleKanalActorChannel;
use factories_actor::runtime::lock::{self, UnguardedLock};
use factories_actor::runtime::registry::RegistryBinder;
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::message::Message;
use factories_actor::runtime::tokio::{TokioMutexLock, TokioTaskSpawner};
use factories_actor::spawn::ActorBuilder;
use factories_actor::declare_static_dispatcher;

// ---------------------------------------------------------------------------
// Defaulted: every component comes from `runtime::defaults`.
// ---------------------------------------------------------------------------

#[derive(Actor)]
struct Defaulted {
    value: u32,
}

#[derive(Debug, Message)]
#[message(answer = u32)]
struct Get;

impl MessageHandler<Get> for Defaulted {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Defaulted, Get> =
        declare_static_dispatcher!(Defaulted, Get);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Get, Self, lock::Exclusive>,
    ) -> impl Future<Output = ()> + 'a {
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
struct CustomError;

#[derive(Actor)]
#[actor(
    channel = SimpleKanalActorChannel,
    error = CustomError,
    binder = StaticOnlyBinder,
    lock = UnguardedLock<Self>,
    run_loop = SequentialRunLoop<Self>,
    name = "custom-actor",
)]
struct Customized {
    hits: u32,
}

#[derive(Debug, Message)]
#[message(answer = u32, name = "hit")]
struct Hit;

impl MessageHandler<Hit> for Customized {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Customized, Hit> =
        declare_static_dispatcher!(Customized, Hit);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Hit, Self, lock::Exclusive>,
    ) -> impl Future<Output = ()> + 'a {
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

// ---------------------------------------------------------------------------
// Tick: message with everything defaulted - the answer type must fall back
// to `()`.
// ---------------------------------------------------------------------------

#[derive(Debug, Message)]
struct Tick;

// ---------------------------------------------------------------------------
// Method-style handlers: #[messages] on an inherent impl block.
// ---------------------------------------------------------------------------

/// Existing message decomposed into handler parameters by field name.
#[derive(Debug, Message)]
#[message(answer = u32)]
struct AddBoth {
    left: u32,
    right: u32,
}

#[factories_actor::messages]
impl Customized {
    /// Stays a plain method (the macro is additive), additionally reachable
    /// through the generated `Touch` message.
    #[handler]
    pub fn touch(&mut self) {
        self.hits += 1;
    }

    /// `&self` receiver: runs under shared access (UnguardedLock supports it).
    #[handler]
    async fn hits_now(&self) -> u32 {
        self.hits
    }

    /// Decomposes the existing `AddBoth` message instead of generating one.
    #[handler(message = AddBoth)]
    fn add_both(&mut self, left: u32, right: u32) -> u32 {
        self.hits += left + right;
        self.hits
    }
}

#[factories_actor::messages]
impl Defaulted {
    /// On a registry-bound actor: the handler must be reachable dynamically
    /// without an explicit `register_dynamic_handler!`.
    #[handler]
    fn add(&mut self, amount: u32) {
        self.value += amount;
    }
}

// -- Tests ----------------------------------------------------------------------

fn assert_type_eq<T: 'static, U: 'static>() {
    assert_eq!(
        TypeId::of::<T>(),
        TypeId::of::<U>(),
        "associated type mismatch"
    );
}

#[test]
fn defaults_are_the_documented_components() {
    assert_type_eq::<<Defaulted as Actor>::Channel, SimpleKanalActorChannel>();
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
    assert_type_eq::<<Partially as Actor>::Channel, SimpleKanalActorChannel>();
    assert_type_eq::<<Partially as Actor>::RuntimeBinder, RegistryBinder<Partially>>();
}

#[test]
fn rtti_names() {
    assert_eq!(<Defaulted as Actor>::RTTI.name(), "Defaulted");
    assert_eq!(<Customized as Actor>::RTTI.name(), "custom-actor");
    assert_eq!(<Partially as Actor>::RTTI.name(), "partial");
}

#[test]
fn message_answer_types() {
    assert_type_eq::<<Tick as Message>::Answer, ()>();
    assert_type_eq::<<Get as Message>::Answer, u32>();
    assert_type_eq::<<Hit as Message>::Answer, u32>();
}

#[test]
fn message_rtti_names() {
    assert_eq!(<Tick as Message>::RTTI.name(), "Tick");
    assert_eq!(<Get as Message>::RTTI.name(), "Get");
    assert_eq!(<Hit as Message>::RTTI.name(), "hit");
}

#[tokio::test]
async fn derived_default_kit_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Defaulted>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Defaulted { value: 7 }).init(),
        )
        .await
        .expect("defaulted init is infallible");

    assert_eq!(handle.ask(Get).exchange().await.expect("ask"), 7);
}

#[test]
fn handler_methods_stay_plain_methods() {
    let mut customized = Customized { hits: 0 };
    customized.touch();
    assert_eq!(customized.hits, 1);
}

#[test]
fn generated_message_shapes() {
    assert_type_eq::<<Touch as Message>::Answer, ()>();
    assert_type_eq::<<HitsNow as Message>::Answer, u32>();
    assert_type_eq::<<Add as Message>::Answer, ()>();
    assert_eq!(<Touch as Message>::RTTI.name(), "Touch");
    assert_eq!(<HitsNow as Message>::RTTI.name(), "HitsNow");
}

#[tokio::test]
async fn method_handlers_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Customized>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Customized { hits: 0 }).init(),
        )
        .await
        .expect("customized init is infallible");

    // Generated unit message, tell-style.
    use factories_actor::actor::channel::ActorChannelSendable;
    handle.tell(Touch).send().await.expect("tell");

    // Generated ask through a `&self`/shared/async handler.
    assert_eq!(handle.ask(HitsNow).exchange().await.expect("ask"), 1);

    // Existing message decomposed into parameters.
    assert_eq!(
        handle
            .ask(AddBoth { left: 2, right: 3 })
            .exchange()
            .await
            .expect("ask"),
        6
    );
}

#[tokio::test]
async fn method_handlers_register_dynamically() {
    use factories_actor::actor::handle::ActorHandle;
    use factories_actor::message::envelope::MessageEnvelope;

    let spawner = TokioTaskSpawner::current();

    let typed = ActorBuilder::<Defaulted>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Defaulted { value: 7 }).init(),
        )
        .await
        .expect("defaulted init is infallible");
    let erased = typed.clone().erase_type();

    // The #[messages]-generated handler registered itself: the message binds
    // dynamically on the registry-bound actor.
    let envelope = MessageEnvelope::new(Add { amount: 5 }, None);
    erased
        .prepare_send_dynamic(envelope)
        .expect("Add must bind dynamically")
        .send()
        .await
        .expect("send must succeed");

    assert_eq!(typed.ask(Get).exchange().await.expect("ask"), 12);
}

#[tokio::test]
async fn derived_custom_kit_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Customized>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Customized { hits: 0 }).init(),
        )
        .await
        .expect("customized init is infallible");

    assert_eq!(handle.ask(Hit).exchange().await.expect("ask"), 1);
    assert_eq!(handle.ask(Hit).exchange().await.expect("ask"), 2);
}

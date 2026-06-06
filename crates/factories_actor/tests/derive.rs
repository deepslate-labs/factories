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

use factories_actor::actor::channel::ActorChannelSendable;
use factories_actor::actor::dispatch::StaticDispatcher;
use factories_actor::actor::handle::{ActorHandle, AnyActorHandle};
use factories_actor::actor::state::{LifecycleState, SharedActorState};
use factories_actor::actor::{
    Actor, ActorContext, ActorInit, IdentityActorInit, MessageHandler, MessageHandlerContext,
    StaticOnlyBinder,
};
use factories_actor::message::Message;
use factories_actor::message::channel::{AnswerSender, answer_channel};
use factories_actor::message::envelope::{MessageEnvelope, SendableEnvelope};
use factories_actor::runtime::concurrent_loop::ConcurrentRunLoop;
use factories_actor::runtime::kanal::SimpleKanalActorChannel;
use factories_actor::runtime::lock::{self, UnguardedLock};
use factories_actor::runtime::registry::RegistryBinder;
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
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

    /// Target of the envelope-forwarding test below.
    #[handler]
    fn probe(&mut self) -> u32 {
        self.value
    }
}

// ---------------------------------------------------------------------------
// Parameter markers: deferred answers (#[answer]), whole-message passthrough
// (#[message]) and sealed envelope forwarding (#[envelope]).
// ---------------------------------------------------------------------------

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Deferring {
    pending: Option<AnswerSender<Defer>>,
}

#[factories_actor::messages]
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

// ---------------------------------------------------------------------------
// Templates: a reusable component bundle, with explicit keys overriding
// individual members.
// ---------------------------------------------------------------------------

struct SequentialSet;

impl factories_actor::runtime::template::ActorTemplate for SequentialSet {
    type Channel = SimpleKanalActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder<A: Actor> = StaticOnlyBinder;
    type LockStrategy<A: Actor> = UnguardedLock<A>;
    type RunLoop<A: Actor> = SequentialRunLoop<A>;
}

#[derive(Actor)]
#[actor(template = SequentialSet)]
struct Templated {
    total: u32,
}

#[factories_actor::messages]
impl Templated {
    #[handler]
    fn bump(&mut self, by: u32) -> u32 {
        self.total += by;
        self.total
    }
}

/// Explicit keys override individual template members.
#[derive(Actor)]
#[actor(template = SequentialSet, error = CustomError, binder = RegistryBinder<Self>)]
struct TemplatedOverride;

// ---------------------------------------------------------------------------
// Actor failure from handlers: die_on_err (forward and consume modes) and
// manual failing through #[context].
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct Boom(u32);

#[derive(Actor)]
#[actor(error = Boom, lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Fragile {
    limit: u32,
}

#[factories_actor::messages]
impl Fragile {
    /// Forward mode: the asker receives the full result, the actor dying is
    /// a side effect of the error.
    #[handler(die_on_err)]
    fn checked_sub(&mut self, value: u32) -> Result<u32, Boom> {
        self.limit = self.limit.checked_sub(value).ok_or(Boom(value))?;
        Ok(self.limit)
    }

    /// Consume mode: the answer is the Ok part, the error only feeds the
    /// actor's death.
    #[handler(die_on_err = consume)]
    fn strict_sub(&mut self, value: u32) -> Result<u32, Boom> {
        self.limit = self.limit.checked_sub(value).ok_or(Boom(value))?;
        Ok(self.limit)
    }

    /// Manual failing through the actor context.
    #[handler]
    fn poison(&mut self, #[context] actor: ActorContext<'_, Self>) {
        actor.fail(Boom(0));
    }
}

/// Forward mode on the concurrent default loop, exercising its error check.
#[derive(Actor)]
#[actor(error = Boom)]
struct FragileConcurrent {
    limit: u32,
}

#[factories_actor::messages]
impl FragileConcurrent {
    #[handler(die_on_err)]
    fn deplete(&mut self, value: u32) -> Result<u32, Boom> {
        self.limit = self.limit.checked_sub(value).ok_or(Boom(value))?;
        Ok(self.limit)
    }
}

#[factories_actor::messages]
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
fn template_supplies_components() {
    assert_type_eq::<<Templated as Actor>::Channel, SimpleKanalActorChannel>();
    assert_type_eq::<<Templated as Actor>::Error, core::convert::Infallible>();
    assert_type_eq::<<Templated as Actor>::RuntimeBinder, StaticOnlyBinder>();
    assert_type_eq::<<Templated as Actor>::LockStrategy, UnguardedLock<Templated>>();
    assert_type_eq::<<Templated as Actor>::RunLoop, SequentialRunLoop<Templated>>();
}

#[test]
fn explicit_keys_override_template_members() {
    assert_type_eq::<<TemplatedOverride as Actor>::Error, CustomError>();
    assert_type_eq::<<TemplatedOverride as Actor>::RuntimeBinder, RegistryBinder<TemplatedOverride>>();
    // Untouched members still come from the template.
    assert_type_eq::<<TemplatedOverride as Actor>::LockStrategy, UnguardedLock<TemplatedOverride>>();
    assert_type_eq::<<TemplatedOverride as Actor>::RunLoop, SequentialRunLoop<TemplatedOverride>>();
}

#[tokio::test]
async fn templated_actor_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Templated>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Templated { total: 0 }).init(),
        )
        .await
        .expect("templated init is infallible");

    assert_eq!(handle.ask(Bump { by: 3 }).exchange().await.expect("ask"), 3);
    assert_eq!(handle.ask(Bump { by: 4 }).exchange().await.expect("ask"), 7);
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
async fn deferred_answer_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Deferring>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Deferring { pending: None }).init(),
        )
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

    let handle = ActorBuilder::<Deferring>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Deferring { pending: None }).init(),
        )
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

    let target = ActorBuilder::<Defaulted>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Defaulted { value: 7 }).init(),
        )
        .await
        .expect("defaulted init is infallible")
        .erase_type();

    let relay = ActorBuilder::<Relay>::builder()
        .build()
        .spawn_ready(&spawner, IdentityActorInit::new(Relay { target }).init())
        .await
        .expect("relay init is infallible");

    // Ask the relay: it forwards the sealed envelope, the target answers the
    // original asker directly.
    assert_eq!(relay.ask(Probe).exchange().await.expect("ask"), 7);
}

/// Wait until the actor's lifecycle reaches `Dead` (bounded).
async fn wait_dead<A: Actor>(state: &SharedActorState<A>) {
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while state.lifecycle() != LifecycleState::Dead {
        assert!(
            tokio::time::Instant::now() < deadline,
            "actor must die after failing"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn die_on_err_forwards_error_and_kills() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Fragile>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Fragile { limit: 10 }).init(),
        )
        .await
        .expect("fragile init is infallible");

    // Healthy ask: full result as the answer, actor stays alive.
    assert_eq!(
        handle
            .ask(CheckedSub { value: 3 })
            .exchange()
            .await
            .expect("ask"),
        Ok(7)
    );
    assert_eq!(handle.state().lifecycle(), LifecycleState::Running);

    // Failing ask: the asker still receives the error - the death is a side
    // effect, observable as lifecycle + actor error.
    assert_eq!(
        handle
            .ask(CheckedSub { value: 100 })
            .exchange()
            .await
            .expect("ask"),
        Err(Boom(100))
    );

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().get_error(), Some(&Boom(100)));
}

#[tokio::test]
async fn die_on_err_consume_closes_answer_and_kills() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Fragile>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Fragile { limit: 10 }).init(),
        )
        .await
        .expect("fragile init is infallible");

    // Consume mode unwraps the answer to the Ok part.
    assert_eq!(
        handle
            .ask(StrictSub { value: 4 })
            .exchange()
            .await
            .expect("ask"),
        6
    );

    // On error the answer channel just closes; the error feeds the death.
    let answer = handle.ask(StrictSub { value: 100 }).exchange().await;
    assert!(
        answer.is_err(),
        "the answer channel must close when the death consumes the error"
    );

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().get_error(), Some(&Boom(100)));
}

#[tokio::test]
async fn context_fail_kills_actor() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Fragile>::builder()
        .build()
        .spawn_ready(&spawner, IdentityActorInit::new(Fragile { limit: 0 }).init())
        .await
        .expect("fragile init is infallible");

    handle.tell(Poison).send().await.expect("tell");

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().get_error(), Some(&Boom(0)));
}

#[tokio::test]
async fn die_on_err_on_concurrent_loop() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<FragileConcurrent>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(FragileConcurrent { limit: 5 }).init(),
        )
        .await
        .expect("fragile init is infallible");

    assert_eq!(
        handle
            .ask(Deplete { value: 100 })
            .exchange()
            .await
            .expect("ask"),
        Err(Boom(100))
    );

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().get_error(), Some(&Boom(100)));
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

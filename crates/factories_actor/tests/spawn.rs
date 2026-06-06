#![cfg(all(
    feature = "kanal-runtime",
    feature = "tokio-runtime",
    feature = "tokio-answer"
))]

use factories_actor::actor::channel::{ActorChannelSendError, ActorChannelSendable};
use factories_actor::actor::dispatch::{AssertSend, StaticDispatcher};
use factories_actor::actor::rtti::ActorRtti;
use factories_actor::actor::state::{LifecycleState, SharedActorState};
use factories_actor::actor::{
    AccessMode, Actor, ActorInit, ActorRunLoop, ActorRunLoopDispatchContext, LockStrategy,
    MessageHandler, MessageHandlerContext, StaticOnlyBinder, ThreadSafe,
};
use factories_actor::runtime::concurrent_loop::ConcurrentRunLoop;
use factories_actor::runtime::kanal::SimpleKanalActorChannel;
use factories_actor::runtime::lock::{self, UnguardedLock};
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::runtime::tokio::TokioTaskSpawner;
use factories_actor::spawn::{
    ActorBuilder, ActorMailbox, ActorTaskSpawner, CreatableChannel, SpawnableRunLoop,
};
use factories_actor::{declare_actor_rtti, declare_message, declare_static_dispatcher};

// ---------------------------------------------------------------------------
// Test actor: Greeter - written fully by hand. This is the manual path the
// future macro layer will generate; if writing this gets painful, the
// framework API is wrong.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct InitError;

struct Greeter {
    greeting: String,
}

// Mode-1 stand-in lock strategy: the actor state behind an async mutex.
struct GreeterLock(tokio::sync::Mutex<Greeter>);

impl LockStrategy<Greeter> for GreeterLock {}

impl From<Greeter> for GreeterLock {
    fn from(value: Greeter) -> Self {
        Self(tokio::sync::Mutex::new(value))
    }
}

/// Exclusive access to the greeter state.
struct Exclusive;

impl AccessMode<Greeter> for Exclusive {
    type Guard<'a> = tokio::sync::MutexGuard<'a, Greeter>;

    fn acquire<'a>(lock_strategy: &'a GreeterLock) -> impl Future<Output = Self::Guard<'a>>
    where
        Self: 'a,
    {
        lock_strategy.0.lock()
    }
}

declare_actor_rtti!(GREETER_RTTI, Greeter);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Greeter {
    const RTTI: &'static ActorRtti = GREETER_RTTI;

    type Channel = SimpleKanalActorChannel;
    type Error = InitError;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = GreeterLock;
    type RunLoop = ConcurrentRunLoop<Greeter>;
}

struct GreeterInit {
    greeting: String,
    fail: bool,
}

impl ActorInit<Greeter> for GreeterInit {
    type Args = (String, bool);

    fn prepare((greeting, fail): Self::Args) -> Self {
        Self { greeting, fail }
    }

    fn init(self) -> impl Future<Output = Result<Greeter, InitError>> {
        core::future::ready(if self.fail {
            Err(InitError)
        } else {
            Ok(Greeter {
                greeting: self.greeting,
            })
        })
    }
}

// -- Messages -----------------------------------------------------------------

#[derive(Debug)]
struct Greet {
    name: String,
}
declare_message!(Greet, String);

impl MessageHandler<Greet> for Greeter {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Greeter, Greet> =
        declare_static_dispatcher!(Greeter, Greet);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Greet, Self, Exclusive>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            let (guard, message, answer) = ctx.into_parts();

            let reply = format!("{} {}", guard.greeting, message.name);
            if let Some(answer) = answer {
                let _ = answer.send(reply);
            }
        }
    }
}

#[derive(Debug)]
struct SetGreeting {
    greeting: String,
}
declare_message!(SetGreeting, ());

impl MessageHandler<SetGreeting> for Greeter {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Greeter, SetGreeting> =
        declare_static_dispatcher!(Greeter, SetGreeting);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, SetGreeting, Self, Exclusive>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            let (mut guard, message, _) = ctx.into_parts();
            guard.greeting = message.greeting;
        }
    }
}

/// A message that cannot cross threads (raw pointer makes it `!Send`).
#[derive(Debug)]
struct NotSendableMsg {
    _marker: *const (),
}
declare_message!(NotSendableMsg, ());

impl MessageHandler<NotSendableMsg> for Greeter {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Greeter, NotSendableMsg> =
        declare_static_dispatcher!(Greeter, NotSendableMsg);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, NotSendableMsg, Self, Exclusive>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            drop(ctx);
        }
    }
}

// -- Builder path ---------------------------------------------------------------

#[tokio::test]
async fn builder_spawn_tell_ask_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Greeter>::builder()
        .build()
        .spawn(&spawner, GreeterInit::prepare(("Hello".into(), false)).init());

    handle
        .tell(SetGreeting {
            greeting: "Servus".into(),
        })
        .send()
        .await
        .expect("tell must succeed");

    let reply = handle
        .ask(Greet {
            name: "Max".into(),
        })
        .exchange()
        .await
        .expect("ask must succeed");

    assert_eq!(reply, "Servus Max");
}

#[tokio::test]
async fn spawn_ready_ok() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Greeter>::builder()
        .build()
        .spawn_ready(&spawner, GreeterInit::prepare(("Hi".into(), false)).init())
        .await
        .expect("init must succeed");

    assert_eq!(handle.state().lifecycle(), LifecycleState::Running);
}

#[tokio::test]
async fn spawn_ready_init_failure() {
    let spawner = TokioTaskSpawner::current();

    let result = ActorBuilder::<Greeter>::builder()
        .build()
        .spawn_ready(&spawner, GreeterInit::prepare(("Hi".into(), true)).init())
        .await;

    assert_eq!(result.err(), Some(InitError));
}

#[tokio::test]
async fn sends_after_init_failure_report_dead() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Greeter>::builder()
        .build()
        .spawn(&spawner, GreeterInit::prepare(("Hi".into(), true)).init());

    // Wait until the failed init has marked the actor dead. The mailbox is
    // dropped before the dead-on-drop guard fires, so `Dead` implies closed.
    assert_eq!(
        handle.state().wait_leave_starting().await,
        LifecycleState::Dead
    );

    let result = handle
        .tell(SetGreeting {
            greeting: "won't arrive".into(),
        })
        .send()
        .await;

    assert!(result.is_err(), "send to dead actor must fail");
}

#[tokio::test]
async fn non_send_message_rejected_at_channel() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Greeter>::builder()
        .build()
        .spawn_ready(&spawner, GreeterInit::prepare(("Hi".into(), false)).init())
        .await
        .expect("init");

    // The kanal channel crosses threads, so the `!Send` message must be
    // rejected at the channel boundary at runtime.
    let result = handle
        .tell(NotSendableMsg {
            _marker: core::ptr::null(),
        })
        .send()
        .await;

    assert!(
        matches!(result, Err(ActorChannelSendError::NotSendable)),
        "expected NotSendable, got {result:?}"
    );
}

// -- Layer-0 hand assembly (everything the builder does, by hand) ---------------

#[tokio::test]
async fn layer0_hand_assembly_matches_builder_behavior() {
    let spawner = TokioTaskSpawner::current();

    // Step 1: channel from options
    let (channel, mailbox) =
        <SimpleKanalActorChannel as CreatableChannel>::create(Default::default());

    // Step 2: shared state
    let shared = SharedActorState::<Greeter>::new();

    // Step 3: run loop future from config + parts
    let fut = <ConcurrentRunLoop<Greeter> as SpawnableRunLoop<Greeter>>::run_with(
        (),
        GreeterInit::prepare(("Moin".into(), false)).init(),
        shared.clone(),
        mailbox,
    );

    // Step 4: spawn + attach the task
    let task = spawner.spawn(fut);
    let _ = shared.attach_task(task);

    // Step 5: assemble the handle
    let handle =
        factories_actor::actor::handle::TypedActorHandle::assemble(channel, StaticOnlyBinder, shared);

    let reply = handle
        .ask(Greet {
            name: "Welt".into(),
        })
        .exchange()
        .await
        .expect("ask must succeed");

    assert_eq!(reply, "Moin Welt");
}

// -- Custom run loop that does NOT implement SpawnableRunLoop -------------------
//
// Proves the assembly contracts are genuinely optional: a hand-crafted
// sequential loop assembled purely from core primitives.

struct Counter {
    count: u32,
}

struct CounterLock(tokio::sync::Mutex<Counter>);

impl LockStrategy<Counter> for CounterLock {}

impl From<Counter> for CounterLock {
    fn from(value: Counter) -> Self {
        Self(tokio::sync::Mutex::new(value))
    }
}

struct CounterExclusive;

impl AccessMode<Counter> for CounterExclusive {
    type Guard<'a> = tokio::sync::MutexGuard<'a, Counter>;

    fn acquire<'a>(lock_strategy: &'a CounterLock) -> impl Future<Output = Self::Guard<'a>>
    where
        Self: 'a,
    {
        lock_strategy.0.lock()
    }
}

declare_actor_rtti!(COUNTER_RTTI, Counter);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Counter {
    const RTTI: &'static ActorRtti = COUNTER_RTTI;

    type Channel = SimpleKanalActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = CounterLock;
    type RunLoop = SequentialLoop;
}

#[derive(Debug)]
struct Increment;
declare_message!(Increment, u32);

impl MessageHandler<Increment> for Counter {
    type AccessMode = CounterExclusive;

    const DISPATCHER: StaticDispatcher<Counter, Increment> =
        declare_static_dispatcher!(Counter, Increment);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Increment, Self, CounterExclusive>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            let (mut guard, _message, answer) = ctx.into_parts();
            guard.count += 1;
            if let Some(answer) = answer {
                let _ = answer.send(guard.count);
            }
        }
    }
}

/// A custom run loop: strictly sequential, no work set, no assembly contract.
struct SequentialLoop;

struct SequentialDispatchContext {
    lock: CounterLock,
}

impl ActorRunLoopDispatchContext<Counter> for SequentialDispatchContext {
    fn lock_strategy(&self) -> &CounterLock {
        &self.lock
    }
}

impl ActorRunLoop<Counter> for SequentialLoop {
    type DispatchContext = SequentialDispatchContext;
    type Demand = ThreadSafe;
}

async fn drive_counter(counter: Counter, mut mailbox: impl ActorMailbox + Send) {
    let ctx = SequentialDispatchContext {
        lock: counter.into(),
    };

    while let Some(message) = mailbox.receive().await {
        // SAFETY: We only ever assemble this loop for `Counter` actors and we are
        //         on the actor task.
        let acquire = unsafe { message.dispatch_onto_loop::<Counter>(&ctx) };

        // SAFETY: `Counter`'s run loop is this loop with a `ThreadSafe` demand,
        //         so every dispatcher reaching this mailbox was demand-checked.
        let work = unsafe { AssertSend::new(acquire) }.await;

        // SAFETY: Same anchor as above.
        unsafe { AssertSend::new(work) }.await;
    }
}

#[tokio::test]
async fn custom_loop_without_assembly_contract() {
    let spawner = TokioTaskSpawner::current();

    let (channel, mailbox) =
        <SimpleKanalActorChannel as CreatableChannel>::create(Default::default());
    let shared = SharedActorState::<Counter>::new();

    // The custom loop manages its own lifecycle reporting.
    let loop_shared = shared.clone();
    let task = spawner.spawn(async move {
        let _guard = loop_shared.dead_on_drop();
        loop_shared.transition_running();
        drive_counter(Counter { count: 0 }, mailbox).await;
    });
    let _ = shared.attach_task(task);

    let handle =
        factories_actor::actor::handle::TypedActorHandle::assemble(channel, StaticOnlyBinder, shared);

    assert_eq!(handle.ask(Increment).exchange().await.expect("ask"), 1);
    assert_eq!(handle.ask(Increment).exchange().await.expect("ask"), 2);
}

// -- Death on dropped handles -----------------------------------------------------

#[tokio::test]
async fn lifecycle_dead_after_handle_dropped() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Greeter>::builder()
        .build()
        .spawn_ready(&spawner, GreeterInit::prepare(("Hi".into(), false)).init())
        .await
        .expect("init");

    let state = handle.state().clone();
    drop(handle);

    // Dropping the last handle drops the channel senders, the mailbox closes,
    // the loop exits and the lifecycle transitions to dead.
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while state.lifecycle() != LifecycleState::Dead {
        assert!(
            tokio::time::Instant::now() < deadline,
            "actor must die after the last handle is dropped"
        );
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
}

// -- Framework sequential set: SequentialRunLoop + UnguardedLock ----------------
//
// The lock-elision counterpart of the hand-rolled SequentialLoop above: the
// framework loop guarantees serialized dispatch (SerializedDispatch), so the
// actor state needs no real lock.

struct Tally {
    total: u32,
}

declare_actor_rtti!(TALLY_RTTI, Tally);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Tally {
    const RTTI: &'static ActorRtti = TALLY_RTTI;

    type Channel = SimpleKanalActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Tally>;
    type RunLoop = SequentialRunLoop<Tally>;
}

#[derive(Debug)]
struct Bump(u32);
declare_message!(Bump, u32);

impl MessageHandler<Bump> for Tally {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Tally, Bump> = declare_static_dispatcher!(Tally, Bump);

    fn handle<'a>(
        ctx: MessageHandlerContext<'a, Bump, Self, lock::Exclusive>,
    ) -> impl Future<Output = ()> + 'a {
        async move {
            let (mut guard, message, answer) = ctx.into_parts();
            guard.total += message.0;
            if let Some(answer) = answer {
                let _ = answer.send(guard.total);
            }
        }
    }
}

#[tokio::test]
async fn sequential_loop_with_unguarded_lock() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Tally>::builder()
        .build()
        .spawn_ready(
            &spawner,
            factories_actor::actor::IdentityActorInit::new(Tally { total: 0 }).init(),
        )
        .await
        .expect("tally init is infallible");

    assert_eq!(handle.ask(Bump(2)).exchange().await.expect("ask"), 2);
    assert_eq!(handle.ask(Bump(3)).exchange().await.expect("ask"), 5);
}

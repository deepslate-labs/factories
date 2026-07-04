#![cfg(all(feature = "tokio-runtime", feature = "tokio-answer"))]

use factories_actor::actor::channel::{ActorChannelSendError, ActorChannelSendable};
use factories_actor::actor::dispatch::{DispatchedActorMessage, StaticDispatcher};
use factories_actor::actor::event::{DefaultMailboxDriver, EventContext, EventDriver};
use factories_actor::actor::extension::ExtensionSet;
use factories_actor::actor::handle::{
    AskError, Calling, MessageCall, TypedActorHandle, WeakActorHandle,
};
use factories_actor::actor::lifecycle::{StopReason, TerminationKind, TerminationReason};
use factories_actor::actor::rtti::ActorRtti;
use factories_actor::actor::state::{LifecycleState, SharedActorState};
use factories_actor::actor::supervision::Terminated;
use factories_actor::actor::work::IntoRunLoopWork;
use factories_actor::actor::{
    AccessMode, Actor, ActorContext, ActorInit, ActorRunLoop, LockStrategy, MessageHandler,
    StaticOnlyBinder,
};
use factories_actor::runtime::concurrent_loop::ConcurrentRunLoop;
use factories_actor::runtime::lock::{self, UnguardedLock};
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::runtime::tokio::TokioMpscActorChannel;
use factories_actor::runtime::tokio::TokioTaskSpawner;
use factories_actor::spawn::{
    ActorLauncher, ActorMailbox, ActorTaskSpawner, CreatableChannel, SpawnableRunLoop,
};
use factories_actor::{declare_actor_rtti, declare_message, declare_static_async_dispatcher};
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

impl LockStrategy<Greeter> for GreeterLock {
    fn into_inner(self) -> Greeter {
        self.0.into_inner()
    }
}

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

    type Channel = TokioMpscActorChannel;
    type Error = InitError;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = GreeterLock;
    type RunLoop = ConcurrentRunLoop<Greeter>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = ();
    type EventDriver = DefaultMailboxDriver;
}

struct GreeterInit {
    greeting: String,
    fail: bool,
}

impl ActorInit<Greeter> for GreeterInit {
    type Fut = core::future::Ready<Result<Greeter, InitError>>;

    fn init(self) -> Self::Fut {
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
        declare_static_async_dispatcher!(Greeter, Greet, |ctx| async move {
            let (guard, message, answer) = ctx.into_parts();

            let reply = format!("{} {}", guard.greeting, message.name);
            if let Some(answer) = answer {
                let _ = answer.send(reply);
            }
        });
}

#[derive(Debug)]
struct SetGreeting {
    greeting: String,
}
declare_message!(SetGreeting, ());

impl MessageHandler<SetGreeting> for Greeter {
    type AccessMode = Exclusive;

    const DISPATCHER: StaticDispatcher<Greeter, SetGreeting> =
        declare_static_async_dispatcher!(Greeter, SetGreeting, |ctx| async move {
            let (mut guard, message, _) = ctx.into_parts();
            guard.greeting = message.greeting;
        });
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
        declare_static_async_dispatcher!(Greeter, NotSendableMsg, |ctx| async move {
            drop(ctx);
        });
}

// -- Builder path ---------------------------------------------------------------

#[tokio::test]
async fn builder_spawn_tell_ask_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default().spawn(
        &spawner,
        GreeterInit {
            greeting: "Hello".into(),
            fail: false,
        },
    );

    handle
        .tell(SetGreeting {
            greeting: "Servus".into(),
        })
        .send()
        .await
        .expect("tell must succeed");

    let reply = handle
        .ask(Greet { name: "Max".into() })
        .exchange()
        .await
        .expect("ask must succeed");

    assert_eq!(reply, "Servus Max");
}

#[tokio::test]
async fn spawn_ready_ok() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(
            &spawner,
            GreeterInit {
                greeting: "Hi".into(),
                fail: false,
            },
        )
        .await
        .expect("init must succeed");

    assert_eq!(handle.state().lifecycle(), LifecycleState::Running);
}

#[tokio::test]
async fn init_closure_constructs_on_the_loop() {
    let spawner = TokioTaskSpawner::current();

    // The closure (captures = `Send` args) crosses to the actor task, the
    // (async) construction runs there.
    let greeting = "Servus".to_string();
    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, || async move {
            tokio::task::yield_now().await;
            Ok(Greeter { greeting })
        })
        .await
        .expect("init must succeed");

    let reply = handle
        .ask(Greet { name: "Max".into() })
        .exchange()
        .await
        .expect("ask must succeed");

    assert_eq!(reply, "Servus Max");
}

#[tokio::test]
async fn spawn_ready_init_failure() {
    let spawner = TokioTaskSpawner::current();

    let result = ActorLauncher::default()
        .spawn_ready(
            &spawner,
            GreeterInit {
                greeting: "Hi".into(),
                fail: true,
            },
        )
        .await;

    assert_eq!(result.err(), Some(InitError));
}

#[tokio::test]
async fn sends_after_init_failure_report_dead() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default().spawn(
        &spawner,
        GreeterInit {
            greeting: "Hi".into(),
            fail: true,
        },
    );

    // Wait until the failed init has marked the actor dead. The mailbox is
    // dropped before the dead-on-drop guard fires, so `Dead` implies closed.
    assert_eq!(
        handle.state().wait_leave_starting().await,
        LifecycleState::Dead
    );

    // The recorded termination reason carries the init failure.
    assert!(
        matches!(
            handle.state().termination_reason(),
            Some(TerminationReason::Failed(InitError))
        ),
        "init failure records Failed(InitError)"
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

    let handle = ActorLauncher::default()
        .spawn_ready(
            &spawner,
            GreeterInit {
                greeting: "Hi".into(),
                fail: false,
            },
        )
        .await
        .expect("init");

    // The tokio mpsc channel crosses threads, so the `!Send` message must be
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

// -- MessageCall vocabulary: handle.call(msg).await / .tell() -------------------

#[tokio::test]
async fn call_await_performs_ask() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default().spawn(
        &spawner,
        GreeterInit {
            greeting: "Hello".into(),
            fail: false,
        },
    );

    // Bare `.await` on a prepared call is an ask: it returns the answer.
    let reply = handle
        .call(Greet { name: "Max".into() })
        .await
        .expect("ask");

    assert_eq!(reply, "Hello Max");
}

#[tokio::test]
async fn call_tell_is_fire_and_forget() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default().spawn(
        &spawner,
        GreeterInit {
            greeting: "Hello".into(),
            fail: false,
        },
    );

    // `.tell()` sends without awaiting a reply; its effect is observed by a
    // following ask.
    handle
        .call(SetGreeting {
            greeting: "Servus".into(),
        })
        .tell()
        .await
        .expect("tell");

    let reply = handle
        .call(Greet { name: "Max".into() })
        .await
        .expect("ask");

    assert_eq!(reply, "Servus Max");
}

// A typed handle written fully by hand, reusing the MessageCall brick. This is
// what the macro layer will generate; if spelling the return type by hand is
// painful, the brick is wrong.
struct GreeterHandle(TypedActorHandle<Greeter>);

impl GreeterHandle {
    fn greet(
        &self,
        name: String,
    ) -> MessageCall<impl Calling<Output = Result<String, AskError>> + use<'_>> {
        self.0.call(Greet { name })
    }

    fn set_greeting(
        &self,
        greeting: String,
    ) -> MessageCall<impl Calling<Output = Result<(), AskError>> + use<'_>> {
        self.0.call(SetGreeting { greeting })
    }
}

#[tokio::test]
async fn manual_typed_handle_reuses_message_call() {
    let spawner = TokioTaskSpawner::current();

    let handle = GreeterHandle(ActorLauncher::default().spawn(
        &spawner,
        GreeterInit {
            greeting: "Hello".into(),
            fail: false,
        },
    ));

    handle
        .set_greeting("Servus".into())
        .tell()
        .await
        .expect("tell");

    let reply = handle.greet("Max".into()).await.expect("ask");

    assert_eq!(reply, "Servus Max");
}

// -- Layer-0 hand assembly (everything the builder does, by hand) ---------------

#[tokio::test]
async fn layer0_hand_assembly_matches_builder_behavior() {
    let spawner = TokioTaskSpawner::current();

    // Step 1: channel from options
    let (channel, mailbox) =
        <TokioMpscActorChannel as CreatableChannel>::create(Default::default());

    // Step 2: shared state
    let shared = SharedActorState::<Greeter>::new(ExtensionSet::new());

    // Step 3: assemble the handle (identity exists before the loop, so the loop
    // can be given the actor's own weak self-reference)
    let handle = TypedActorHandle::assemble(channel, StaticOnlyBinder, shared.clone());

    // Step 4: run loop future from config + parts, handed the weak self-ref
    let fut = <ConcurrentRunLoop<Greeter> as SpawnableRunLoop<Greeter>>::run_with(
        (),
        GreeterInit {
            greeting: "Moin".into(),
            fail: false,
        },
        shared,
        mailbox,
        handle.downgrade(),
    );

    // Step 5: spawn + attach the task
    let task = spawner.spawn(fut);
    let _ = handle.state().attach_task(task);

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
// Proves the assembly contracts are genuinely optional, AND that a hand-rolled
// loop can be written from an *external* crate: it drives the public
// `dispatch_onto_loop` building block, which yields the run loop's converter
// work (here a `Send` future) with its bounds intact - the opaque work cell never
// escapes.
mod custom_loop_scenario {
    use super::*;
    use factories_actor::actor::ActorRunLoopDispatchContext;
    use factories_actor::actor::work::SendFutureConverter;

    struct Counter {
        count: u32,
    }

    struct CounterLock(tokio::sync::Mutex<Counter>);

    impl LockStrategy<Counter> for CounterLock {
        fn into_inner(self) -> Counter {
            self.0.into_inner()
        }
    }

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

        type Channel = TokioMpscActorChannel;
        type Error = core::convert::Infallible;
        type RuntimeBinder = StaticOnlyBinder;
        type LockStrategy = CounterLock;
        type RunLoop = SequentialLoop;
        type TypedHandle = TypedActorHandle<Self>;
        type SharedData = ();
        type EventDriver = DefaultMailboxDriver;
    }

    #[derive(Debug)]
    struct Increment;
    declare_message!(Increment, u32);

    impl MessageHandler<Increment> for Counter {
        type AccessMode = CounterExclusive;

        const DISPATCHER: StaticDispatcher<Counter, Increment> =
            declare_static_async_dispatcher!(Counter, Increment, |ctx| async move {
                let (mut guard, _message, answer) = ctx.into_parts();
                guard.count += 1;
                if let Some(answer) = answer {
                    let _ = answer.send(guard.count);
                }
            });
    }

    /// A custom run loop: strictly sequential, no work set, no assembly contract.
    struct SequentialLoop;

    struct SequentialDispatchContext {
        lock: CounterLock,
        shared: SharedActorState<Counter>,
        self_ref: WeakActorHandle<Counter>,
    }

    impl ActorRunLoopDispatchContext<Counter> for SequentialDispatchContext {
        fn lock_strategy(&self) -> &CounterLock {
            &self.lock
        }

        fn shared_state(&self) -> &SharedActorState<Counter> {
            &self.shared
        }

        fn self_ref(&self) -> &WeakActorHandle<Counter> {
            &self.self_ref
        }
    }

    impl ActorRunLoop<Counter> for SequentialLoop {
        type DispatchContext = SequentialDispatchContext;
        type WorkConverter = SendFutureConverter;
    }

    async fn drive_counter(
        counter: Counter,
        shared: SharedActorState<Counter>,
        self_ref: WeakActorHandle<Counter>,
        mut mailbox: impl ActorMailbox + Send,
    ) {
        let ctx = SequentialDispatchContext {
            lock: counter.into(),
            shared,
            self_ref,
        };

        while let Some(message) = mailbox.receive().await {
            // SAFETY: We only ever assemble this loop for `Counter` actors and we are
            //         on the actor task.
            let work = unsafe { message.dispatch_onto_loop::<Counter>(&ctx) };

            // One erased unit of work: acquire-then-handle, folded by the dispatcher
            // and genuinely `Send` (no reclaim). Drive it to completion.
            work.await;
        }
    }

    #[tokio::test]
    async fn custom_loop_without_assembly_contract() {
        let spawner = TokioTaskSpawner::current();

        let (channel, mailbox) =
            <TokioMpscActorChannel as CreatableChannel>::create(Default::default());
        let shared = SharedActorState::<Counter>::new(ExtensionSet::new());

        // Assemble the handle before the loop, so the loop's dispatch context can
        // carry the actor's own weak self-reference.
        let handle = TypedActorHandle::assemble(channel, StaticOnlyBinder, shared.clone());
        let self_ref = handle.downgrade();

        // The custom loop manages its own lifecycle reporting.
        let loop_shared = shared.clone();
        let task = spawner.spawn(async move {
            let _guard = loop_shared.dead_on_drop();
            loop_shared.transition_running();
            drive_counter(Counter { count: 0 }, loop_shared.clone(), self_ref, mailbox).await;
        });
        let _ = handle.state().attach_task(task);

        assert_eq!(handle.ask(Increment).exchange().await.expect("ask"), 1);
        assert_eq!(handle.ask(Increment).exchange().await.expect("ask"), 2);
    }
}

// -- Supervision: watch -> Terminated push -------------------------------------
//
// A `Supervisor` watches another actor and records the `Terminated` signals
// pushed into its mailbox when a watched actor stops. The watcher is an ordinary
// actor with a `MessageHandler<Terminated>` - no special driver, no special
// extension beyond its own log.

#[derive(Default, Clone)]
struct DeathLog(std::sync::Arc<std::sync::Mutex<Vec<(u64, TerminationKind)>>>);

impl DeathLog {
    fn record(&self, tag: u64, kind: TerminationKind) {
        self.0.lock().expect("death log").push((tag, kind));
    }

    fn snapshot(&self) -> Vec<(u64, TerminationKind)> {
        self.0.lock().expect("death log").clone()
    }
}

struct Supervisor;

declare_actor_rtti!(SUPERVISOR_RTTI, Supervisor);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Supervisor {
    const RTTI: &'static ActorRtti = SUPERVISOR_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Supervisor>;
    type RunLoop = SequentialRunLoop<Supervisor>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = DeathLog;
    type EventDriver = DefaultMailboxDriver;
}

impl MessageHandler<Terminated> for Supervisor {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Supervisor, Terminated> =
        declare_static_async_dispatcher!(Supervisor, Terminated, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, message, _) = ctx.into_parts();
            actor_cx.shared_data().record(message.tag(), message.kind());
        });
}

#[derive(Debug)]
struct GetDeaths;
declare_message!(GetDeaths, Vec<(u64, TerminationKind)>);

impl MessageHandler<GetDeaths> for Supervisor {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Supervisor, GetDeaths> =
        declare_static_async_dispatcher!(Supervisor, GetDeaths, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, _message, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send(actor_cx.shared_data().snapshot());
            }
        });
}

#[tokio::test]
async fn watcher_receives_terminated_on_clean_finish() {
    let spawner = TokioTaskSpawner::current();

    let watcher = ActorLauncher::default()
        .spawn_ready(&spawner, Supervisor)
        .await
        .expect("supervisor init is infallible");

    let watched = ActorLauncher::default()
        .spawn_ready(&spawner, Tally { total: 0 })
        .await
        .expect("tally init is infallible");

    // Unidirectional, explicit: the supervisor watches the tally under tag 42.
    watcher.watch(&watched, 42);

    // Drop the watched's last handle: it drains, finishes, and pushes a
    // `Terminated` into the supervisor's mailbox.
    let watched_state = watched.state().clone();
    drop(watched);
    watched_state.wait_for_terminal().await;

    // The Terminated was enqueued before this query (FIFO), so the supervisor
    // has already recorded it.
    let deaths = watcher.ask(GetDeaths).exchange().await.expect("ask");
    assert_eq!(deaths, vec![(42, TerminationKind::Finished)]);
}

// A watched actor that fails on demand, to exercise the `Failed` kind.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Kaboom;

struct Fragile;

declare_actor_rtti!(FRAGILE_RTTI, Fragile);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Fragile {
    const RTTI: &'static ActorRtti = FRAGILE_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = Kaboom;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Fragile>;
    type RunLoop = SequentialRunLoop<Fragile>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = ();
    type EventDriver = DefaultMailboxDriver;
}

#[derive(Debug)]
struct Detonate;
declare_message!(Detonate, ());

impl MessageHandler<Detonate> for Fragile {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Fragile, Detonate> =
        declare_static_async_dispatcher!(Fragile, Detonate, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, _message, answer) = ctx.into_parts();
            actor_cx.fail(Kaboom);
            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

#[tokio::test]
async fn watcher_receives_terminated_on_failure() {
    let spawner = TokioTaskSpawner::current();

    let watcher = ActorLauncher::default()
        .spawn_ready(&spawner, Supervisor)
        .await
        .expect("supervisor init is infallible");

    let watched = ActorLauncher::default()
        .spawn_ready(&spawner, Fragile)
        .await
        .expect("fragile init is infallible");

    watcher.watch(&watched, 7);

    // Failing the actor drives it to `Dead` even while a handle is held.
    let watched_state = watched.state().clone();
    watched.tell(Detonate).send().await.expect("tell");
    watched_state.wait_for_terminal().await;

    let deaths = watcher.ask(GetDeaths).exchange().await.expect("ask");
    assert_eq!(deaths, vec![(7, TerminationKind::Failed)]);
}

#[tokio::test]
async fn unwatch_stops_delivery() {
    let spawner = TokioTaskSpawner::current();

    let watcher = ActorLauncher::default()
        .spawn_ready(&spawner, Supervisor)
        .await
        .expect("supervisor init is infallible");
    let watched = ActorLauncher::default()
        .spawn_ready(&spawner, Tally { total: 0 })
        .await
        .expect("tally init is infallible");

    watcher.watch(&watched, 1);
    watcher.unwatch(&watched);

    let watched_state = watched.state().clone();
    drop(watched);
    watched_state.wait_for_terminal().await;

    // Any erroneous signal would have been enqueued before this query; the
    // supervisor recorded nothing because the watch was removed.
    let deaths = watcher.ask(GetDeaths).exchange().await.expect("ask");
    assert_eq!(deaths, Vec::new());
}

// A message carrying a handle, so a handler can `ctx.watch` it.
#[derive(Debug)]
struct WatchIt(TypedActorHandle<Tally>);
declare_message!(WatchIt, ());

impl MessageHandler<WatchIt> for Supervisor {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Supervisor, WatchIt> =
        declare_static_async_dispatcher!(Supervisor, WatchIt, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, message, answer) = ctx.into_parts();
            // Watch from inside a handler via the actor's own context - no handle
            // to self required. The carried target handle drops at block end, so
            // it does not keep the target alive.
            actor_cx.watch(&message.0, 99);
            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

#[tokio::test]
async fn ctx_watch_registers_from_handler() {
    let spawner = TokioTaskSpawner::current();

    let watcher = ActorLauncher::default()
        .spawn_ready(&spawner, Supervisor)
        .await
        .expect("supervisor init is infallible");
    let watched = ActorLauncher::default()
        .spawn_ready(&spawner, Tally { total: 0 })
        .await
        .expect("tally init is infallible");

    // The supervisor watches the tally from within its WatchIt handler. The ask
    // resolves once the watch is registered.
    watcher
        .ask(WatchIt(watched.clone()))
        .exchange()
        .await
        .expect("watch-it ask");

    let watched_state = watched.state().clone();
    drop(watched);
    watched_state.wait_for_terminal().await;

    let deaths = watcher.ask(GetDeaths).exchange().await.expect("ask");
    assert_eq!(deaths, vec![(99, TerminationKind::Finished)]);
}

// -- Death on dropped handles -----------------------------------------------------

#[tokio::test]
async fn lifecycle_dead_after_handle_dropped() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(
            &spawner,
            GreeterInit {
                greeting: "Hi".into(),
                fail: false,
            },
        )
        .await
        .expect("init");

    let state = handle.state().clone();
    drop(handle);

    // Dropping the last handle drops the channel senders, the mailbox closes,
    // the loop exits and the lifecycle transitions to dead.
    state.wait_for_terminal().await;
    assert_eq!(state.lifecycle(), LifecycleState::Dead);
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

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Tally>;
    type RunLoop = SequentialRunLoop<Tally>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = ();
    type EventDriver = DefaultMailboxDriver;
}

#[derive(Debug)]
struct Bump(u32);
declare_message!(Bump, u32);

impl MessageHandler<Bump> for Tally {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Tally, Bump> =
        declare_static_async_dispatcher!(Tally, Bump, |ctx| async move {
            let (mut guard, message, answer) = ctx.into_parts();
            guard.total += message.0;
            if let Some(answer) = answer {
                let _ = answer.send(guard.total);
            }
        });
}

#[tokio::test]
async fn sequential_loop_with_unguarded_lock() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Tally { total: 0 })
        .await
        .expect("tally init is infallible");

    assert_eq!(handle.ask(Bump(2)).exchange().await.expect("ask"), 2);
    assert_eq!(handle.ask(Bump(3)).exchange().await.expect("ask"), 5);
}

// A handler that reaches back to the actor itself through `ctx.actor_ref()` and
// self-sends. Proves the self-reference is present during execution and wired to
// the right mailbox.
#[derive(Debug)]
struct KickSelf;
declare_message!(KickSelf, ());

impl MessageHandler<KickSelf> for Tally {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Tally, KickSelf> =
        declare_static_async_dispatcher!(Tally, KickSelf, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, _message, answer) = ctx.into_parts();

            let me = actor_cx
                .actor_ref()
                .expect("a strong self handle exists while a handler runs");
            me.tell(Bump(5)).send().await.expect("self-send");

            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

#[tokio::test]
async fn actor_can_message_itself_via_self_ref() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Tally { total: 0 })
        .await
        .expect("tally init is infallible");

    // The KickSelf handler self-sends Bump(5).
    handle.ask(KickSelf).exchange().await.expect("kick");

    // Sequential processing: the self-sent Bump(5) is drained before this query.
    assert_eq!(handle.ask(Bump(0)).exchange().await.expect("ask"), 5);
}

#[tokio::test]
async fn weak_handle_upgrades_while_alive_and_fails_after_death() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Tally { total: 0 })
        .await
        .expect("tally init is infallible");

    let weak: WeakActorHandle<Tally> = handle.downgrade();

    // While a strong handle is alive, the weak handle upgrades.
    assert!(
        weak.upgrade().is_some(),
        "upgrades while the actor is alive"
    );

    // Dropping the last strong handle closes the mailbox; the actor dies and the
    // identity's strong count hits zero, so the weak handle no longer upgrades.
    let state = handle.state().clone();
    drop(handle);
    state.wait_for_terminal().await;

    assert!(
        weak.upgrade().is_none(),
        "no strong handle survives a dead actor"
    );
}

// -- Event source: a driver coordinating with handlers via shared state --------
//
// `Ticker`'s driver owns the polling decision: it fires `Tick` self-messages
// until a lock-free counter in the *shared state extension* reaches a budget,
// then defers to the mailbox. The `Tick` handler bumps that same counter - so
// the driver and the handlers coordinate through `SharedData`, with no
// actor-lock contention. This exercises the driver-owns-the-mailbox model, raw
// shared access, and the extension all at once.

const TICK_BUDGET: u32 = 3;

#[derive(Default)]
struct TickerShared {
    fired: core::sync::atomic::AtomicU32,
}

struct Ticker {
    total: u32,
}

declare_actor_rtti!(TICKER_RTTI, Ticker);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Ticker {
    const RTTI: &'static ActorRtti = TICKER_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Ticker>;
    type RunLoop = SequentialRunLoop<Ticker>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = TickerShared;
    type EventDriver = TickSource;
}

#[derive(Debug)]
struct Tick;
declare_message!(Tick, ());

impl MessageHandler<Tick> for Ticker {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Ticker, Tick> =
        declare_static_async_dispatcher!(Ticker, Tick, |ctx| async move {
            // Grab the (lock-free) extension before decomposing the context.
            let actor_cx = ctx.actor_context();
            let (mut guard, _message, answer) = ctx.into_parts();
            guard.total += 1;
            actor_cx
                .shared_data()
                .fired
                .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

#[derive(Debug)]
struct GetTotal;
declare_message!(GetTotal, u32);

impl MessageHandler<GetTotal> for Ticker {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Ticker, GetTotal> =
        declare_static_async_dispatcher!(Ticker, GetTotal, |ctx| async move {
            let (guard, _message, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send(guard.total);
            }
        });
}

struct TickSource;

impl From<&Ticker> for TickSource {
    fn from(_actor: &Ticker) -> Self {
        TickSource
    }
}

impl<M: ActorMailbox + Send> EventDriver<Ticker, M> for TickSource {
    fn next<'a>(
        &'a mut self,
        cx: EventContext<'a, Ticker>,
        mailbox: &'a mut M,
    ) -> impl Future<Output = Option<DispatchedActorMessage>> + 'a {
        async move {
            // Drive our own source until the handlers have processed the budget,
            // reading the shared counter the `Tick` handler bumps. Don't even
            // poll the mailbox until then - the actor's lever against starvation.
            if cx
                .shared_data()
                .fired
                .load(core::sync::atomic::Ordering::Acquire)
                < TICK_BUDGET
            {
                return Some(cx.message(Tick));
            }
            // Budget drained: defer to the mailbox.
            mailbox.receive().await
        }
    }
}

#[tokio::test]
async fn event_source_produces_self_messages() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Ticker { total: 0 })
        .await
        .expect("ticker init is infallible");

    // The driver drains its whole budget (coordinating with the handler via the
    // shared counter) before it ever polls the mailbox, so the first query sees
    // every tick.
    let total = handle.ask(GetTotal).exchange().await.expect("ask");
    assert_eq!(
        total, TICK_BUDGET,
        "event source should fire its whole budget"
    );

    // No further ticks once the budget drained.
    let again = handle.ask(GetTotal).exchange().await.expect("ask");
    assert_eq!(again, TICK_BUDGET, "no ticks after the budget drained");
}

// -- Lifecycle hooks: on_start / on_stop ---------------------------------------
//
// A hand-written actor implementing the `Actor::on_start` / `Actor::on_stop`
// hooks directly (the manual path - no derive). Both record into a shared log so
// the test can assert ordering: `on_start` runs before `Running` (so a
// `spawn_ready` waiter sees it), handlers run in between, and `on_stop` runs once
// the loop has drained after the last handle drops.

#[derive(Default, Clone)]
struct LifecycleLog(std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>);

impl LifecycleLog {
    fn record(&self, event: &'static str) {
        self.0.lock().expect("log mutex").push(event);
    }

    fn snapshot(&self) -> Vec<&'static str> {
        self.0.lock().expect("log mutex").clone()
    }
}

struct Hooked;

declare_actor_rtti!(HOOKED_RTTI, Hooked);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Hooked {
    const RTTI: &'static ActorRtti = HOOKED_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Hooked>;
    type RunLoop = SequentialRunLoop<Hooked>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedData = LifecycleLog;
    type EventDriver = DefaultMailboxDriver;

    fn on_start<'a>(
        &'a mut self,
        cx: ActorContext<'a, Self>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        let log = cx.shared_data().clone();
        async move { log.record("start") }
    }

    fn on_stop<'a>(
        self,
        reason: StopReason<'a, Self>,
        cx: ActorContext<'a, Self>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        let log = cx.shared_data().clone();
        let tag = match reason {
            StopReason::Finished => "stop:finished",
            StopReason::Failed(_) => "stop:failed",
        };
        async move { log.record(tag) }
    }
}

#[derive(Debug)]
struct Ping;
declare_message!(Ping, ());

impl MessageHandler<Ping> for Hooked {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Hooked, Ping> =
        declare_static_async_dispatcher!(Hooked, Ping, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, _message, answer) = ctx.into_parts();
            actor_cx.shared_data().record("ping");
            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

#[tokio::test]
async fn lifecycle_hooks_run_in_order() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Hooked)
        .await
        .expect("hooked init is infallible");

    // `on_start` ran before `Running` was observable, so `spawn_ready` already
    // sees it.
    let log = handle.state().shared_data().clone();
    assert_eq!(log.snapshot(), ["start"], "on_start runs before Running");

    handle.ask(Ping).exchange().await.expect("ask");
    assert_eq!(log.snapshot(), ["start", "ping"]);

    // Drop the last handle: the mailbox closes, the loop drains and runs the stop
    // hook before the actor dies.
    let state = handle.state().clone();
    drop(handle);

    state.wait_for_terminal().await;

    assert_eq!(
        log.snapshot(),
        ["start", "ping", "stop:finished"],
        "on_stop runs on a clean drain with the Finished reason"
    );
    assert!(
        matches!(
            state.termination_reason(),
            Some(TerminationReason::Finished)
        ),
        "a clean drain records the Finished termination reason"
    );
}

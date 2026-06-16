# Under the Hood

Every chapter so far has leaned on a macro. `#[derive(Actor)]` filled in the
machinery, `#[messages]` turned methods into messages, `#[protocol]` minted an
erased handle. The introduction promised that none of this was magic - that each
convenience expands to code you could have written by hand, against the same
public API. This chapter cashes that promise.

We'll start with the rule that keeps it true, walk the three tiers the crate is
organized around, and then build an actor from scratch - no derives, just the
public primitives - and watch it behave identically to one the builder spawned.

## The lego rule

The framework is a box of lego bricks. You combine the pre-made parts the easy
way, or you craft your own. The one rule that holds the whole design together:

> **Every convenience must decompose into public primitives.** The builder may
> not do anything a hand-assembler can't do with public API. The derive macros
> may not reach for anything you can't reach for. No private shortcuts.

This isn't a slogan - it's enforced by construction. If the `ActorLauncher`
builder needs a capability, that capability is public API *first*. If
`#[derive(Actor)]` needs to emit something, that something is a public macro or
trait you can call yourself. The proof you're about to see - a hand-written
actor that round-trips through the same channel, run loop, and handle as a
derived one - is a test in the crate (`tests/spawn.rs`). When it stops compiling,
the lego rule has been broken, and that's a bug.

A corollary worth holding onto: **assembly bounds never leak into the core
contract.** The traits that define *what an actor is* carry no `Send` bounds and
no spawn requirements; those appear only where you actually assemble a running
actor. A fully custom run loop ignores the assembly tier entirely and still
dispatches messages. We'll see that too.

## The three tiers

The crate's modules mirror three layers, from the contract an actor must satisfy
down to the concrete parts you spawn it with. You met the module names back in
[Getting Started](02-getting-started.md); here is what lives in each.

### Tier 1 - the core contract (`factories::actor`)

*What it means to be an actor, and how a message reaches it.* This is the stud
geometry - not optional, present even in `no_std`.

At its center is the `Actor` trait, and the surprising thing about it is how much
it *names*. An actor isn't a bag of behavior; it's a declaration of parts:

```rust
pub unsafe trait Actor: 'static {
    const RTTI: &'static ActorRtti;

    type Channel: ActorChannel;          // how messages are transported
    type Error;                          // what a failed init / handler yields
    type RuntimeBinder: ActorRuntimeBinder; // the dynamic-dispatch seam
    type LockStrategy: LockStrategy<Self> + 'static; // how state is guarded
    type RunLoop: ActorRunLoop<Self>;    // how messages are pulled and run
    type TypedHandle: From<TypedActorHandle<Self>>; // the handle callers hold
    type SharedStateExtension: Default + Send + Sync; // out-of-band shared data
    type EventDriver: for<'a> From<&'a Self>; // where work comes from
}
```

Every one of those associated types is load-bearing. `LockStrategy` and the run
loop's dispatch context feed the function-pointer devirtualization that makes a
typed send a direct call; `Channel` feeds the handles; `RuntimeBinder` feeds
dynamic dispatch; `Error` feeds the shared lifecycle state; `EventDriver` is the
source of work we met in [Event Sources](08-event-sources.md). The derive macros
fill these in for you - but here they are, plain, for you to fill in yourself.

The trait is `unsafe` because `RTTI` must describe *exactly this type*; the
`declare_actor_rtti!` macro (below) is how you discharge that obligation safely.

Tier 1 also holds [`MessageHandler<M>`](03-messages-and-handlers.md) (one impl
per actor/message pair, carrying a `const DISPATCHER`), the `AccessMode` /
`LockStrategy` vocabulary, the handles (`TypedActorHandle`, `WeakActorHandle`,
the erased `AnyActorHandle`), the dispatch machinery, identity, and
`SharedActorState`.

### Tier 2 - the assembly contract (`factories::spawn`)

*What it takes to generically construct a running actor.* This tier is opt-in:
its traits all share one symmetric shape - **config in, assembled part out.**

- **`CreatableChannel`** - a channel you can build from options, handing back a
  mailbox: `create(options) -> (channel, mailbox)`.
- **`ActorMailbox`** - the *receive* side of a channel, touched only by run
  loops. `receive()` returns the next delivery (and is `+ Send`, so the loop can
  migrate across threads).
- **`SpawnableRunLoop`** - a run loop you can build from a config plus the
  assembled parts and drive to completion: `run_with(config, init, shared,
  mailbox, self_ref)`.
- **`ActorTaskSpawner`** - the one seam where the framework touches an executor:
  `spawn(fut) -> ActorTaskHandle`. `TokioTaskSpawner` is one implementation.
- **`ActorLauncher`** - the builder. It is *nothing but* a consumer of the three
  contracts above. Its `spawn` performs five public steps and not one private
  one.

### Tier 3 - the concrete parts (`factories::runtime`)

*The bricks in the box.* All feature-gated, all swappable:

- `TokioMpscActorChannel` and `TokioTaskSpawner` (feature `tokio-runtime`) - the
  default transport and executor.
- `ConcurrentRunLoop` (a `FuturesUnordered` work-set loop) and
  `SequentialRunLoop` (strictly one dispatch at a time) - the two run loops from
  [Concurrency](05-concurrency.md).
- The lock strategies: `UnguardedLock` (core, dep-free - the lock-elision
  partner of `SequentialRunLoop`) and the tokio `TokioMutexLock` /
  `TokioRwLock` (feature `tokio-lock`), plus the `Exclusive` / `Shared` access
  modes in `runtime::lock`.

When `#[derive(Actor)]` picks "sensible defaults," it is choosing tier-3 parts:
`TokioMpscActorChannel`, `SequentialRunLoop`, `UnguardedLock`. Nothing more
magic than that.

## The manual path

Now we build an actor with no macros at all. The example is `Greeter`: it holds a
greeting string, answers `Greet { name }` with a formatted message, and accepts
`SetGreeting` to change it. This is the exact shape `#[derive(Actor)]` and
`#[messages]` generate - written out by hand.

### Declaring the actor

We need the actor struct, a lock strategy, and the `Actor` impl. We'll use
`UnguardedLock` + `SequentialRunLoop` - the serial-by-default set - so there's no
real mutex to write:

```rust
use factories::prelude::*;
use factories::actor::rtti::ActorRtti;
use factories::actor::{Actor, MessageHandler, StaticOnlyBinder};
use factories::actor::dispatch::StaticDispatcher;
use factories::actor::event::DefaultMailboxDriver;
use factories::runtime::lock::{self, UnguardedLock};
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::{TokioMpscActorChannel, TokioTaskSpawner};
use factories::{declare_actor_rtti, declare_message, declare_static_async_dispatcher};

struct Greeter {
    greeting: String,
}

declare_actor_rtti!(GREETER_RTTI, Greeter);

// SAFETY: the RTTI is declared for exactly this type.
unsafe impl Actor for Greeter {
    const RTTI: &'static ActorRtti = GREETER_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Greeter>;
    type RunLoop = SequentialRunLoop<Greeter>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;
}
```

`declare_actor_rtti!` builds the static run-time type information the `unsafe
impl` points at - that's the obligation that makes the `unsafe` sound.
`StaticOnlyBinder` is a binder that never binds dynamically (static dispatch
only) - what the derive picks when the `dynamic-dispatch` feature is off; with it
on (as in the default feature set) the derive defaults to `RegistryBinder`
instead, the subject of [Dynamic Dispatch and Binders](11-dynamic-dispatch.md).
`DefaultMailboxDriver` is the event driver that just reads the mailbox - what an
actor with no `#[event_source]` gets.

### Declaring a message and its handler

A message needs two things: a `declare_message!` to register its type and answer,
and a `MessageHandler<M>` impl carrying the dispatcher. The dispatcher is built by
`declare_static_async_dispatcher!`, which demand-checks the handler future against
the run loop right at the declaration site - a `!Send` handler on a thread-safe
loop is a compile error *here*, where the future's concrete type is known.

```rust
struct Greet {
    name: String,
}
declare_message!(Greet, String); // answer type is `String`

impl MessageHandler<Greet> for Greeter {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Greeter, Greet> =
        declare_static_async_dispatcher!(Greeter, Greet, |ctx| async move {
            let (guard, message, answer) = ctx.into_parts();
            let reply = format!("{} {}", guard.greeting, message.name);
            if let Some(answer) = answer {
                let _ = answer.send(reply);
            }
        });
}

struct SetGreeting {
    greeting: String,
}
declare_message!(SetGreeting, ());

impl MessageHandler<SetGreeting> for Greeter {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Greeter, SetGreeting> =
        declare_static_async_dispatcher!(Greeter, SetGreeting, |ctx| async move {
            let (mut guard, message, _) = ctx.into_parts();
            guard.greeting = message.greeting;
        });
}
```

This is precisely what `#[messages]` emits. `ctx.into_parts()` hands you the lock
guard (your `&mut Greeter` or `&Greeter`, per the `AccessMode`), the message, and
the optional answer sender. The `if let Some(answer)` dance is the auto-reply the
macro writes for you. There's no hidden step here - a method-style handler is
sugar over exactly this `MessageHandler` impl.

You can spawn this actor with the ordinary builder, and it works just like a
derived one:

```rust
let greeter = ActorLauncher::default()
    .spawn_ready(&TokioTaskSpawner::current(), Greeter { greeting: "Hello".into() })
    .await?;

greeter.tell(SetGreeting { greeting: "Servus".into() }).send().await?;
let reply = greeter.ask(Greet { name: "Max".into() }).exchange().await?;
assert_eq!(reply, "Servus Max");
```

### Hand-assembling the running actor

The builder is the convenience; here is the primitive it decomposes into. The
`spawn` step is exactly five public calls - channel, shared state, handle, run
loop, task:

```rust
use factories::actor::state::SharedActorState;
use factories::spawn::{CreatableChannel, SpawnableRunLoop};

let spawner = TokioTaskSpawner::current();

// 1. Build the channel from options; get back the mailbox.
let (channel, mailbox) =
    <TokioMpscActorChannel as CreatableChannel>::create(Default::default());

// 2. Allocate the shared lifecycle state.
let shared = SharedActorState::<Greeter>::new();

// 3. Assemble the handle. Identity exists *before* the loop, so the loop can be
//    handed the actor's own weak self-reference.
let handle = TypedActorHandle::assemble(channel, StaticOnlyBinder, shared.clone());

// 4. Build the run-loop future from config + parts + the weak self-ref.
let fut = <SequentialRunLoop<Greeter> as SpawnableRunLoop<Greeter>>::run_with(
    (),                                    // loop config
    Greeter { greeting: "Moin".into() },   // the initializer (an actor is its own ActorInit)
    shared,
    mailbox,
    handle.downgrade(),
);

// 5. Spawn the future and attach the task to the identity.
let task = spawner.spawn(fut);
let _ = handle.state().attach_task(task);

let reply = handle.ask(Greet { name: "Welt".into() }).exchange().await?;
assert_eq!(reply, "Moin Welt");
```

That's the whole builder, unrolled. `ActorLauncher::default().spawn(&spawner,
init)` does these five things and returns the handle; `spawn_ready` does the same
and then awaits the lifecycle leaving `Starting`. The `init` is an `ActorInit`,
and an actor is its own initializer - there's a blanket `impl<A: Actor>
ActorInit<A> for A` - so handing over `Greeter { .. }` directly is enough. When
construction itself needs to run *on* the task (fallible or async setup), you pass
a construction closure `|| async { Ok(..) }` instead; the initializer crosses onto
the task and its `init` runs there, so the actor is built where it lives. A
hand-assembled actor and a builder-spawned one are the same actor.

## A custom run loop

Tier 2 is opt-in, and here's the proof. A run loop doesn't have to implement
`SpawnableRunLoop` - it only has to drive the public dispatch building block. You
can write one in an external crate that has never heard of `ActorLauncher`.

The core primitive is `DispatchedActorMessage::dispatch_onto_loop`, which takes
your loop's dispatch context and yields one opaque unit of work - acquire the
lock, run the handler - with its `Send`-ness intact. Your loop only has to
provide a context (the lock strategy, the shared state, the self-reference) and
await the work:

```rust
use factories::actor::{ActorRunLoop, ActorRunLoopDispatchContext};
use factories::actor::work::SendFutureConverter;
use factories::spawn::ActorMailbox;

struct SequentialLoop;

struct MyDispatchContext {
    lock: CounterLock,
    shared: SharedActorState<Counter>,
    self_ref: WeakActorHandle<Counter>,
}

impl ActorRunLoopDispatchContext<Counter> for MyDispatchContext {
    fn lock_strategy(&self) -> &CounterLock { &self.lock }
    fn shared_state(&self) -> &SharedActorState<Counter> { &self.shared }
    fn self_ref(&self) -> &WeakActorHandle<Counter> { &self.self_ref }
}

impl ActorRunLoop<Counter> for SequentialLoop {
    type DispatchContext = MyDispatchContext;
    type WorkConverter = SendFutureConverter;
}

async fn drive(ctx: MyDispatchContext, mut mailbox: impl ActorMailbox + Send) {
    while let Some(message) = mailbox.receive().await {
        // SAFETY: we only assemble this loop for `Counter`, on the actor task.
        let work = unsafe { message.dispatch_onto_loop::<Counter>(&ctx) };
        work.await; // one acquire-then-handle unit, driven to completion
    }
}
```

This loop is genuinely sequential and carries no work set. Because it doesn't
implement `SpawnableRunLoop`, you can't hand it to the builder - but you don't
need to. Assemble the handle and shared state by hand, spawn `drive(..)` as a
task, attach it, and you have a running actor on a run loop the framework has
never seen. (See the `custom_loop_scenario` module in `tests/spawn.rs` for the
full, compiling version, including the `dead_on_drop` guard that reports the
loop's exit to the lifecycle.)

## The dynamic-dispatch seam

One tier-1 part we named but never exercised is the `RuntimeBinder` - the seam
that lets an *erased* handle resolve a runtime-chosen message to a handler. The
default, `StaticOnlyBinder`, never binds (static dispatch only); the
`dynamic-dispatch` feature swaps in a `RegistryBinder` backed by a global handler
registry. That whole story - binders, the registry, link-time registration, and
how [Protocols](09-protocols.md) ride on it - has a chapter to itself, so we leave
it there rather than sketch it twice.

## What you've seen

Beneath every macro is an ordinary trait impl; beneath the builder is five public
calls. The three tiers - core contract, assembly contract, concrete parts - are a
deliberate separation of *what an actor is* from *how it's built* from *which
bricks you chose*. Pick different bricks, and nothing above them changes.

The one seam we only pointed at - the `RuntimeBinder` - is where we head next.
[Dynamic Dispatch and Binders](11-dynamic-dispatch.md) follows the dynamic path
from an erased handle down to the registry that resolves it; then
[`no_std` and Choosing Your Parts](12-no-std-and-parts.md) closes the tour by
taking the tier-3 box apart.

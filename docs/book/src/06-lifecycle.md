# Lifecycle

So far our actors have simply appeared, served some messages, and vanished when
we dropped the last handle. That's the happy path, and most of the time it's all
you need. But an actor has a *life*: it is born when you spawn it, it runs while
anyone holds a handle to it, and it dies when the last handle is gone - or when
something goes wrong. This chapter is about the two ends of that arc. We'll look
at the **hooks** that run at birth and death, and then at the **spawn** calls
that bring an actor into being and let you wait for it to be ready.

We'll use the heartbeat actor as our running illustration. You can follow along
by running it:

```sh
cargo run -p factories --example heartbeat
```

## The two hooks

A derived actor can declare two lifecycle hooks right inside its `#[messages]`
block, alongside its handlers:

- `#[on_start]` runs **before** the actor begins processing messages.
- `#[on_stop]` runs **after** the loop has drained, just before the actor is
  dropped.

They are not handlers - you never send them, and they don't generate calling
methods. They are points in the actor's life where your code gets to run. Here
they are on `Heartbeat`:

```rust
use factories::actor::lifecycle::StopReason;
use factories::actor::ActorContext;
use factories::prelude::*;

#[factories::messages]
impl Heartbeat {
    /// Runs before `Running` is observable - the actor announces itself.
    #[on_start]
    async fn announce(&mut self, _cx: ActorContext<'_, Self>) {
        println!("[on_start] heartbeat coming online");
    }

    /// Runs once on a clean drain. `StopReason` distinguishes a graceful
    /// finish from a failure, so the hook can react accordingly.
    #[on_stop]
    async fn farewell(self, reason: StopReason<'_, Self>, _cx: ActorContext<'_, Self>) {
        match reason {
            StopReason::Finished => {
                println!("[on_stop] graceful shutdown after {} beats", self.beats)
            }
            StopReason::Failed(_) => println!("[on_stop] shutting down after a failure"),
        }
    }
}
```

`StopReason` and `ActorContext` are both in the prelude, so `use
factories::prelude::*;` is all the hooks need - no extra imports. (`StopReason`
also lives at its canonical path, `factories::actor::lifecycle::StopReason`, if you
ever want to name it explicitly.)

Notice the receivers, because they are the heart of the design.

### `on_start` takes `&mut self`

The start hook borrows the actor mutably, exactly like a `&mut self` handler.
The actor already exists - initialization has produced the value - and the hook
gets the first crack at it before any message is dispatched. This is where you
register a watch ([Supervision](07-supervision.md)), kick off a self-send to
prime an event loop ([Event Sources](08-event-sources.md)), or log that you're
online.

The crucial guarantee is *ordering*: **`on_start` completes before the actor's
state becomes `Running`.** A caller that waits for readiness (we'll get to
`spawn_ready` below) is guaranteed to see the effects of `on_start` before its
first message is served. From the lifecycle test:

```rust
let handle = ActorLauncher::default()
    .spawn_ready(&spawner, Hooked)
    .await
    .expect("hooked init is infallible");

// `on_start` ran before `Running` became observable.
assert_eq!(handle.state().extension().snapshot(), ["start"]);

handle.ping().await.expect("ask");
assert_eq!(handle.state().extension().snapshot(), ["start", "ping"]);
```

### `on_stop` takes `self` by value

The stop hook is different, and it's worth dwelling on. It takes `self` - the
whole actor, by value, *not* a reference. Throughout the actor's life its state
lived inside a lock (an `UnguardedLock` by default; see
[Concurrency](05-concurrency.md)). When the loop drains, there are no more
handlers to run, so the framework reclaims the actor out of its lock and hands
it to you whole. You own it. You can move fields out of it, send them somewhere,
flush a buffer, close a file - whatever a clean shutdown means for this actor.

Along with `self`, `on_stop` receives a `StopReason`:

```rust
pub enum StopReason<'a, A: Actor + ?Sized> {
    /// The mailbox closed and the loop drained without failing.
    Finished,
    /// A handler - or the start hook - failed the actor.
    Failed(&'a A::Error),
}
```

`Finished` means a graceful end: every handle was dropped, the mailbox closed,
and the loop wound down on its own. `Failed(e)` means a handler (or `on_start`
itself) called `cx.fail(..)` and the actor is dying because of it - and the hook
gets a reference to the error that killed it, so it can react. The same
shutdown code runs either way; the reason just tells it *why*.

This is the same `cx.fail` you met in [State, Answers, and Errors](04-state-answers-errors.md):
failing the actor is how a handler signals "I cannot continue." It's distinct
from the `Aborted` outcome a *panic* or task abort produces - a panic unwinds the
loop, so `on_stop` does not run for it. Watchers can still observe an abort; see
[Supervision](07-supervision.md) for the `TerminationKind` they receive.

> **A note on defaults.** Both hooks are optional. If you don't write them, the
> framework supplies a no-op, and there is *zero* cost - no empty future to poll,
> no allocation. You only pay for a hook when you write one.

### Hooks may `die_on_err`

A start hook can fail. Maybe it opens a connection, reads a config file, or
registers with a coordinator, and any of those can go wrong. Mark the hook with
`die_on_err` and return a `Result`: if it returns `Err`, the actor is failed
(the error is routed to `cx.fail`) and **startup is aborted** - the run loop
never begins processing messages.

```rust
#[derive(Actor)]
#[actor(error = StartBoom)]
struct FailingStart;

#[factories::messages]
impl FailingStart {
    #[on_start(die_on_err)]
    async fn start(&mut self, _cx: ActorContext<'_, Self>) -> Result<(), StartBoom> {
        Err(StartBoom)
    }
}
```

This is the same `die_on_err` sugar handlers use ([State, Answers, and
Errors](04-state-answers-errors.md)) - it just means "treat an `Err` from this
method as a reason to fail the actor." When startup is aborted this way, a caller
waiting on `spawn_ready` gets the error back, which is exactly what the next
section is about.

## Spawning

Every actor in this book has been born the same way: `ActorLauncher::default()`,
then a spawn call. `ActorLauncher` is the builder - it carries the channel
options, the run-loop config, and the runtime binder, all default-able, so
`::default()` gives you a fully-configured launcher with nothing to fill in.
There are two ways to spawn from it, and the difference is *when control returns
to you*.

### `spawn` - fire and forget

`spawn` is the primitive. It assembles the actor's parts, hands the run loop to
the task spawner, and **returns the handle immediately** - before the actor has
finished initializing, before `on_start` has run, before the actor is `Running`.

```rust
let handle = ActorLauncher::default()
    .spawn(&TokioTaskSpawner::current(), Heartbeat { beats: 0 });
// `handle` is live right now; the actor may still be in `Starting`.
```

This is fine, because of the mailbox. Messages you send to a still-starting
actor simply **queue**; they're served once initialization completes and the
loop starts turning. You never have to coordinate "is it ready yet?" by hand.

But what if initialization *fails*? With `spawn` you don't find out at the call
site - `spawn` already returned. Instead, the error is recorded in the actor's
shared state, the mailbox closes, and your sends start coming back as errors:

```rust
let handle = ActorLauncher::default().spawn(&spawner, FailingInit { .. });

// The failed init marks the actor dead; subsequent sends report it.
let result = handle.some_message().tell().await;
assert!(result.is_err(), "send to a dead actor fails");
```

Fire-and-forget is the right tool when you're spawning a fleet of workers, or
when the caller genuinely doesn't need to know the moment an actor is live - the
queue-and-report-later behavior is enough.

### `spawn_ready` - wait for the actor to be live

Often, though, you *do* want to know - your next step depends on the actor being
up, or initialization can fail and you'd rather learn that now than discover it
through a bounced message. That's `spawn_ready`. It does everything `spawn` does,
then **awaits the actor leaving `Starting`** and reports what happened:

```rust
let handle = ActorLauncher::default()
    .spawn_ready(&TokioTaskSpawner::current(), Heartbeat { beats: 0 })
    .await?;
```

Its return type is `Result<TypedActorHandle<A>, A::Error>`:

- Init succeeded and `on_start` ran → the lifecycle is `Running`, and you get
  `Ok(handle)`. Because `on_start` finishes before `Running` is observable,
  the actor you receive is fully warmed up.
- Init (or a `die_on_err` start hook) failed → you get `Err(error)`, the very
  error the actor recorded. This is why the `FailingStart` test above could
  assert `result.err() == Some(StartBoom)`.

That's the whole difference: `spawn` is the non-blocking primitive that always
hands back a handle; `spawn_ready` layers a readiness wait on top and surfaces
init failures as a `Result`. Reach for `spawn_ready` whenever the next line of
code assumes the actor is alive - which is most of the time, and why it's the
call you've seen in every chapter so far.

### What the launcher will accept

Look closely at the spawn calls and you'll notice we passed a plain `Heartbeat`
value - not some wrapper, not an "init" object. That works because both spawn
methods take their initializer through one flexible front door,
`IntoActorInit`, which accepts three shapes:

```rust
// 1. A plain actor value - the common case. The actor *is* its own initializer.
ActorLauncher::default().spawn_ready(&spawner, Heartbeat { beats: 0 }).await?;

// 2. A bare async closure - construction runs *on the actor's task*.
ActorLauncher::default()
    .spawn_ready(&spawner, || async move {
        let greeting = load_greeting().await?;
        Ok(Greeter { greeting })
    })
    .await?;

// 3. A custom initializer type - a value implementing `ActorInit<A>`,
//    for when construction needs its own arguments or fallible setup.
ActorLauncher::default().spawn_ready(&spawner, GreeterInit { .. }).await?;
```

The closure form is worth understanding. The closure crosses onto the actor's
task, and the `async` block it returns runs *there* - so the actor is constructed
on the thread it will live on, and any async work that construction needs (a
`.await` on a connection, a file read) happens off the spawning thread. Whatever
the closure captures becomes the constructor's "arguments." That's why the
launcher takes an initializer and not just a ready-made actor: it lets
construction itself be asynchronous and fallible, while keeping the spawn call a
single expression. A plain value is the natural default; reach for a closure or a
custom `ActorInit` only when construction has real work to do.

## Putting it together

The heartbeat example walks the full arc. It spawns ready (so it knows the actor
is live), lets the actor run for a moment, asks it a question, and then drops the
last handle and waits for the end:

```rust
let handle = ActorLauncher::default()
    .spawn_ready(&TokioTaskSpawner::current(), Heartbeat { beats: 0 })
    .await?;                                  // on_start has run; actor is Running

tokio::time::sleep(Duration::from_millis(200)).await;
println!("main: observed {} beats", handle.beats().await?);

// Drop the last handle: the mailbox closes, the loop drains, and `on_stop`
// runs with `StopReason::Finished`.
let state = handle.state().clone();
drop(handle);
state.wait_for_terminal().await;
```

Birth (`spawn_ready` + `on_start`), life (the `beats` ask), and a graceful death
(`drop` → drain → `on_stop` with `Finished`). That's the complete lifecycle of a
self-contained actor.

But an actor rarely lives alone. The moment one actor's death needs to *matter*
to another - a supervisor that restarts a failed child, a coordinator that
cleans up after a worker - we need a way for one actor to learn that another has
stopped. That's the `Terminated` signal, and it's where we go next, in
[Supervision](07-supervision.md).

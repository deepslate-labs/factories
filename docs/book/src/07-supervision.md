# Supervision

Actors die. A connection drops, a worker hits an unrecoverable error, a task
finishes its job and has nothing left to do. In a system built from many actors,
the interesting question is rarely *that* one died - it's *who needs to know*.
A pool wants to replace a worker that failed. A session manager wants to clean
up after a connection that closed. A coordinator wants to tear down its children
when one of them gives up.

`factories` gives you exactly one primitive for this, and - true to the rule
from the [Introduction](01-introduction.md) - it is as plain as it can be. One
actor *watches* another. When the watched actor terminates, an ordinary
`Terminated` message lands in the watcher's mailbox, and the watcher handles it
like any other message. There is no supervisor trait to implement, no special
run loop, no restart policy baked into the framework. **Any actor can be a
watcher** - if it can handle a `Terminated` message, it can supervise.

This chapter's running example is `supervision`. You can follow along with it:

```sh
cargo run -p factories --example supervision
```

## Watching an actor

To start watching, you call `watch` on the watcher's handle, passing it a
handle to the actor you want to watch and a `tag`:

```rust
supervisor.watch(&task, 42);
```

That's the whole API. From this point on, when `task` terminates, the
`supervisor` will receive a `Terminated` message.

Two properties of `watch` matter, and they follow directly from the framework's
"no magic" stance:

- **It is unidirectional.** `supervisor.watch(&task, …)` sets up a one-way
  subscription: the supervisor learns about the task's death, but the task
  learns nothing about the supervisor. If you want the relationship to go both
  ways, you call `watch` twice, once in each direction. There is no implicit
  pairing.
- **It is weak - it keeps neither actor alive.** The watch does not hold a
  strong handle to anyone. The watcher is held *weakly* on the watched side, so
  registering a watch never extends the watched actor's life, and it never keeps
  the watcher alive either. A watch is a notification wire, not an ownership
  edge. (Recall from earlier chapters that an actor stays alive as long as a
  strong handle to it exists; `watch` deliberately does not add one.)

The `tag: u64` is a correlation key, and it is yours to choose. It means nothing
to the framework - it simply rides along and comes back to you on the
`Terminated` signal. That is what lets one supervisor tell its children apart:
watch each child under a different tag, and when a death arrives you read the
tag to know *which* child it was, without having to compare handles or
addresses. In the example we use `42` for the single task; a pool would
typically use the worker's index or id.

## Handling `Terminated`

A watcher is just an actor that handles the `Terminated` message. The signal is
ordinary - it goes through the mailbox in FIFO order alongside every other
message, and you write a normal handler for it. The one wrinkle is that you ask
for the *whole message* rather than destructured fields, because `Terminated`
exposes its payload through methods, not public fields:

```rust
use factories::prelude::*;

#[derive(Actor)]
#[actor(shared = DeathLog)]
struct Supervisor;

#[factories::messages]
impl Supervisor {
    #[handler(message = Terminated)]
    fn on_terminated(
        &mut self,
        #[message] terminated: Terminated,
        #[context] cx: ActorContext<'_, Self>,
    ) {
        let (tag, kind) = (terminated.tag(), terminated.kind());
        println!("[supervisor] watched actor under tag {tag} left: {kind:?}");
        cx.extension().record(tag, kind);
    }
}
```

Three pieces are worth naming:

- **`#[handler(message = Terminated)]`** tells `#[messages]` that this method
  handles an existing message type rather than minting a new one from the
  method name. `Terminated` is defined by the framework (it's in the prelude),
  so we name it explicitly. (We met the `message =` form for handling
  externally-defined messages in [Messages and Handlers](03-messages-and-handlers.md).)
- **`#[message] terminated: Terminated`** binds the parameter to the incoming
  message itself. We take it whole because everything we want is behind methods:
  - `terminated.tag()` returns the `u64` correlation key you passed to `watch`.
  - `terminated.kind()` returns a `TerminationKind` describing *how* the actor
    left.
- **`#[context] cx: ActorContext<'_, Self>`** gives the handler access to the
  actor's own services. Here we use `cx.extension()` to reach the shared
  `DeathLog` - but the context is also how a handler watches other actors, which
  we'll get to below. (Contexts were introduced in
  [State, Answers, and Errors](04-state-answers-errors.md).)

### How an actor terminated: `TerminationKind`

`TerminationKind` is the error-free summary of an actor's outcome, with three
variants:

- `Finished` - the run loop drained and exited cleanly. This is what you get
  when the last handle to an actor drops and it simply runs out of work.
- `Failed` - a handler, the start hook, or initialization failed the actor
  (we'll cover failing an actor in [Lifecycle](06-lifecycle.md)).
- `Aborted` - the actor reached its dead state with no recorded outcome: a
  panic, or a task abort.

Notice that `Failed` carries no error value. A watcher learns *that* the actor
failed, not the specific error - the error is the watched actor's private
business, projected away when it crosses the watch boundary. If a watcher needs
the details, the watched actor should report them explicitly through a message
before it dies or the watcher needs to hold a handle to access the error.

## Where the watcher keeps its memory

A watcher is an actor, so it records what it learns into its own state - exactly
like any other handler mutating `&mut self`. In the example the supervisor's
state is a shared `DeathLog`, declared with `#[actor(shared = DeathLog)]` so that
the test harness can query it from outside after the death has been handled:

```rust
#[derive(Default, Clone)]
struct DeathLog(Arc<Mutex<Vec<(u64, TerminationKind)>>>);

impl DeathLog {
    fn record(&self, tag: u64, kind: TerminationKind) {
        self.0.lock().expect("death log").push((tag, kind));
    }

    fn snapshot(&self) -> Vec<(u64, TerminationKind)> {
        self.0.lock().expect("death log").clone()
    }
}
```

In a real supervisor you might not need a shared extension at all - you'd react
to the death directly in the handler (spawn a replacement, send a message,
update a plain field on `self`). The shared log here exists only so the example
can *observe* what the supervisor recorded; `(tag, kind)` is precisely what
`Terminated` hands you.

## Putting it together

Here is the shape of the example's `main`, with the supervision moments called
out:

```rust
let supervisor = ActorLauncher::default()
    .spawn_ready(&spawner, Supervisor)
    .await?;
let task = ActorLauncher::default()
    .spawn_ready(&spawner, Task { done: 0 })
    .await?;

// The supervisor watches the task under tag 42.
supervisor.watch(&task, 42);

task.work().await?;

// Drop the task's last handle. With nothing left to keep it alive, its
// mailbox closes, the loop drains, and a `Terminated { tag: 42, kind:
// Finished }` is pushed into the supervisor's mailbox.
let task_state = task.state().clone();
drop(task);
task_state.wait_for_terminal().await;

// The `Terminated` was enqueued before this ask (FIFO), so by the time
// the supervisor answers, it has already handled the death.
let deaths = supervisor.deaths().await?;
assert_eq!(deaths, vec![(42, TerminationKind::Finished)]);
```

The ordering here is worth dwelling on, because it's how you observe a death
deterministically without any extra synchronization. Dropping the task's last
handle starts its clean shutdown; `task.state().wait_for_terminal()` lets us
await the task actually reaching its terminal state. Because the mailbox is
FIFO, the `Terminated` signal was enqueued in the supervisor's mailbox *before*
the `deaths()` ask we send afterward - so when the supervisor answers `deaths()`,
it has necessarily already processed the death.

## Watching from inside a handler

You don't have to set up a watch from the outside. An actor can decide, while
handling a message, to start watching some actor it has just been handed - a new
worker, say, or a connection it just accepted. The `ActorContext` you already
take for other reasons gives you `watch` directly, with no handle to *self*
required:

```rust
#[handler]
fn adopt(
    &mut self,
    target: TypedActorHandle<Worker>,
    #[context] cx: ActorContext<'_, Self>,
) {
    cx.watch(&target, 99);
}
```

`cx.watch(&target, tag)` behaves identically to `handle.watch(&target, tag)` -
it uses the actor's own weak self-reference under the hood. As with the handle
form, the watch is weak: the `target` handle in this example drops at the end of
the block, and the watch does not keep it alive. The only requirement is the one
you'd expect: `Self` must handle `Terminated`, since that's where the signal
will land.

## Stopping a watch

To stop watching, call `unwatch`:

```rust
supervisor.unwatch(&task);
```

This removes every subscription the supervisor registered on that actor. It is
idempotent - calling it when you aren't watching, or after the watch has already
fired, is a harmless no-op. After `unwatch`, no `Terminated` for that actor will
be delivered. `unwatch` is also available on the context (`cx.unwatch(&target)`)
for symmetry with `cx.watch`.

## What supervision is, and isn't

It's worth being precise about what this primitive does and doesn't give you,
because other actor systems bundle more into the word "supervision."

`factories` gives you the *notification*: a reliable, ordered, weakly-held
signal that an actor terminated, tagged with a key you chose, telling you
whether it finished, failed, or aborted. What you *do* with that signal -
restart the actor, escalate to your own supervisor, log it, tear down siblings,
back off and retry - is ordinary actor code that you write in the handler. There
is no built-in restart strategy or supervision tree type, because every one of
those is a policy, and policies compose cleanly out of this single primitive
plus the spawning you already know. A supervision tree, in `factories`, is just
actors watching actors.

---

A watcher reacts to messages that arrive on their own schedule - a death can
land at any time. That's a hint at a more general capability: an actor whose
work is driven not (only) by callers sending it messages, but by an external
source of events it pulls from. That's the subject of the next chapter,
[Event Sources](08-event-sources.md).

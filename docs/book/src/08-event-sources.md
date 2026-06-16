# Event Sources

Every actor so far has been *reactive*: it sits quietly until a message lands in
its mailbox, handles it, and goes back to waiting. That's the right shape for
most actors - a connection, a session, a cache. But some actors need to act on
their own: a clock that ticks once a second, a poller that wakes to check a
socket, a worker that grinds through a backlog it gave itself. These actors
don't wait to be told; they *drive themselves*.

`factories` calls the thing that produces those self-driven turns an **event
source**. It's the answer to a simple question the run loop asks on every
iteration: *what feeds the actor next?* By default the answer is always "the next
piece of mail." An event source lets you give a different answer - sometimes a
timer firing, sometimes a self-message, sometimes the mailbox after all.

## The shape of an event source

You add an event source with one method on your `#[messages]` impl, marked
`#[event_source]`. It has a fixed signature:

```rust
use factories::actor::dispatch::DispatchedActorMessage;
use factories::actor::event::EventContext;
use factories::spawn::ActorMailbox;

#[event_source]
async fn drive(
    cx: EventContext<'_, Self>,
    mailbox: &mut (impl ActorMailbox + Send),
) -> Option<DispatchedActorMessage> {
    // decide what feeds the actor this turn
    mailbox.receive().await
}
```

Three of those types live outside the prelude, so you import them explicitly:
`DispatchedActorMessage` from `factories::actor::dispatch`, `EventContext` from
`factories::actor::event`, and `ActorMailbox` from `factories::spawn`.

Notice what's *missing*: there is no `self`. An event source is **not** a handler.
A handler is invoked *with the actor's state already locked for it* - the
framework acquires the lock, hands the handler its `&self` / `&mut self`, and
releases it afterward. An event source is handed no such guard: its job is to
decide what the *next* message will be, and the framework does no locking on its
behalf. The dispatch that *follows* - the message the source returns - then goes
through the normal locking machinery and runs a handler as usual.

That doesn't wall the source off from the actor's state, though. When a source
needs to look at `self` to make its decision, it takes the lock itself, through
`cx`: `cx.acquire_shared().await` hands back a read guard (`&Self`),
`cx.acquire_exclusive().await` a write guard (`&mut Self`) - the same access modes
handlers get. The only difference from a handler is *who* reaches for the lock: a
handler has it taken for it, a source takes it when, and only when, it wants it.
Both acquisitions are cancel-safe - if a source loses a race against the mailbox,
a pending acquire is dropped without ever taking the lock.

Each turn, the run loop hands the source two things: a borrowed
`EventContext` (your window onto the actor) and the actor's `mailbox`. The source
returns one of:

- `Some(message)` - dispatch this message, then come back next turn.
- `None` - stop the actor; the loop drains and shuts down.

## The three things a source can do

Within that one method you have three moves, and you'll mix them freely:

1. **Mint a self-message.** `cx.message(SomeMsg)` builds a fire-and-forget
   dispatch addressed to *this* actor - the same `SomeMsg` you'd send from
   outside, only it skips the channel. Return it and the matching `#[handler]`
   runs next.
2. **Defer to the mailbox.** `mailbox.receive().await` waits for real traffic,
   exactly like a plain reactive actor. Returning this is how an event source
   "stands down" and behaves normally.
3. **Race your own futures against the mailbox.** Run a timer (or a socket read,
   or any future) *and* `mailbox.receive()` together, and yield whichever wins.
   This is the interesting case, and it's where the heartbeat earns its name.

## The running example: a heartbeat

Let's build an actor that beats on a timer. You can run the finished program
with:

```sh
cargo run -p factories --example heartbeat
```

The actor keeps a tally of beats, fires a `Tick` to itself on a short interval,
and - crucially - stops self-ticking after a small budget so a runaway timer can
never starve real messages.

```rust
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use factories::actor::dispatch::DispatchedActorMessage;
use factories::actor::event::EventContext;
use factories::actor::ActorContext;
use factories::prelude::*;
use factories::spawn::ActorMailbox;

const TICK_BUDGET: u32 = 3;

#[derive(Actor)]
#[actor(shared = Pacer)]
struct Heartbeat {
    beats: u32,
}
```

Before the impl, look at the `#[actor(shared = Pacer)]` line and this little
struct:

```rust
#[derive(Default)]
pub struct Pacer {
    fired: AtomicU32,
}
```

Here's the wrinkle that drives the whole design. An event source is **stateless**
- it gets a fresh `cx` and `mailbox` each turn and keeps nothing of its own
between calls. So where does the budget counter live? The source *could* read it
out of `self` by taking the lock (`cx.acquire_shared().await`), but doing that
every single turn makes the source contend with handlers for the actor lock - a
lot of ceremony for one counter. The lighter answer is a *shared extension*:
lock-free state, declared with `#[actor(shared = ...)]`, that both halves touch
without locking anything. The source counts down through it; the handler bumps it.
The rule of thumb: take the lock (`cx.acquire_*`) when you need a consistent view
of the actor's *real* state to decide; use a shared extension for cheap, hot
coordination like this budget.

Now the source itself:

```rust
#[factories::messages]
impl Heartbeat {
    #[event_source]
    async fn drive(
        cx: EventContext<'_, Self>,
        mailbox: &mut (impl ActorMailbox + Send),
    ) -> Option<DispatchedActorMessage> {
        if cx.extension().fired.load(Ordering::Acquire) < TICK_BUDGET {
            // A heartbeat interval. Short so the example finishes in a blink.
            tokio::time::sleep(Duration::from_millis(20)).await;
            return Some(cx.message(Tick));
        }
        // Budget spent: behave like any ordinary actor and await real traffic.
        mailbox.receive().await
    }

    #[handler]
    async fn tick(&mut self, #[context] cx: ActorContext<'_, Self>) {
        self.beats += 1;
        cx.extension().fired.fetch_add(1, Ordering::AcqRel);
        println!("  tick! beat #{}", self.beats);
    }

    #[handler]
    async fn beats(&self) -> u32 {
        self.beats
    }
}
```

Walk through one turn. The source reads `fired` from the shared `Pacer`. While
it's below budget, it sleeps 20 ms, then returns `cx.message(Tick)` - a
self-addressed `Tick`. The run loop dispatches it; the `tick` handler runs with
the lock held, bumps `self.beats`, and bumps the shared `fired` so the *next*
turn of the source sees one more tick has landed. (Both halves reach the shared
extension the same way: the source through `cx.extension()`, the handler through
its `ActorContext`'s `extension()`.) Once `fired` reaches `TICK_BUDGET`, the
source falls through to `mailbox.receive().await` and the actor is reactive again
- answering `beats()` asks promptly, starving nothing.

That budget is the point worth internalizing: an event source that always returns
a self-message and never polls the mailbox is a tight loop the actor can never
escape. Giving the source a way to *stand down* - here, the budget check - keeps
real traffic flowing. Cancel-safety matters for the same reason: if you race the
timer against the mailbox and the mailbox wins, the dropped timer branch must
leave no half-finished state behind. (The heartbeat sidesteps this by not racing
- it `await`s the sleep, *then* checks the mailbox - but a true race needs care.)

## No `EventDriver` to write

You may have noticed you never named a driver type. The derive does that for you.
When `#[derive(Actor)]` sees an `#[event_source]` method on the impl, it
autoref-detects it and generates the run loop that calls it each turn, wiring it
in as the actor's `EventDriver`. There is an ordinary trait impl underneath -
this is `factories` keeping its one promise from [the introduction](01-introduction.md)
- but you don't write it. A plain actor with no `#[event_source]` simply gets the
default driver, which is exactly `mailbox.receive()` and nothing more, so a
reactive actor pays nothing for machinery it doesn't use.

## The escape hatch: a stateful driver

A stateless source covers most needs, but sometimes the *driver itself* must
remember something across turns - a countdown, a retry schedule, a buffered chunk
- that doesn't belong in the actor's state and isn't naturally lock-free. For
that, `#[event_source]` deliberately steps aside: you hand-write an `EventDriver`
and name it.

```rust
use factories::actor::event::{EventContext, EventDriver};

#[derive(Actor)]
#[actor(event_driver = CountdownDriver)]
struct Beacon {
    budget: u32,
    pings: u32,
}
```

`#[actor(event_driver = CountdownDriver)]` tells the derive to use your type
*verbatim* instead of generating one. The driver is a normal struct with whatever
private fields it needs, and it implements `EventDriver`:

```rust
struct CountdownDriver {
    remaining: u32,
}

// Seeded from the actor after init, so it can read the actor's starting state.
impl From<&Beacon> for CountdownDriver {
    fn from(actor: &Beacon) -> Self {
        Self { remaining: actor.budget }
    }
}

impl<M: ActorMailbox + Send> EventDriver<Beacon, M> for CountdownDriver {
    fn next<'a>(
        &'a mut self,
        cx: EventContext<'a, Beacon>,
        mailbox: &'a mut M,
    ) -> impl Future<Output = Option<DispatchedActorMessage>> + 'a {
        async move {
            if self.remaining > 0 {
                // Cancel-safe: no await between the mutation and the return.
                self.remaining -= 1;
                return Some(cx.message(Ping));
            }
            mailbox.receive().await
        }
    }
}
```

The shape mirrors the stateless source - same `cx`, same `mailbox`, same
`Option<DispatchedActorMessage>` return - but now `&mut self` is the driver's own
state, persisted by the run loop from one turn to the next. The `From<&Beacon>`
impl seeds it after the actor initializes, which is the one thing the stateless
source genuinely can't do: its only state, the shared extension, predates the
actor.

So the rule of thumb is short. Reach for `#[event_source]` first - it's stateless,
detected automatically, and coordinates through a shared extension when it needs
to. Drop down to a hand-written `EventDriver` only when the driver must carry
private mutable state across turns. Both produce the same `DispatchedActorMessage`
and dispatch through the same loop; the choice is only about where the state
lives.

With self-driving actors in hand, we've covered how an actor decides what to do.
Next, in [Protocols](09-protocols.md), we turn to how callers talk to actors
without naming their concrete type - abstracting over an actor by the set of
messages it accepts.

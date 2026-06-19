# Concurrency: Loops and Locks

So far every actor you've written has been *serial*. It pulls one message off its
mailbox, runs the handler to completion, and only then reaches for the next. That
isn't a performance compromise — it's the default, and it's the right default. An
actor that handles one message at a time behaves like ordinary single-threaded
code: no overlapping borrows of `self`, no data races, no lock to remember. The
introduction called this *"serial by default, concurrent on purpose"* — this is
the chapter where you opt in on purpose, and see exactly what opting in buys.

## The serial baseline, in named parts

When you write `#[derive(Actor)]` with no `#[actor(...)]` overrides, two of the
parts the derive fills in are the **run loop** and the **lock strategy**:

- The run loop is `SequentialRunLoop<Self>`. Each dispatch — acquiring access to
  the actor and running its handler — is driven to completion before the next
  message is pulled. Dispatches never overlap.
- The lock strategy is `UnguardedLock<Self>`. Because the loop guarantees
  dispatches never overlap, there is nothing to guard: the lock elides all
  synchronization down to a single atomic flag, not a real wait.

These two are a matched pair. `UnguardedLock` is sound *only* because
`SequentialRunLoop` promises serialized dispatch — and the type system enforces
that pairing (we'll see how below). Under this default both `&self` and `&mut
self` handlers run at zero lock overhead, because no handler can be running when
another starts.

## Where serial dispatch limits you

One message at a time is usually exactly right. But consider a *read-mostly*
actor — a cache, a directory, a config store — whose reads each do something
slow: a database round-trip, a disk seek, a call to another service. Those reads
don't conflict with each other. They only *look* at `self`; none of them mutates
it. Yet a serial loop still runs them strictly one after another, so eight
concurrent callers each wait behind the seven ahead of them. Nothing about the
data forces that ordering — it's purely the loop being conservative.

That is the case the concurrent loop exists for: letting handlers that *don't*
conflict — specifically, shared `&self` reads — run at the same time.

## Opting in: the concurrent loop and a real lock

Opting into concurrency means changing both parts together:

1. **Swap the run loop** to `ConcurrentRunLoop<Self>`. The loop now keeps pulling
   messages and admitting handlers while earlier handlers are still running, so
   several handler futures can be in flight at once.
2. **Swap the lock strategy** to a real lock. The instant handlers can overlap,
   the unguarded "lock" is no longer sound — you need one that actually arbitrates
   access. For overlapping reads, that lock is `TokioRwLock`.

Neither part lives in the prelude. That's deliberate: concurrency is something you
reach for on purpose, so its parts are named explicitly out of the `runtime`
module.

```rust
use factories::prelude::*;

use factories::runtime::concurrent_loop::ConcurrentRunLoop;
use factories::runtime::tokio::TokioRwLock;

#[derive(Actor)]
#[actor(run_loop = ConcurrentRunLoop<Self>, lock = TokioRwLock<Self>)]
struct Cache {
    entries: HashMap<u32, u64>,
}
```

The `run_loop` and `lock` keys are the same `#[actor(...)]` configuration keys you
use to override any associated type — concurrency isn't a special mode, just a
different choice of two parts.

### The compiler keeps the pair checked

What stops you from flipping only the loop? The type system. `UnguardedLock`'s
access modes require the loop to provide serialized dispatch — a marker trait,
`SerializedDispatch`, that `SequentialRunLoop` implements and `ConcurrentRunLoop`
deliberately does not. Set `run_loop = ConcurrentRunLoop<Self>` while leaving the
default `UnguardedLock`, and the code simply does not compile; the error points
you at the real fix, which is to pick a real lock. You cannot *accidentally* run
overlapping handlers over unguarded state.

## Exclusive and Shared — and what actually overlaps

The lock you choose, and the concurrency you get, both come down to how your
handlers borrow `self`. Recall from
[Messages and Handlers](03-messages-and-handlers.md) that the receiver picks each
handler's **access mode**:

- `&mut self` is **`Exclusive`** access — the handler mutates, so it needs the
  state to itself.
- `&self` is **`Shared`** access — the handler only reads, so it can run
  alongside other readers.

Here is the crucial part, and the thing the concurrent loop is really about:
**only `Shared` reads overlap.** Under a `TokioRwLock`, any number of `&self`
handlers hold the read lock together, so their awaits run in parallel. A `&mut
self` handler takes the write lock, which excludes everyone — readers and other
writers wait for it, and it waits for them. Two writes never overlap; a read and
a write never overlap. The concurrency you gain is *concurrent reads*, full stop.

That has a sharp consequence worth stating plainly:

> If every handler on your actor takes `&mut self`, the concurrent loop buys you
> nothing. `TokioMutexLock` grants only `Exclusive` access, so every handler takes
> it in turn — the loop admits their futures eagerly, but the lock funnels them
> back into single file. You've reproduced the serial default's behavior with
> extra lock overhead on top. For an all-writes actor, keep the serial default.

So the two real configurations are: the **serial default** (`SequentialRunLoop` +
`UnguardedLock`) for everything that doesn't need overlap, and **`ConcurrentRunLoop`
+ `TokioRwLock`** for a read-heavy actor whose `&self` reads you want to run
together. (`TokioMutexLock` exists for the rare actor that wants async-mutex
semantics without a read side, but as noted, on the concurrent loop it only
re-serializes — a `&self` handler on a mutex is in fact a compile error, steering
you to the read-write lock.)

## A read-mostly cache

The running example is exactly the `Cache` above: a key-value store whose `lookup`
is a slow `&self` read and whose `insert` is a rare `&mut self` write. Run it:

```sh
cargo run -p factories --example concurrency
```

The reads carry a small gauge — a lock-free [shared extension](06-lifecycle.md)
counting how many lookups are in flight at once — so the program can *prove* the
overlap rather than just assert it:

```rust
#[factories::messages]
impl Cache {
    /// Exclusive write: `&mut self` takes the write lock, so it runs alone.
    #[handler]
    fn insert(&mut self, key: u32, value: u64) {
        self.entries.insert(key, value);
    }

    /// Shared read: `&self` takes the read lock. Under the concurrent loop many
    /// of these hold it at once, so the slow reads happen in parallel.
    #[handler]
    async fn lookup(&self, key: u32, #[context] cx: ActorContext<'_, Self>) -> Option<u64> {
        let gauge = cx.shared_data();
        let now = gauge.in_flight.fetch_add(1, Ordering::AcqRel) + 1;
        gauge.peak.fetch_max(now, Ordering::AcqRel);

        tokio::time::sleep(Duration::from_millis(50)).await; // a slow read
        let value = self.entries.get(&key).copied();

        gauge.in_flight.fetch_sub(1, Ordering::AcqRel);
        value
    }
}
```

Fire eight lookups concurrently — build the ask futures, then await them together
— and the gauge tells the story:

```rust
let answers = futures::future::join_all((0..8).map(|key| cache.lookup(key).ask())).await;

let peak = cache.state().shared_data().peak.load(Ordering::Acquire);
println!("peak concurrent lookups: {peak}"); // 8
```

All eight reads hold the read lock simultaneously: the peak is `8`, and the batch
finishes in about one 50 ms read rather than eight stacked end to end. Swap the
loop back to the serial default and that peak drops to `1` — the reads queue up.
That gap is the whole value of the concurrent loop, and it lives entirely in the
read path.

## Two axes of concurrency

It's worth separating two things the word "concurrency" can mean here, because
conflating them is easy:

- **Between actors.** Separate actors are separate tasks, so they already run in
  parallel — you've had this since [Getting Started](02-getting-started.md),
  with no special loop. A *pool* of worker actors splitting a workload is just
  this axis: many actors, each serial, running at once.
- **Within one actor.** This chapter's subject: letting one actor's own `&self`
  handlers overlap. This is the axis the concurrent loop and `TokioRwLock` add.

If your instinct for "make this concurrent" is "spawn more actors," that's the
first axis and it needs nothing from this chapter. Reach for the concurrent loop
when the parallelism you want is *inside* a single actor's reads.

## When to opt in

Choose `ConcurrentRunLoop` + `TokioRwLock` when an actor is read-heavy and its
`&self` reads await slow work you'd like to overlap — the cache above is the
archetype. Keep the serial default for everything else: short CPU-bound handlers,
write-heavy actors (where a lock would only re-serialize), and any actor where
"one message fully handled before the next" is the behavior you actually want. The
default is not a limitation to grow out of; it's the right answer most of the time,
and the concurrent loop is a precise tool for the one shape that benefits.

Next we'll look at the edges of an actor's life — the [Lifecycle](06-lifecycle.md)
hooks that run when an actor starts and stops.

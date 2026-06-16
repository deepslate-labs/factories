# Introduction

`factories` is an actor framework for Rust, built on a single rule:

> **Everything is explicit, and every convenience decomposes into public primitives.**

There is no hidden runtime, no ambient global, and no magic. An actor is an
ordinary struct. Its message handlers are ordinary methods. The machinery that
carries a message from a sender to an actor - the channel, the mailbox, the run
loop, the lock that guards the actor's state - is assembled from parts you can
name, inspect, and replace. The derive macros you'll use throughout this book
are pure convenience: each one expands to code you could have written by hand,
against the same public API. Nothing is reserved for the framework's private use.

That rule is worth keeping in mind, because it explains the shape of everything
that follows. When a later chapter shows you a one-line attribute, you can trust
there's an ordinary trait impl underneath - and [Under the Hood](10-under-the-hood.md)
will show you exactly that impl.

## Why actors?

An actor is a piece of state with a mailbox. It owns its data outright, and the
only way to interact with it is to send it a message. The actor processes its
messages one at a time, so its handler code reads like ordinary single-threaded
code - no locks to remember, no data races to reason about - even though many
actors may be running at once on a thread pool.

This buys you a clean split: **concurrency happens *between* actors; each actor's
own state stays sequential.** It's a natural fit for the stateful pieces of a
system - a connection, a session, a coordinator, a cache, a device driver, a
worker in a pipeline - anywhere "an object you talk to by sending it things" is
the right mental model.

## What makes `factories` different

- **Explicit, not magic.** Every actor names its parts (or accepts well-marked
  defaults). There is no global executor you opt into by importing a prelude;
  you hand each actor the spawner it runs on.
- **Zero-overhead where it counts.** A typed send is devirtualized to a direct
  function-pointer call - no boxing, no dynamic dispatch on the hot path. You
  reach for type erasure only when you actually need it (see
  [Protocols](09-protocols.md)).
- **Serial by default, concurrent on purpose.** A freshly derived actor handles
  one message at a time and needs no lock. Concurrency is something you opt into
  deliberately ([Concurrency](05-concurrency.md)), not a tax you pay up front.
- **A `no_std` core.** The framework's heart runs without the standard library.
  The tokio-backed runtime you'll use in this book is one (default) *choice* of
  parts, not a requirement - [the last chapter](12-no-std-and-parts.md) shows how
  to pare it back or swap it out.

## A taste

Here is a complete actor - a counter you can increment and read:

```rust
use factories::prelude::*;

#[derive(Actor)]
struct Counter {
    value: u64,
}

#[factories::messages]
impl Counter {
    #[handler]
    fn inc(&mut self) {
        self.value += 1;
    }

    #[handler]
    async fn get(&self) -> u64 {
        self.value
    }
}
```

Spawn it onto a runtime, and from then on you only ever touch it through messages:

```rust
let counter = ActorLauncher::default()
    .spawn_ready(&TokioTaskSpawner::current(), Counter { value: 0 })
    .await?;

counter.inc().tell().await?;          // fire-and-forget: send and move on
assert_eq!(counter.get().await?, 1);  // ask: send, then await the answer
```

`#[derive(Actor)]` configured the actor with sensible defaults; `#[messages]`
turned each method into a message you can send, and generated the `counter.inc()`
/ `counter.get()` calling methods. We'll unpack every piece of this in the next
two chapters.

## How this book is organized

This book is a guided tour. Each chapter introduces one concept and builds on the
last:

- [Getting Started](02-getting-started.md) gets the crate into your project and
  spawns your first actor.
- [Messages and Handlers](03-messages-and-handlers.md) through
  [Protocols](09-protocols.md) walk the framework feature by feature - handlers,
  errors, concurrency, lifecycle, supervision, event sources, and protocols.
- [Under the Hood](10-under-the-hood.md) drops beneath the macros to the
  three-tier substrate and shows you how to build an actor entirely by hand.
- [`no_std` and Choosing Your Parts](12-no-std-and-parts.md) covers feature flags
  and running without tokio.

Every chapter's code is a real, runnable example in the crate's `examples/`
directory. Wherever you see a listing, you can run the program it comes from:

```sh
cargo run -p factories --example counter
```

## Who this book is for

You should be comfortable with Rust - ownership, traits, and `async`/`await` in
particular. You don't need prior experience with actor systems; we'll build the
ideas up from scratch. The examples run on the [tokio](https://tokio.rs) runtime,
which the default features pull in for you.

Let's get started.

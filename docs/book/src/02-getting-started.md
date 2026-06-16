# Getting Started

In the [introduction](01-introduction.md) we saw a counter actor in passing.
Now we'll build it for real - from an empty Cargo project to a running program
that talks to a live actor. We'll go a line at a time and explain each piece, so
by the end you'll know not just *what* to type but *why* each part is there.

## One dependency

`factories` is a single crate. Add it to your `Cargo.toml`:

```toml
[dependencies]
factories = "0.1"
```

That one line is enough to write *and run* an actor. The crate ships with
batteries included: its default features pull in the derive macros (the
`#[derive(Actor)]`, `#[factories::messages]`, and friends you'll use everywhere)
plus a complete tokio-backed runtime - the mailbox, task spawner, and answer
channel that carry messages around. So you'll also want tokio itself, to provide
the async runtime your `main` runs on:

```toml
[dependencies]
factories = "0.1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

The framework's core is actually `no_std` - the tokio runtime is just the
*default* choice of parts, not a hard requirement. You can pare the features back
or swap the runtime out entirely, but that's a topic for the
[last chapter](12-no-std-and-parts.md). For now, the defaults are exactly what
you want.

## The prelude

Every program in this book begins the same way:

```rust
use factories::prelude::*;
```

The prelude is the common surface for writing and spawning actors. It brings in
just what you reach for constantly:

- the **`Actor`** trait and its `#[derive(Actor)]`, and the **`Message`** trait;
- the **`#[messages]`** and **`#[protocol]`** attributes;
- the actor **handle** types you use to talk to a running actor;
- lifecycle and supervision types (we'll meet those in
  [Lifecycle](06-lifecycle.md) and [Supervision](07-supervision.md));
- and - because we're using the tokio runtime - **`ActorLauncher`** for
  spawning and **`TokioTaskSpawner`** for telling an actor *which* runtime to run
  on.

Anything outside the prelude lives in one of four modules - `actor`, `message`,
`spawn`, `runtime` - and you'll import it explicitly when you need it, e.g.
`use factories::runtime::...`. We won't need any of that today; the prelude
covers everything.

## Writing the actor

An actor in `factories` is an ordinary struct. Here's our counter - a single
number we'll be able to increment and read:

```rust
use factories::prelude::*;

#[derive(Actor)]
struct Counter {
    value: u64,
}
```

`#[derive(Actor)]` is what makes this struct an *actor* rather than a plain piece
of data. It configures the actor with sensible defaults - and the defaults are
deliberately unsurprising: a tokio mpsc mailbox, and **serial dispatch**. That
means a `SequentialRunLoop` handles one message at a time, so the state needs no
real lock (an `UnguardedLock`). One message in, one handler runs to completion,
the next message in. Concurrency is something you opt into on purpose, not a tax
you pay up front - we'll get to that in [Concurrency](05-concurrency.md). You
don't need to remember those type names yet; just know that "derived actor" means
"safe, sequential, no surprises."

### Giving it messages

A struct on its own can't receive anything. We make it talk by writing an
ordinary inherent `impl` block and marking it with `#[factories::messages]`:

```rust
#[factories::messages]
impl Counter {
    /// Fire-and-forget increment. Takes `&mut self` (exclusive state
    /// access) and answers `()`, so it reads naturally as a command.
    #[handler]
    fn inc(&mut self) {
        self.value += 1;
    }

    /// Request/response read. Takes `&self` (it only reads) and answers a `u64`.
    #[handler]
    async fn get(&self) -> u64 {
        self.value
    }
}
```

These are just methods - and they stay just methods; the macro is *additive*, so
you can still call `inc`/`get` directly on a `Counter` value. But
`#[factories::messages]` reads each method marked `#[handler]` and does two extra
things:

1. It turns each handler into a **message type** you can send to the actor.
2. It generates a **handle** type - here `CounterHandle` - with one calling
   method per handler, so you talk to the actor in the same vocabulary you wrote
   the methods in.

The receiver tells you the shape of each message. `inc` takes `&mut self`: it
mutates, so it's a command. `get` takes `&self`: it only reads, and it *returns*
a value, so it's a query. The return type is the message's *answer*: `inc`
answers `()` (nothing to report) and `get` answers `u64`. We'll dig into handler
shapes - arguments, answers, errors, `&self` vs `&mut self` - in
[Messages and Handlers](03-messages-and-handlers.md) and
[State, Answers, and Errors](04-state-answers-errors.md). For now, the two rules
above are all you need.

## Spawning it

A defined actor isn't a running one. To bring it to life we spawn it onto a
runtime and get back a handle:

```rust
let counter = ActorLauncher::default()
    .spawn_ready(&TokioTaskSpawner::current(), Counter { value: 0 })
    .await?;
```

There's a lot of intent packed into that one expression, so let's unpack it:

- **`ActorLauncher::default()`** is the builder that assembles a running actor
  from parts. `default()` takes the same unsurprising defaults the derive picked
  - the tokio channel, serial loop, and so on.
- **`spawn_ready(...)`** spawns an actor whose state is *ready* - already built.
  Our `Counter { value: 0 }` is infallible to construct, so we just hand it over.
  (Some actors need to do fallible async setup before they can run; there's a
  separate path for those, covered in [Lifecycle](06-lifecycle.md).)
- **`&TokioTaskSpawner::current()`** is how we say *where* the actor runs. There
  is no hidden global executor in `factories`; you hand each actor its spawner
  explicitly. `current()` grabs the tokio runtime your code is already running
  on. This is the "no magic global" rule from the introduction, made concrete.
- **`.await?`** - spawning is async, and it can fail, so we await and propagate.

What comes back, `counter`, is a `CounterHandle`. From this point on the
`Counter` struct is sealed away inside its own task; the *only* way to interact
with it is through this handle, by sending messages. That's the whole actor
discipline: state plus a mailbox, reachable only by message.

## Telling vs. asking

The handle gives you two ways to send a message, and the difference is worth
internalizing early because you'll choose between them constantly.

**Tell** is fire-and-forget. You call the handle method and then `.tell()`:

```rust
counter.inc().tell().await?;
```

Reading that right to left: `counter.inc()` builds the `Inc` message, `.tell()`
enqueues it, and the `.await?` waits only for *delivery* - that the message made
it into the mailbox - not for the handler to run. There's nothing to wait for
here anyway: `inc` answers `()`. Tell is the right tool for commands.

**Ask** sends a message and waits for the answer. You get it by `.await`-ing the
handle method directly, with no `.tell()`:

```rust
let value = counter.get().await?;
```

That bare `.await` sends `Get`, then suspends until the actor has actually run
the `get` handler and sent the `u64` back. Because asking waits for a real
answer, you use it for queries - anything where you need the result.

The `?` in both cases handles the one thing that can go wrong with *sending*: the
actor might no longer be alive to receive the message. That's distinct from
errors a handler itself might return, which we'll cover in
[State, Answers, and Errors](04-state-answers-errors.md).

## Running it

Putting it together - and wrapping it in a tokio `main` so we have a runtime to
spawn onto - here is the complete program:

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let counter = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Counter { value: 0 })
        .await?;

    // Tell it to increment three times - fire-and-forget commands.
    for _ in 0..3 {
        counter.inc().tell().await?;
    }

    // Ask for the value - wait for the real answer.
    let value = counter.get().await?;
    assert_eq!(value, 3);
    println!("the Counter saw all three increments: {value}");

    Ok(())
}
```

Each `inc` is its own message, and the actor processes its mailbox in order, so
all three increments land before the `get` is handled - which is why we observe
`3`. When `main` ends, `counter` (the last handle) is dropped, the actor's task
winds down, and the program exits cleanly.

This is a real, runnable example. You can run it straight from the crate:

```sh
cargo run -p factories --example counter
```

That's a complete actor: defined, spawned, told, and asked. You've already seen
the whole loop the rest of the book builds on. Next, in
[Messages and Handlers](03-messages-and-handlers.md), we'll slow down on the
handlers themselves - how arguments and answers flow, and what the `#[messages]`
macro really generates.

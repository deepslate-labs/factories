# `no_std` and Choosing Your Parts

Everywhere in this book you've added `factories` to your `Cargo.toml` with its
default features on, and that's the right call for most projects: you get the
derive macros and a complete tokio-backed runtime, batteries included. But the
introduction made a promise - the tokio runtime is *one choice of parts, not a
requirement* - and [Under the Hood](10-under-the-hood.md) showed you the
three-tier substrate that makes good on it. This chapter cashes that promise in.
We'll walk the feature flags one at a time, see what survives when you strip
everything back to a `no_std` core, and then assemble an actor on parts you
supply yourself.

## The feature flags

The crate's defaults are two flags, one of which is an umbrella:

```toml
[dependencies]
factories = "0.1"          # default = ["derive", "full-runtime"]
```

Here is what each flag unlocks. Every one of these turns on, in turn, the
matching feature of the internal `factories_actor` crate - but you only ever name
them on `factories` itself.

- **`derive`** - the macros: `#[derive(Actor)]`, `#[derive(Message)]`,
  `#[messages]`, and `#[protocol]`. Without it, the `messages` and `protocol`
  re-exports vanish from the prelude and you write the trait impls by hand, the
  way [Under the Hood](10-under-the-hood.md) does. Everything still *works*
  without `derive`; you just lose the shorthand.

- **`tokio-answer`** - request/response. This is what makes `.ask()` and `.call()`
  available, backed by `tokio::sync::oneshot`. It's lightweight: it pulls in
  `tokio/sync` only, never the runtime. The generated calling methods that return
  a `MessageCall` live behind this flag, so with it off you lose `.ask()`, the
  answer channel, and `AnswerSender` / `AnswerReceiver` - and the generated
  `counter.inc().tell()` calling methods go with them. Fire-and-forget still works
  through the lower-level `handle.tell(Inc)` on the typed handle.

- **`tokio-lock`** - the tokio lock strategies, `TokioMutexLock` and
  `TokioRwLock` (the ones [Concurrency](05-concurrency.md) reached for). Like
  `tokio-answer`, this only needs `tokio/sync`, not the runtime - the locks are
  async primitives, not executor machinery.

- **`tokio-runtime`** - the default *parts you spawn on*: the
  `TokioMpscActorChannel` mailbox and the `TokioTaskSpawner`. This is the flag
  that introduces the `tokio/rt` dependency. With it on, `DefaultChannel` resolves
  to the cancellation-safe tokio mpsc channel and `TokioTaskSpawner` appears in
  the prelude. Turn it off and a derived actor no longer has a default channel -
  you must supply one (we'll do exactly that below).

- **`dynamic-dispatch`** - the global handler registry. Handlers register at
  binary load time, and the registry builds per-actor dispatch tables so that a
  message arriving through a *type-erased* path (an `AnyActorHandle`, the dynamic
  sends from [Protocols](09-protocols.md)) can find its handler at runtime. With
  this flag off, only static dispatch is available: typed sends still devirtualize
  to a direct call as always, but dynamic sends never bind.

- **`tracing`** - diagnostic spans. The send site captures the current span and
  the run loop re-enters it before invoking your handler, so handler events nest
  under the call site. Pure observability; off by default in the sense that it
  costs nothing when you don't enable it.

- **`full-runtime`** - not a feature of its own so much as a bundle. It is exactly
  `["tokio-answer", "tokio-runtime", "tokio-lock", "dynamic-dispatch"]`. When you
  see "the full tokio-backed runtime," this is the set it means.

So `default = ["derive", "full-runtime"]` is the everyday experience you've had
all book: macros, ask/answers, the tokio channel and spawner, the tokio locks,
and the dynamic registry.

## Paring it back to the `no_std` core

The framework's heart is `#![no_std]`. To get there, switch off the defaults and
add back only what you need:

```toml
[dependencies]
factories = { version = "0.1", default-features = false, features = ["derive"] }
```

What remains is the always-on core - the contract for *what it means to be an
actor*, independent of any runtime:

- **`actor`** - the [`Actor`](actor::Actor) trait, message handlers, handles,
  lifecycle, and supervision.
- **`message`** - message types, envelopes, and (with `tokio-answer`) the answer
  channel.
- **`spawn`** - the *assembly contracts*: the `ActorLauncher` builder and the
  traits a channel, mailbox, and spawner must satisfy to be assembled.

These three modules are `no_std` + `alloc`; they're there whatever your feature
set. The fourth module, **`runtime`**, is the feature-gated part: it holds the
*concrete* parts - the tokio channel and spawner under `tokio-runtime`, the tokio
locks under `tokio-lock`, the registry under `dynamic-dispatch`. The default run
loops (`SequentialRunLoop`, `ConcurrentRunLoop`) and the `UnguardedLock` strategy
live in `runtime` too, but they're dependency-free `core`-only code and are always
available - only the *tokio* pieces are gated.

This is the seam the whole design turns on: `actor` / `message` / `spawn` is the
contract; `runtime` is one implementation of it. Strip `tokio-runtime` and a
derived actor compiles fine - right up until it asks for its `DefaultChannel` and
finds the alias doesn't exist. The error points straight at the
`defaults` module and tells you to either enable the feature or configure the
component yourself. That second option is the rest of this chapter.

### A note on `critical-section`

There is one practical wrinkle for genuine `no_std` targets. The core uses
`once_cell` in its `critical-section` configuration for the bits of global state
it needs (the dynamic-dispatch registry among them). `once_cell` then expects the
*final binary* to provide a `critical-section` implementation - that's how a
no-OS target says "here is how I disable interrupts to make this access atomic."

For ordinary `std` builds you supply the off-the-shelf one. That's why the crate's
own tests, doctests, and examples carry this dev-dependency:

```toml
[dev-dependencies]
critical-section = { version = "1", features = ["std"] }
```

On a real embedded target you'd swap `features = ["std"]` for the implementation
your HAL or chip-support crate provides. The point is only that the obligation
lands on the binary, not on `factories`.

## Choosing - and providing - your parts

Because the substrate is split into a contract and an implementation
([Under the Hood](10-under-the-hood.md), tier 2), you are never stuck with the
parts the crate ships. The launcher is generic over the contracts, so anything
that satisfies a contract drops in - with no changes to the framework. There are
three seams worth knowing.

**A channel.** A mailbox is just a queue. To plug your own in, implement
`CreatableChannel` (so the launcher can construct it) and pair it with an
`ActorMailbox` (so a standard run loop can receive from it). Both live in
`factories::spawn`:

```rust
use factories::spawn::{ActorMailbox, CreatableChannel};
```

`CreatableChannel::create(options)` hands back the sender half (the channel) and
its `Mailbox`; `ActorMailbox::receive` is the async `next` the loop pulls from,
resolving to `None` once the channel is closed and drained. Implement those two
and your actor can be spawned on your queue exactly as it would on the tokio one.
(If your channel doesn't want to participate in *generic* assembly at all, you can
implement only the lower-level `ActorChannel` from `factories::actor::channel` and
wire it up by hand.)

**A task spawner.** The single point where the framework touches an executor is
`ActorTaskSpawner`, also in `factories::spawn`:

```rust
use factories::spawn::ActorTaskSpawner;
```

Its whole surface is one method:

```rust
fn spawn<F>(&self, fut: F) -> ActorTaskHandle
where
    F: Future<Output = ()> + Send + 'static;
```

`TokioTaskSpawner` is one implementation of this and nothing more. Want
`async-std`, a thread-per-actor pool, or a single-threaded embedded executor?
Implement `spawn` against it, return an `ActorTaskHandle`, and hand your spawner
to `ActorLauncher::spawn_ready` in place of `&TokioTaskSpawner::current()`. The
actor doesn't know or care.

**A lock strategy.** The state-guarding scheme is equally swappable. The default
`UnguardedLock` is the zero-overhead no-op that pairs with the sequential loop;
`TokioMutexLock` and `TokioRwLock` are the tokio-backed real locks. To define your
own vocabulary, implement `LockStrategy` (from `factories::actor`) plus the
capability traits `ExclusiveLockStrategy` / `SharedLockStrategy` (from
`factories::runtime::lock`):

```rust
use factories::runtime::lock::{ExclusiveLockStrategy, SharedLockStrategy};
```

A handler asking for `&mut self` (the `Exclusive` access mode) works on any
strategy that implements `ExclusiveLockStrategy`; `&self` (`Shared`) works on any
that implements `SharedLockStrategy`. Your strategy opts into whichever modes it
can serve, and handlers stay portable across all of them.

In every case the move is the same: name the contract, implement it, and pass your
part where the default would have gone. You configure these per actor through the
same `#[actor(run_loop = …, lock = …)]` attribute from [Concurrency](05-concurrency.md),
or by setting the associated types by hand - the derive adds no capability you
couldn't write yourself. That's the rule from the very first page holding all the
way down: every convenience decomposes into public primitives, and here you're
simply choosing different ones.

---

That closes the tour. You've gone from a one-line `#[derive(Actor)]` to the parts
it stands on, and now to picking those parts yourself or running with none of
tokio's at all. From here the source is the next teacher: the `actor`, `message`,
`spawn`, and `runtime` modules are documented in the same spirit as this book, and
every example in the crate's `examples/` directory is a working program you can
read, run, and bend. Build something.

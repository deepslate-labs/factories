# Protocols

So far, every handle you've held has been *typed*. When you spawn a `Counter`,
you get back a `CounterHandle` (or, erased one notch, a `TypedActorHandle<Counter>`):
a handle that knows exactly which actor sits on the other end, and therefore
knows exactly which messages it accepts. Calling `counter.get()` is checked at
compile time and devirtualized to a direct function-pointer call - no boxing, no
dispatch overhead. That's the happy path the framework optimizes for.

But sometimes you don't care *which* actor you're talking to. You care only that
it understands a particular set of messages. A chat room wants to broadcast to
its subscribers; it has no business knowing whether a subscriber is a `Logger`,
a `Counter`, or some type written by a plugin it has never heard of. All it needs
is the promise: *whoever you are, you can receive a `ChatMessage`.*

That promise is a **protocol**.

> Protocols use the answer channel under the hood, so they need the
> `tokio-answer` feature. It's on by default; if you've pared your features back
> ([no_std and Choosing Your Parts](12-no-std-and-parts.md)), enable it.

## Declaring a protocol

A protocol is a trait whose methods *name messages*. You write it with the
`#[protocol]` attribute (it's in the prelude):

```rust
use factories::prelude::*;

#[derive(Debug, Clone, Message)]
struct ChatMessage {
    text: String,
}

#[protocol]
trait Subscriber {
    fn notify(&self, msg: ChatMessage);
}
```

Read that trait carefully, because it's doing less than it looks like. Each
method has exactly one real parameter: a message type. The method *name* -
`notify` - is just the calling surface you'll type at the call site; it carries
no meaning to the framework. What selects the handler is the parameter type,
`ChatMessage`. The return type is filled in for you: it's the message's own
`Message::Answer` (here `()`, since `ChatMessage` is fire-and-forget). A protocol
with several messages simply lists several methods:

```rust
#[protocol]
trait Counting {
    fn add(&self, msg: Add);
    fn total(&self, msg: Total);
}
```

From that one declaration, `#[protocol]` generates **two** things, and the rest
of this chapter is about when you reach for each.

## Thing one: the trait, a zero-cost generic bound

The first thing it generates is the trait itself, blanket-implemented over *any*
typed handle whose actor handles every message the protocol lists. So a
`CountingHandle`-style typed handle for any actor that has both an `Add` and a
`Total` handler satisfies `Counting` automatically - you write no `impl`.

This means you can take a protocol as a generic bound and pay nothing for it:

```rust
async fn drive(counter: impl Counting) -> u32 {
    counter.add(Add { amount: 5 }).tell().await.expect("tell");
    counter.total(Total).await.expect("ask")
}

let calc = /* a TypedActorHandle<Calc> */;
assert_eq!(drive(calc).await, 15);
```

`impl Counting` here is an ordinary monomorphized generic. The handle stays
fully typed inside `drive`, so each call devirtualizes exactly as it would if you
had named the concrete actor. The protocol bought you *abstraction over the actor
type* with no erasure cost at all.

Because these are just trait bounds, combining protocols is free too - you don't
need a third "combined" protocol. A typed handle whose actor handles both message
sets satisfies `impl Adder + Reader` directly:

```rust
async fn drive(handle: impl Adder + Reader) -> u32 {
    handle.add(Add { amount: 3 }).tell().await.expect("tell");
    handle.total(Total).ask().await.expect("ask")
}
```

## Thing two: the erased handle, for heterogeneous collections

The generic bound is perfect when each call site knows *one* type. But our chat
room wants a `Vec` holding subscribers of *different* actor types. A generic
parameter can't do that - `Vec<impl Subscriber>` is still one concrete type per
`Vec`. You need erasure.

That's the second thing `#[protocol]` generates: a concrete struct named after
the trait with `Handle` appended - `SubscriberHandle`, `CountingHandle`. It holds
an erased actor handle plus a small **cached dispatcher table**: one entry per
protocol message, each a function pointer that has already been verified to bind.
That table is the *proof* the messages resolve, carried alongside the erased
handle so that calling through it stays a direct dispatch - no per-call lookup,
no `dyn`.

Because the actor type is erased, all of these share one type:

```rust
#[derive(Default)]
struct ChatRoom {
    subscribers: Vec<SubscriberHandle>,
}
```

A `Logger` handle and a `Counter` handle, two structurally unrelated actors, can
live side by side in that `Vec`. The room calls the protocol method on each, and
the cached table routes it to the right handler:

```rust
impl ChatRoom {
    fn subscribe(&mut self, who: SubscriberHandle) {
        self.subscribers.push(who);
    }

    async fn broadcast(&self, text: &str) {
        for subscriber in &self.subscribers {
            subscriber
                .notify(ChatMessage { text: text.to_owned() })
                .tell()
                .await
                .expect("subscriber mailbox accepts the broadcast");
        }
    }
}
```

Calling `subscriber.notify(...)` returns a message call just like a typed handle
would; `.tell()` delivers it fire-and-forget (and `.ask()` would await an answer,
exactly as in earlier chapters).

## Building an erased handle

There are two ways to construct a `SubscriberHandle`, and they differ in whether
the compiler can already prove the messages bind.

**Infallibly, from a typed handle, with `From` / `.into()`.** When you start from
a typed handle, the compiler *already knows* the actor handles every protocol
message - that's a static fact. So the conversion can't fail, and it's a plain
`.into()`:

```rust
let mut room = ChatRoom::default();
room.subscribe(logger.clone().into());   // LoggerHandle -> SubscriberHandle
room.subscribe(counter.clone().into());  // CounterHandle -> SubscriberHandle
```

This `From` is deliberately generous about what it accepts. It works from a bare
`TypedActorHandle<A>` *and* from the derive's generated `…Handle` newtype (the
`LoggerHandle` / `CounterHandle` you get back from `spawn_ready`). That's why
`logger.clone().into()` works directly, without first unwrapping to a
`TypedActorHandle`.

**Fallibly, from an erased `AnyActorHandle`, with `try_bind`.** Sometimes you
*don't* have a typed handle - you have a fully type-erased `AnyActorHandle`
(perhaps received from a registry, or handed across a boundary that forgot the
actor type). The compiler can no longer guarantee anything, so binding has to be
checked at runtime. That's the inherent method `try_bind`, which consults the
actor's runtime type information to verify every protocol message resolves:

```rust
use factories::actor::handle::AnyActorHandle;

let any: AnyActorHandle = calc.erase_type();
let counter = CountingHandle::try_bind(any).expect("Calc speaks Counting");
```

`try_bind` returns a `Result`: `Ok(handle)` if the actor speaks the protocol, and
`Err(any)` handing the original erased handle back to you on failure, so nothing
is lost. The two paths interoperate freely - you can mix `.into()`-built and
`try_bind`-built handles in the same `Vec<CountingHandle>`. Exactly how `try_bind`
resolves each message at runtime - the *binder* underneath - is the subject of
[Dynamic Dispatch and Binders](11-dynamic-dispatch.md).

> A note on naming: the fallible path is `try_bind`, **not** `TryFrom`. It can't
> be `TryFrom`, because the standard library's blanket `From`-to-`TryFrom` bridge
> would collide with the infallible `From` above. Reach for the inherent
> `try_bind` method; there is no `try_from` for protocol handles.

A shared protocol handle is `Send + Sync` - it wraps an `AnyActorHandle` - so you
can move it across threads and share it freely.

## Thread-local protocols

If your actors run on a single thread and their messages or answers aren't
`Send`, write `#[protocol(local)]`. The trait and its generic-bound surface are
identical; two things differ. The erased handle wraps an `AnyLocalActorHandle`
and is therefore `!Send`. And the methods return a `LocalMessageCall` (the
`LocalCalling` surface) instead of a `MessageCall`: the same verbs, but the
futures carry no declared `Send` bound - a shared protocol promises `Send`
futures, which a `!Send` answer cannot honor (see chapter 3's note on the
`Send` guarantee).

```rust
#[protocol(local)]
trait LocalCounting {
    fn add(&self, msg: Add);
    fn total(&self, msg: Total);
}
```

The construction paths mirror the shared variant: `.into()` from a typed handle,
and `try_bind` from an erased *local* handle (an `AnyLocalActorHandle`, obtained
via `erase_type_local()`).

## Running it

The chat-room story above is a complete, runnable program - a `ChatRoom` holding
a `Logger` and a `Counter` behind one `Vec<SubscriberHandle>`, each interpreting
the same broadcast in its own way:

```sh
cargo run -p factories --example chat_room
```

## Where protocols sit

Protocols are the deliberate exception to the "stay typed" rule from the
[Introduction](01-introduction.md). The framework devirtualizes typed sends so
hard precisely so that erasure is something you opt into only when the shape of
the problem demands it - a heterogeneous collection, a plugin boundary, an actor
received without its type. When you reach for `#[protocol]`, you get the smallest
amount of erasure that solves the problem: an abstraction over actor *type* that
keeps the message *set* statically guaranteed and each call a direct dispatch.

You've now seen the whole macro-driven surface of the framework. The next chapter,
[Under the Hood](10-under-the-hood.md), keeps the promise made on page one: it
drops beneath every attribute you've used and shows you the ordinary trait impls
they expand to - including how a typed handle, an erased handle, and that cached
dispatcher table are actually built.

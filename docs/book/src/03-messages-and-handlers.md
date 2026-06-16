# Messages and Handlers

In the last chapter you spawned an actor and talked to it with `counter.inc()` and
`counter.get()`. Those calling methods didn't exist on the `Counter` struct - they
came from `#[factories::messages]`. This chapter is about that macro: it is the heart
of the derive surface, the piece you'll reach for in nearly every actor you write.

The promise is simple. You write an ordinary inherent `impl` block of ordinary
methods, and mark the ones you want reachable by message with `#[handler]`. The macro
reads each method's *signature* and, from it alone, generates everything needed to
send that method a message and get its answer back. Let's take it apart.

## A handler is a method, plus a message

Here is the counter again, with the two handlers we already met:

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

For each `#[handler]` method the macro derives a **message type** from the signature:

- The name is the method name in PascalCase: `inc` becomes `Inc`, `get` becomes
  `Get`. A method called `set_config` would produce `SetConfig`.
- Each non-`self` parameter becomes a **public field** of the message, with the same
  name and type. `inc` and `get` take no parameters, so `Inc` and `Get` are unit
  structs. A handler `fn add(&mut self, amount: u64)` produces a message
  `struct Add { pub amount: u64 }`.
- The message's `Answer` is the method's **return type** - `()` for `inc`, `u64`
  for `get`. (`Answer` is the associated type on the `Message` trait that says what
  comes back when you *ask*; we'll lean on it throughout.)
- The message **inherits the method's visibility**. A `pub fn` handler yields a `pub`
  message; a private handler yields a private one. The generated type lives in the
  same module as the `impl`, so you can name `Inc`, `Get`, `Add` directly.

The macro also generates a `MessageHandler` impl - the glue that calls your method
when one of these messages arrives. That's the second half of "a handler is a method
*plus* a message": the method is your code, the message and its handler impl are the
wiring that delivers a message to it.

Crucially, **the macro is additive.** It re-emits your `impl` block unchanged, just
with the markers stripped. So `inc` and `get` are still plain methods - you can call
`counter.inc()` directly on a `&mut Counter` you own, no actor machinery involved. The
message interface is *added*, never substituted.

```rust
let mut counter = Counter { value: 0 };
counter.inc();            // just a method call
assert_eq!(counter.value, 1);
```

## Shared vs. exclusive: the receiver picks

Look at the two receivers above: `inc` takes `&mut self`, `get` takes `&self`. That
choice is not cosmetic. The macro reads it to decide how the handler accesses the
actor's state:

- `&mut self` → **exclusive** access. The handler may mutate. While it runs, no other
  handler touches the state.
- `&self` → **shared** access. The handler only reads.

Under the serial default an actor handles one message at a time regardless, so a
`&self` handler "only reads" but doesn't run *concurrently* with anything. The
distinction becomes a real concurrency win once you opt into a concurrent run loop
with a shared lock - a `&self` handler can then run alongside other readers. That's
the subject of [Concurrency: Loops and Locks](05-concurrency.md). For now, just write
the receiver that's honest about what the method does: `&self` if it reads, `&mut self`
if it writes.

`async fn` handlers are simply awaited before the answer is produced. `get` above is
`async` even though it does no awaiting - that's fine; you write `async` when a
handler needs to `.await` something inside it (call another actor, do I/O), and a
plain `fn` otherwise.

## Tell and ask ride on the generated method

For each handler, the macro also generates a **calling method on the handle** with the
same name and the field parameters as arguments. `counter.inc()` and `counter.get()`
are those. Each returns a `MessageCall` - a small, `#[must_use]` value that has done
nothing yet. *How* you finish it decides the kind of send:

```rust
counter.inc().tell().await?;          // fire-and-forget
let value = counter.get().await?;     // ask: await the answer
```

- **`.tell().await`** enqueues the message and waits only for *delivery*, not for the
  handler to run. The result is whether the send succeeded. This is the right form
  for commands whose answer is `()` - there's nothing to wait for.
- **A bare `.await`** on the call is the **ask**: it sends the message, then waits for
  the actor to run the handler and send the answer back. It yields
  `Result<Answer, _>` - so `counter.get().await?` gives you the `u64`.

(There's also an explicit `.ask()` that's identical to the bare `.await`, and
`.blocking_tell()` / `.blocking_ask()` for use off an async runtime. You'll mostly
write `.tell().await` and the bare `.await`.)

For a handler with fields, the field parameters become the method's arguments and the
message is built for you:

```rust
#[handler]
fn add(&mut self, amount: u64) { self.value += amount; }

// ...
counter.add(5).tell().await?;   // builds `Add { amount: 5 }` internally
```

You can run a complete program demonstrating this with
`cargo run -p factories --example counter`.

## Reusing an existing message

Sometimes the message already exists - maybe several actors all answer the *same*
message, or you derived a `Message` by hand and want a handler for it. Point the
handler at it with `#[handler(message = ...)]`:

```rust
#[derive(Debug, Message)]
#[message(answer = u32)]
pub struct AddBoth {
    pub left: u32,
    pub right: u32,
}

#[factories::messages]
impl Customized {
    #[handler(message = AddBoth)]
    fn add_both(&mut self, left: u32, right: u32) -> u32 {
        self.hits += left + right;
        self.hits
    }
}
```

Now **no new message is generated.** Instead the parameter *names* select the fields
of `AddBoth` to decompose into your handler: `left` and `right` are pulled from the
incoming `AddBoth` by name. The type system checks this - the names must match real
fields of the message, with matching types. Extra fields the handler doesn't name are
simply ignored.

The calling method changes shape to match: since the message is the existing
`AddBoth`, the generated `customized.add_both(...)` takes the whole `AddBoth` value:

```rust
let total = customized.add_both(AddBoth { left: 2, right: 3 }).await?;
```

This is exactly how the [chat room example](09-protocols.md) lets two unrelated
actors both handle one shared `ChatMessage`:

```rust
#[derive(Debug, Clone, Message)]
struct ChatMessage { text: String }

#[factories::messages]
impl Logger {
    #[handler(message = ChatMessage)]
    fn on_chat(&mut self, text: String) {
        self.transcript.push(text);
    }
}
```

The handler is named `on_chat` for readability, but the *message* it handles is
`ChatMessage` - and that binding is what later lets `Logger` speak a shared protocol.
Run it with `cargo run -p factories --example chat_room`.

## Parameter markers: re-routing a parameter

By default, every non-`self` parameter is a message field. But some things a handler
wants aren't message data - they're machinery: the answer channel, the whole message,
the sealed envelope, the actor's own context. You ask for those with a **parameter
marker**, an attribute on the parameter that tells the macro "this isn't a field -
route something else into it." The macro never looks at the parameter's type; the
generated call supplies the value and the compiler checks the type for you.

### `#[answer]` - answer later

Normally the answer is whatever the handler returns. But sometimes you can't answer
*now* - you need to stash the request and reply after some later event. Mark a
parameter `#[answer]` and it receives `Option<AnswerSender<M>>`, the sender half of
the asker's reply channel:

```rust
#[derive(Actor)]
struct Deferring {
    pending: Option<AnswerSender<Defer>>,
}

#[factories::messages]
impl Deferring {
    #[handler(answer = u32)]
    fn defer(&mut self, #[answer] reply: Option<AnswerSender<Defer>>) {
        self.pending = reply;        // stash it; don't answer yet
    }

    #[handler]
    fn release(&mut self) {
        if let Some(pending) = self.pending.take() {
            let _ = pending.send(42); // answer the earlier asker now
        }
    }
}
```

Two details. First, taking the answer sender **disables the automatic answer**, so the
handler must *not* also return a value - and because there's no return type to read
the answer type from, you declare it with `#[handler(answer = u32)]`. Second,
`AnswerSender` lives in `factories::message::channel` and is re-exported in the
prelude (with the tokio runtime), so `use factories::prelude::*;` brings it in.

### `#[message]` - the whole message by value

When you've reused an existing message but want it whole rather than decomposed into
fields, mark a single parameter `#[message]`:

```rust
#[handler(message = AddBoth)]
fn sum(&mut self, #[message] whole: AddBoth) -> u32 {
    whole.left + whole.right
}
```

This composes with the automatic answer (here the `u32` return is still sent back). It
cannot be combined with field parameters - it's *either* the whole message *or*
decomposed fields.

### `#[envelope]` - the sealed envelope, for forwarding

A `#[envelope]` parameter receives the message sealed in its `SendableEnvelope`, with
the answer sender still riding inside. The classic use is forwarding: hand the sealed
envelope to another actor and *it* answers the original asker directly.

```rust
#[handler(message = Probe)]
async fn relay(&mut self, #[envelope] envelope: SendableEnvelope) {
    self.target
        .prepare_send_dynamic(envelope.into_inner())
        .expect("probe must bind on the target")
        .send()
        .await
        .expect("forward must succeed");
}
```

It's the *sendable* envelope rather than the raw one because an `async fn`'s arguments
are captured into the future's state, where a `!Send` envelope would break thread-safe
dispatch. `SendableEnvelope` lives in `factories::message::envelope`. Like `#[message]`,
it can't be combined with message-derived field parameters.

### `#[context]` - the actor's own services

A `#[context]` parameter receives the [`ActorContext`](06-lifecycle.md) - a reduced
handle to the actor's own runtime services from inside a handler. With it a handler can
fail the actor (`cx.fail(error)`), get a weak self-reference, or set up a watch on
another actor. `ActorContext` is in the prelude (it's `ActorContext<'_, Self>` - the
lifetime is the borrow of the actor's shared state, `Self` the actor type):

```rust
#[handler]
fn poison(&mut self, #[context] cx: ActorContext<'_, Self>) {
    cx.fail(Boom(0));   // record a failure and stop the run loop
}
```

You can mix markers with ordinary field parameters freely (except where noted:
`#[message]` and `#[envelope]` own the whole message). A handler
`fn check(&mut self, value: u32, #[context] cx: ActorContext<'_, Self>)` still has
`value` as a field of the generated `Check` message; `cx` is routed in by the macro.
We'll meet `ActorContext` again, and the `fail` it offers, in
[State, Answers, and Errors](04-state-answers-errors.md) and [Lifecycle](06-lifecycle.md).

The markers, decomposition, and additive nature are exercised directly in the crate's
derive tests - `tests/derive/handlers.rs` and `tests/derive/markers.rs` are worth a
read if you want to see every combination spelled out.

## What's next

You now know how to define handlers, send them messages, and ask for answers. We've
been quietly returning `Result<_, _>` from our calls and writing `?` - but we haven't
said what those errors *are*, what happens when a handler itself returns an error, or
how `#[handler(die_on_err)]` can make an error fatal to the actor. That's the subject
of the next chapter, [State, Answers, and Errors](04-state-answers-errors.md).

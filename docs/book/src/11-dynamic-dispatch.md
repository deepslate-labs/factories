# Dynamic Dispatch and Binders

Almost every send in this book has been *static*. You hold a `CounterHandle`, you
call `counter.inc()`, and the compiler — knowing both the actor type and the
message type — wires the call straight to `Counter`'s handler for `Inc`. No
lookup, no indirection: a typed send is devirtualized to a direct
function-pointer call. That is what
"[zero-overhead where it counts](01-introduction.md)" meant.

But [Protocols](09-protocols.md) showed a different situation. A
`Vec<SubscriberHandle>` holds actors whose concrete types you've *erased*, and
when you build one with `try_bind` from an `AnyActorHandle`, whether a given
message even resolves is a runtime question, not a compile-time one. Something has
to take a message whose type is only known at runtime and find the handler for
it on a given actor. That something is the **binder** — and this chapter is the
whole story of it, the registry behind it, and how the protocols you already met
ride on top.

## The static path needs no binder

Start with what you already have, so the dynamic path stands in contrast. When
you send through a `TypedActorHandle<A>` (or the generated `…Handle` that wraps
one), the type of the message `M` is known, and the compiler has proven `A:
MessageHandler<M>` — that's what makes the call type-check at all. The handler to
run is `A`'s `MessageHandler<M>::DISPATCHER`, a `const` dispatcher the send reads
directly. There is nothing to look up: the dispatcher is chosen at
compile time and the call is direct. This is the path you want for everything you
*can* name statically, which is most things.

Dynamic dispatch is for the rest: the cases where you hold a handle but not a
type.

## The binder: the dynamic seam

Back in [Under the Hood](10-under-the-hood.md), the `Actor` trait named an
associated type we glossed over: `RuntimeBinder`. This is it. A binder answers
exactly one question:

> Given a message's *runtime* type — its RTTI — produce the dispatcher that runs
> it on this actor, or `None` if this actor doesn't handle it.

```rust
pub unsafe trait ActorRuntimeBinder: Send + Sync {
    fn bind(&self, message: &MessageRtti) -> Option<ActorMessageDispatcher>;
}
```

Two things to note. First, binding happens **caller-side** — `bind` never touches
the actor's state; it only maps a message type to a function pointer, which is why
it can run on whatever thread is doing the sending. Second, the trait is `unsafe`:
an implementor promises that a dispatcher it hands back genuinely handles that
message type on this actor and satisfies the actor's run-loop demand. Get that
wrong and you'd dispatch a message to the wrong handler — hence the obligation.

`factories` ships two binders.

## `StaticOnlyBinder` — the zero-cost default

The default binder, `StaticOnlyBinder`, never binds: its `bind` always returns
`None`. An actor configured with it supports the static path and nothing else —
an erased, runtime-typed send simply doesn't resolve. That's not a limitation so
much as a floor: if you never reach for dynamic dispatch, you pay nothing for it,
and there's no global registry in your binary at all. This is the binder a derived
actor gets when the `dynamic-dispatch` feature is off.

## `RegistryBinder` — registry-backed

Turn the `dynamic-dispatch` feature on and the default flips to
`RegistryBinder<Self>`, which resolves messages through a global registry of
handlers. The important property for the hot path: a `RegistryBinder` looks up its
actor's dispatch table **once, at construction** (that is, at spawn time). After
that, every `bind` is a lookup into that already-resolved table and never touches
global state again. (Its `PhantomData<fn(&A)>` typing is load-bearing, not
decoration: it makes mixing up one actor's binder for another's a compile error.)

So where does the table come from?

## The registry

You opt a handler into dynamic dispatch by registering it, right next to its
`MessageHandler` impl:

```rust
// on the actor:    type RuntimeBinder = RegistryBinder<Self>;

impl MessageHandler<AddValue> for Calc { /* ... DISPATCHER ... */ }
register_dynamic_handler!(Calc, AddValue);
```

`register_dynamic_handler!` records the triple `(Calc::RTTI, AddValue::RTTI,
DISPATCHER)` into a global collection **at binary load**, through a link-section
constructor. The dispatcher it registers is the *same* `MessageHandler::DISPATCHER`
constant the static path uses — so everything checked at that dispatcher's
declaration site (its thread-safety demand, its type coherence) carries over by
construction. There is no second, weaker dynamic dispatch path to get wrong.

The first time the registry is consulted it **freezes** the collected set and
assigns each registered message a dispatch ID:

- A message handled by exactly **one** actor type gets one of a block of
  consecutive IDs for that actor — so binding it is a subtraction and an index
  into a dense array.
- A message handled by **several** actor types lands in a small per-actor table
  sorted by ID, found by binary search.

The ID is stored on the message's own RTTI, write-once. Two consequences worth
internalizing: these IDs are **process-local and ASLR-shuffled** — they are an
in-process index, never wire-stable, so never serialize one. And **registrations
that arrive after the freeze never bind**: a handler registered by a library you
`dlopen` past startup gets no ID and silently fails to resolve. To make that loud,
`RegistryBinder` construction panics in debug builds naming the late
registrations, and `DispatchRegistry::is_stale()` exposes the check for release
builds. If startup timing matters, call `dispatch_registry()` yourself once,
early, to freeze deterministically.

## Sending dynamically

With a registry-backed actor, an erased handle can send a message whose type is
chosen at runtime. You build an envelope and ask the handle to resolve it:

```rust
use factories::message::envelope::MessageEnvelope;
use factories::actor::handle::ActorHandle;

let erased = calc.erase_type(); // AnyActorHandle — the concrete type is gone

let envelope = MessageEnvelope::new(AddValue(5), None); // None = tell, no answer
erased
    .prepare_send_dynamic(envelope)
    .expect("AddValue is registered for this actor")
    .send()
    .await?;
```

`prepare_send_dynamic` consults the binder and returns `None` when the message
doesn't bind for *this* actor — either it was never registered, or it was
registered only for a different actor type. That `None` is the runtime check that
stands in for the compile-time guarantee the typed path gave you for free: with
the type erased, "does this actor handle this message?" can only be answered by
looking.

## How protocols ride on this

You have, in fact, already used this machinery — [Protocols](09-protocols.md) is
its ergonomic front. A protocol's erased `…Handle` carries a *cached dispatcher
table*, one entry per protocol message, and it fills that table in one of two ways:

- **From a typed handle** (`From` / `.into()`): each dispatcher comes straight
  from the actor's `MessageHandler::DISPATCHER` — the compiler already proved the
  actor handles every protocol message, so no binder and no registry are involved.
  This path works even with `dynamic-dispatch` *off*.
- **From an `AnyActorHandle`** (`try_bind`): the concrete type is gone, so each
  dispatcher is resolved through the binder, exactly like `prepare_send_dynamic`.
  This is the path that *needs* a `RegistryBinder` — `try_bind` against a truly
  erased handle only succeeds when the actor was built with dynamic dispatch and
  registered the protocol's messages.

Because the table is cached at construction, per-send cost on a protocol handle is
the same whichever way it was built: the binder lookup, if any, happens once. You
rarely call `prepare_send_dynamic` by hand — you reach for a protocol, and the
binder works underneath.

## The derive default, and what it costs

Put together, the `derive` feature makes this seamless. With `dynamic-dispatch`
on, `#[derive(Actor)]` defaults an actor's `RuntimeBinder` to `RegistryBinder`,
and `#[messages]` auto-emits a `register_dynamic_handler!` for every handler (it
expands to nothing when the feature is off, mirroring the binder default). So a
derived actor in a `full-runtime` build supports erased sends and `try_bind` with
no extra code; the same actor in a build without `dynamic-dispatch` gets
`StaticOnlyBinder` and the zero-cost static-only world, paying nothing for a
registry it doesn't use.

The cost model, then, is clean: the static path is a direct function-pointer call
and always free. The dynamic path adds one table lookup at *bind* time — resolved
once when a `RegistryBinder` is built, or once per protocol handle — after which a
send is an ordinary send. You only enter that path by erasing a type on purpose,
and you only carry the registry by enabling the feature.

That is the last seam. With dynamic dispatch in hand you've seen the whole span —
from a one-line `#[derive(Actor)]` down to the binder that resolves a message with
its type erased. The final chapter,
[`no_std` and Choosing Your Parts](12-no-std-and-parts.md), steps back out to the
feature flags and shows how to run with none of tokio's parts at all.

# State, Answers, and Errors

In [Messages and Handlers](03-messages-and-handlers.md) every handler succeeded.
A deposit always went through; a read always produced a number. Real services
aren't so lucky: a withdrawal can overdraw, a lookup can miss, a request can
arrive in the wrong state. This chapter is about what happens when a handler
*fails* - and the first thing to understand is that "fails" splits into two very
different stories.

Sometimes a failure is just an answer the caller didn't want to hear: "no, you
can't withdraw that much." The operation is over, the actor is fine, and life
goes on. Other times a failure means the actor's world is broken and it has no
business continuing. `factories` keeps these two cases firmly apart, and it lets
*you* decide which one a given error is.

We'll keep one running example throughout: a bank account. It owns a balance,
takes deposits, and serves withdrawals that can be refused. You can run the full
program at any point:

```sh
cargo run -p factories --example bank_account
```

## A failure that is just an answer

Here is the account. Notice the error type is an ordinary value - there is
nothing actor-specific about it:

```rust
use factories::prelude::*;

/// A recoverable, domain-level error. It is an ordinary value carried back as
/// (part of) the answer - nothing about returning it touches the actor's life.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Overdraft {
    requested: u64,
    available: u64,
}

#[derive(Actor)]
struct BankAccount {
    balance: u64,
}
```

The interesting handler returns a `Result`:

```rust
#[factories::messages]
impl BankAccount {
    #[handler]
    fn deposit(&mut self, amount: u64) -> u64 {
        self.balance += amount;
        self.balance
    }

    /// A fallible operation. The handler's return type is `Result<u64, Overdraft>`,
    /// and *that whole `Result` is the message answer*.
    #[handler]
    fn withdraw(&mut self, amount: u64) -> Result<u64, Overdraft> {
        if amount > self.balance {
            return Err(Overdraft {
                requested: amount,
                available: self.balance,
            });
        }
        self.balance -= amount;
        Ok(self.balance)
    }

    #[handler]
    async fn balance(&self) -> u64 {
        self.balance
    }
}
```

This is the key idea, and it's deliberately boring: **a handler that returns
`Result` is not special.** The return value is the answer, full stop. If
`withdraw` returns `Ok(60)`, the asker gets `Ok(60)`; if it returns
`Err(Overdraft { .. })`, the asker gets exactly that `Err`. Returning `Err` is a
normal completion - the handler ran, produced a value, and control returned to
the run loop. The actor is none the wiser and processes the next message just as
it would after any other handler.

That has a consequence worth pausing on. When you `ask` a fallible handler, you
end up with two layers of result:

```rust
match account.withdraw(40).await? {
    Ok(remaining) => println!("withdrew 40 -> balance {remaining}"),
    Err(over)     => println!("withdraw 40 refused: {over:?}"),
}
```

The outer `?` belongs to the *send*: it unwraps the transport - did the message
reach a live actor and come back? The inner `Result` is the *handler's own
answer*: the domain question of whether the withdrawal was allowed. They are
genuinely different failures. A closed mailbox is a transport error; an overdraft
is a perfectly successful round-trip that happens to carry an `Err`.

So an overdrawing withdrawal succeeds at the transport level and reports its
domain failure:

```rust
match account.withdraw(1_000).await? {
    Ok(remaining) => println!("withdrew 1000 -> balance {remaining}"),
    Err(over) => println!(
        "refused: wanted {} but only {} available (actor still alive)",
        over.requested, over.available
    ),
}

// Proof the actor survived the `Err`: it still answers, and the balance is
// exactly what it was before the failed withdrawal.
let still_there = account.balance().await?;
assert_eq!(still_there, 60);
```

This is the path you'll want most of the time. Domain errors - insufficient
funds, not found, already exists, bad input - are answers, not catastrophes. Let
them flow back to the caller and keep the actor running.

## When an error *should* be fatal

Some errors are different. If an actor discovers its invariant is broken, or a
resource it depends on has vanished, continuing to serve messages is worse than
stopping. For those, you want the error to *kill the actor* - but you still want
the asker to learn what went wrong.

That's what `die_on_err` is for. Before we can use it, the actor needs a real
error type. By default `Actor::Error` is `Infallible` (a freshly derived actor
can't fail of its own accord), so a fatal error has to convert into the actor's
declared error. You declare that type with `#[actor(error = ...)]`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct Overdrawn(u64);

#[derive(Actor)]
#[actor(error = Overdrawn)]
struct StrictAccount {
    balance: u64,
}
```

Now a handler can opt its errors into being fatal. There are two flavors, and the
difference is entirely about what the asker sees.

### `die_on_err`: forward the error, then die

The bare form forwards the error untouched and *additionally* records it as the
actor's death:

```rust
#[factories::messages]
impl StrictAccount {
    /// The asker receives the full `Result`; the actor dying is a side effect.
    #[handler(die_on_err)]
    fn withdraw(&mut self, amount: u64) -> Result<u64, Overdrawn> {
        self.balance = self
            .balance
            .checked_sub(amount)
            .ok_or(Overdrawn(amount))?;
        Ok(self.balance)
    }
}
```

The answer is the full `Result`, *exactly as if the key weren't there* - an
asker still gets `Ok(remaining)` on success and `Err(Overdrawn(..))` on failure.
The only added behavior is that, on `Err`, the error is also fed into the actor's
death. Concretely, an overdrawing `withdraw` here:

1. returns `Err(Overdrawn(amount))` to the asker, just like the recoverable
   version; then
2. records that error as the actor's failure and stops the run loop.

The actor transitions to `Dead`, and the failure is observable - every later
message hits a dead mailbox. (We'll cover lifecycle and how observers learn of a
death in [Lifecycle](06-lifecycle.md) and [Supervision](07-supervision.md).)

Because the error is sent to the asker *and* moved into the death, the bare form
needs to clone it: the error type must be `E: Clone + Into<Actor::Error>`. Here
`Overdrawn` is both the handler error and the actor error, and it derives
`Clone`, so both requirements are met.

### `die_on_err = consume`: the error only feeds the death

Sometimes the asker has nothing useful to do with the error - the death *is* the
report. The `consume` form unwraps the answer to its `Ok` part and routes the
error solely into the death:

```rust
#[handler(die_on_err = consume)]
fn withdraw_or_close(&mut self, amount: u64) -> Result<u64, Overdrawn> {
    self.balance = self
        .balance
        .checked_sub(amount)
        .ok_or(Overdrawn(amount))?;
    Ok(self.balance)
}
```

On success the asker receives the *inner* value directly - `u64`, not
`Result<u64, _>`. On failure there's nothing to send back: the answer channel
simply closes, which the asker sees as a (transport-level) send error, and the
error feeds the death. Because the error is never duplicated to the asker,
`consume` needs only `E: Into<Actor::Error>` - **no `Clone` required.**

So the two forms trade off cleanly:

| Form                   | Answer on `Ok` | Answer on `Err`            | Bound on `E`                 |
|------------------------|----------------|----------------------------|------------------------------|
| (no key)               | `Ok(v)`        | `Err(e)`, actor lives      | -                            |
| `die_on_err`           | `Ok(v)`        | `Err(e)`, actor dies       | `Clone + Into<Actor::Error>` |
| `die_on_err = consume` | `v`            | channel closes, actor dies | `Into<Actor::Error>`         |

Reach for the recoverable form by default. Use `die_on_err` when an error is
genuinely fatal but the caller still benefits from seeing it; use `consume` when
the death already tells the whole story and you'd rather not pay for a `Clone`.

## Failing by hand from a handler

`die_on_err` is sugar over a primitive you can call directly. A handler can ask
for the actor's runtime context with a `#[context]` parameter and fail the actor
explicitly with `cx.fail(error)`:

```rust
#[handler]
fn poison(&mut self, #[context] cx: ActorContext<'_, Self>) {
    cx.fail(Overdrawn(0));
}
```

`ActorContext` (in the prelude) is the actor's own handle on its runtime
services. The `#[context]` marker tells `#[messages]` that this parameter isn't a
message field - it's the context, injected by the dispatcher. Calling
`cx.fail(error)` records the failure immediately; the run loop notices at its
next turn (typically when this handler returns), drops any in-flight work, and
exits. The first error wins - later `fail` calls are dropped.

This is the escape hatch for failures that don't come from a `Result` return:
a handler that detects a broken invariant partway through, a `tell` (which has no
answer to carry an error) that nonetheless needs to bring the actor down, or any
condition where "stop now" is the right move regardless of return type.

A note on `&mut self` and reclaiming state: `cx.fail` only *records* the error
and signals the loop. The handler still finishes normally and returns to the run
loop, which then performs the shutdown. You don't have to unwind by hand.

## Choosing the path

Three tools, one decision - *should this failure end the actor?*

- **No** → return `Result<T, E>` from a plain `#[handler]`. The `Err` is just an
  answer; the actor keeps serving.
- **Yes, and the caller should still see why** → `#[handler(die_on_err)]`.
- **Yes, and the death is the whole report** → `#[handler(die_on_err = consume)]`.
- **Yes, but the trigger isn't a `Result`** → take `#[context]` and call
  `cx.fail(error)`.

Notice none of these required a special run loop or lock. A serial, default-derived
actor fails exactly the same way a concurrent one does - which is the subject of
the next chapter, [Concurrency: Loops and Locks](05-concurrency.md), where we
finally let an actor handle more than one message at a time.

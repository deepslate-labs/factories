//! The "hello world" of `factories`: defining an actor, spawning it, and the
//! tell-vs-ask distinction on the generated handle methods.
//!
//! Run with: `cargo run -p factories --example counter`

use factories::prelude::*;

// An actor is just an ordinary struct. `#[derive(Actor)]` gives it the default
// configuration: a tokio mpsc mailbox and *serial* dispatch - a
// `SequentialRunLoop` handles one message at a time, so the state needs no real
// lock (an `UnguardedLock`). Nothing here is hidden; these are just the safe,
// surprise-free defaults. Concurrency is something you opt into explicitly
// (see the concurrency example).
#[derive(Actor)]
struct Counter {
    value: u64,
}

// `#[factories::messages]` turns an ordinary inherent impl into a message
// interface: every `#[handler]` method also becomes a message type, and a
// `CounterHandle` is generated with one calling method per handler. The methods
// stay plain methods too - the macro is additive.
#[factories::messages]
impl Counter {
    /// Fire-and-forget increment. Takes `&mut self` (exclusive state access)
    /// and answers `()`, so it reads naturally as a command, not a query.
    #[handler]
    fn inc(&mut self) {
        self.value += 1;
    }

    /// Request/response read. Takes `&self` (shared access: it only reads).
    /// Under the serial default this still runs one message at a time; `&self`
    /// becomes a real concurrency win once you opt into a concurrent loop with a
    /// shared lock.
    #[handler]
    async fn get(&self) -> u64 {
        self.value
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Spawn the actor onto the current tokio runtime. `spawn_ready` constructs
    // the state up front (our `Counter` is infallible to build) and hands back
    // the typed handle through which we talk to it. The actor now runs as its
    // own task; we only ever touch it through messages.
    let counter = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Counter { value: 0 })
        .await?;
    println!("spawned a Counter starting at 0");

    // `.tell()` is fire-and-forget: we enqueue the `Inc` message and await only
    // the *delivery*, not the handler's result. Perfect for commands whose
    // answer is `()` - there's nothing to wait for.
    for n in 1..=3 {
        counter.inc().tell().await?;
        println!("told the Counter to inc ({n} of 3)");
    }

    // A bare `.await` on a handle method is the *ask*: it sends `Get`, then
    // waits for the actor to run the handler and send the answer back. Because
    // every `inc` was a separate message, all three are processed before this
    // query - so we observe the fully-incremented value.
    let value = counter.get().await?;
    println!("asked the Counter for its value: {value}");

    assert_eq!(value, 3);
    println!("done - the Counter saw all three increments");

    // Dropping `counter` is the last handle; the actor's task winds down and
    // the program exits cleanly.
    Ok(())
}

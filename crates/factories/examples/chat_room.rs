//! Protocols and broadcast: one `#[protocol] trait Subscriber` lets a `ChatRoom`
//! hold a heterogeneous list of *different* actor types behind a single erased
//! `SubscriberHandle`, then fan one `ChatMessage` out to all of them.
//!
//! Run with: `cargo run -p factories --example chat_room`

use factories::prelude::*;

// The message every subscriber must understand. Note it is *not* named
// `Message` - that name belongs to the protocol trait below. Its `Answer`
// defaults to `()`, so delivering one is fire-and-forget.
#[derive(Debug, Clone, Message)]
struct ChatMessage {
    text: String,
}

// The protocol. `#[protocol]` reads a trait whose method *names a message* and
// emits two things: the trait itself (a zero-cost generic bound, blanket-impl'd
// over any typed handle whose actor handles `ChatMessage`) and a concrete erased
// `SubscriberHandle` that carries a cached dispatcher - the proof the message
// binds. The method name (`notify`) is only the calling surface; the parameter
// type (`ChatMessage`) is what selects the handler.
#[protocol]
trait Subscriber {
    fn notify(&self, msg: ChatMessage);
}

/// A subscriber that remembers everything it heard.
#[derive(Actor)]
struct Logger {
    transcript: Vec<String>,
}

#[factories::messages]
impl Logger {
    // `message = ChatMessage` decomposes the *existing* shared message into this
    // handler's parameters by field name, rather than generating a new message.
    // That binding is exactly what makes `LoggerHandle` satisfy `Subscriber`.
    #[handler(message = ChatMessage)]
    fn on_chat(&mut self, text: String) {
        self.transcript.push(text);
    }

    // Not part of the protocol - a private query we use to inspect the actor.
    #[handler]
    async fn transcript(&self) -> Vec<String> {
        self.transcript.clone()
    }
}

/// A second, structurally unrelated subscriber that only counts traffic.
#[derive(Actor)]
struct Counter {
    seen: u64,
}

#[factories::messages]
impl Counter {
    #[handler(message = ChatMessage)]
    fn on_chat(&mut self, text: String) {
        let _ = text;
        self.seen += 1;
    }

    #[handler]
    async fn seen(&self) -> u64 {
        self.seen
    }
}


/// A plain struct, not an actor: it just owns erased subscriber handles and fans
/// messages out. The room neither knows nor cares whether an entry is a `Logger`,
/// a `Counter`, or some type it has never heard of - only that it speaks the
/// `Subscriber` protocol.
#[derive(Default)]
struct ChatRoom {
    subscribers: Vec<SubscriberHandle>,
}

impl ChatRoom {
    fn subscribe(&mut self, who: SubscriberHandle) {
        self.subscribers.push(who);
    }

    // Broadcast: drive every subscriber through the protocol's `notify`, which
    // returns a `MessageCall`; `.tell()` delivers it fire-and-forget.
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spawner = TokioTaskSpawner::current();

    // Spawn the two distinct subscriber actors. `spawn_ready` hands back each
    // actor's generated typed handle (`LoggerHandle`, `CounterHandle`).
    let logger = ActorLauncher::default()
        .spawn_ready(&spawner, Logger { transcript: Vec::new() })
        .await?;
    let counter = ActorLauncher::default()
        .spawn_ready(&spawner, Counter { seen: 0 })
        .await?;
    println!("spawned two subscribers: a Logger and a Counter");

    // Erase each typed handle into the shared `SubscriberHandle`. The conversion
    // is infallible (`.into()`): the compiler already proved each actor handles
    // `ChatMessage`, so the generated `LoggerHandle` / `CounterHandle` convert
    // straight into the protocol handle.
    let mut room = ChatRoom::default();
    room.subscribe(logger.clone().into());
    room.subscribe(counter.clone().into());
    println!("registered both with the room as erased Subscribers (one Vec, two actor types)");

    // Post a couple of messages; the room fans each out to every subscriber.
    room.broadcast("hello, room!").await;
    room.broadcast("anyone here?").await;
    println!("broadcast 2 messages to all subscribers");

    // Inspect each subscriber through its *typed* handle to show delivery.
    let transcript = logger.transcript().await?;
    let seen = counter.seen().await?;
    println!("Logger heard: {transcript:?}");
    println!("Counter tallied: {seen} message(s)");

    assert_eq!(transcript, vec!["hello, room!", "anyone here?"]);
    assert_eq!(seen, 2);
    println!("both subscribers received the same broadcast, each in its own way");

    Ok(())
}

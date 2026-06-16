#![cfg(all(feature = "tracing", feature = "tokio-runtime", feature = "tokio-answer"))]

//! Behavioural tests for the framework's `tracing` instrumentation.
//!
//! A process-global capturing subscriber records every span and event; each test
//! drives its *own* actor type and filters the capture by `actor.name`, so the
//! tests stay isolated without serialising or clearing the shared buffer.

use std::collections::BTreeMap;
use std::sync::Mutex;

use factories_actor::actor::channel::ActorChannelSendable;
use factories_actor::actor::dispatch::StaticDispatcher;
use factories_actor::actor::event::DefaultMailboxDriver;
use factories_actor::actor::handle::TypedActorHandle;
use factories_actor::actor::lifecycle::StopReason;
use factories_actor::actor::rtti::ActorRtti;
use factories_actor::actor::supervision::Terminated;
use factories_actor::actor::work::IntoRunLoopWork;
use factories_actor::actor::{
    Actor, ActorContext, ActorRunLoop, MessageHandler, StaticOnlyBinder,
};
use factories_actor::runtime::lock::{self, UnguardedLock};
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::runtime::tokio::{TokioMpscActorChannel, TokioTaskSpawner};
use factories_actor::spawn::ActorLauncher;
use factories_actor::{declare_actor_rtti, declare_message, declare_static_async_dispatcher};

use tracing::field::{Field, Visit};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

// ---------------------------------------------------------------------------
// Capture harness
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Record {
    /// Span name, or the event's (callsite) name for events.
    name: String,
    level: Level,
    is_span: bool,
    /// The span's id (0 for events) - used to merge later `record` calls.
    id: u64,
    /// The span's explicitly-set parent id, if any (`None` for events).
    parent: Option<u64>,
    fields: BTreeMap<String, String>,
}

impl Record {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

static CAPTURED: Mutex<Vec<Record>> = Mutex::new(Vec::new());

struct MapVisitor(BTreeMap<String, String>);

impl Visit for MapVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().to_string(), value.to_string());
    }
    fn record_debug(&mut self, field: &Field, value: &dyn core::fmt::Debug) {
        self.0.insert(field.name().to_string(), format!("{value:?}"));
    }
}

struct CaptureLayer;

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        _ctx: Context<'_, S>,
    ) {
        let mut visitor = MapVisitor(BTreeMap::new());
        attrs.record(&mut visitor);
        CAPTURED.lock().unwrap().push(Record {
            name: attrs.metadata().name().to_string(),
            level: *attrs.metadata().level(),
            is_span: true,
            id: id.into_u64(),
            parent: attrs.parent().map(tracing::span::Id::into_u64),
            fields: visitor.0,
        });
    }

    // Fields recorded after span creation (e.g. otel.name, error.type, which
    // start `Empty`) arrive here - merge them into the span's captured record.
    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _ctx: Context<'_, S>,
    ) {
        let mut visitor = MapVisitor(BTreeMap::new());
        values.record(&mut visitor);
        let mut captured = CAPTURED.lock().unwrap();
        if let Some(record) = captured
            .iter_mut()
            .rev()
            .find(|r| r.is_span && r.id == id.into_u64())
        {
            record.fields.extend(visitor.0);
        }
    }

    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MapVisitor(BTreeMap::new());
        event.record(&mut visitor);
        CAPTURED.lock().unwrap().push(Record {
            name: event.metadata().name().to_string(),
            level: *event.metadata().level(),
            is_span: false,
            id: 0,
            parent: None,
            fields: visitor.0,
        });
    }
}

fn install_subscriber() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        tracing_subscriber::registry().with(CaptureLayer).init();
    });
}

/// Every captured record whose `actor.name` field matches `actor`.
fn records_for(actor: &str) -> Vec<Record> {
    CAPTURED
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.field("actor.name") == Some(actor))
        .cloned()
        .collect()
}

/// A snapshot of every captured record.
fn all_records() -> Vec<Record> {
    CAPTURED.lock().unwrap().clone()
}

// ---------------------------------------------------------------------------
// Test actor: a hand-written `Pinged` actor with lifecycle hooks.
// ---------------------------------------------------------------------------

struct Pinged;

declare_actor_rtti!(PINGED_RTTI, Pinged);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Pinged {
    const RTTI: &'static ActorRtti = PINGED_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Pinged>;
    type RunLoop = SequentialRunLoop<Pinged>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;

    fn on_start<'a>(
        &'a mut self,
        _cx: ActorContext<'a, Self>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        async {}
    }

    fn on_stop<'a>(
        self,
        _reason: StopReason<'a, Self>,
        _cx: ActorContext<'a, Self>,
    ) -> impl IntoRunLoopWork<<Self::RunLoop as ActorRunLoop<Self>>::WorkConverter> + 'a {
        async {}
    }
}

#[derive(Debug)]
struct Ping;
declare_message!(Ping, ());

impl MessageHandler<Ping> for Pinged {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Pinged, Ping> =
        declare_static_async_dispatcher!(Pinged, Ping, |ctx| async move {
            let (_guard, _message, answer) = ctx.into_parts();
            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

// A bare actor with no message handlers, used to observe the full lifecycle
// (start hook, running, stop hook, termination) in isolation.
struct Cycle;

declare_actor_rtti!(CYCLE_RTTI, Cycle);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Cycle {
    const RTTI: &'static ActorRtti = CYCLE_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Cycle>;
    type RunLoop = SequentialRunLoop<Cycle>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;
}

#[tokio::test]
async fn lifecycle_emits_hook_spans_and_events() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Cycle)
        .await
        .expect("init");

    // Drive the actor to a clean termination so the stop hook + stopped event fire.
    let state = handle.state().clone();
    drop(handle);
    state.wait_for_terminal().await;

    let records = records_for("Cycle");
    let id = state.id().as_usize().to_string();

    let on_start = records
        .iter()
        .find(|r| r.is_span && r.name == "factories.on_start")
        .expect("on_start span");
    assert_eq!(on_start.level, Level::DEBUG, "lifecycle spans are DEBUG");
    assert_eq!(on_start.field("actor.id"), Some(id.as_str()));

    let started = records
        .iter()
        .find(|r| !r.is_span && r.field("message") == Some("actor started"))
        .expect("actor started event");
    assert_eq!(started.level, Level::DEBUG);
    assert_eq!(started.field("actor.id"), Some(id.as_str()));

    let on_stop = records
        .iter()
        .find(|r| r.is_span && r.name == "factories.on_stop")
        .expect("on_stop span");
    assert_eq!(on_stop.level, Level::DEBUG);

    let stopped = records
        .iter()
        .find(|r| !r.is_span && r.field("message") == Some("actor stopped"))
        .expect("actor stopped event");
    assert_eq!(stopped.level, Level::DEBUG, "a clean stop is DEBUG");
    assert_eq!(stopped.field("outcome"), Some("finished"));
}

// A watcher actor: handles `Terminated` (a no-op log), so `watch` compiles and
// a watched actor's termination delivers a signal into its mailbox.
struct Watcher;

declare_actor_rtti!(WATCHER_RTTI, Watcher);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Watcher {
    const RTTI: &'static ActorRtti = WATCHER_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Watcher>;
    type RunLoop = SequentialRunLoop<Watcher>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;
}

impl MessageHandler<Terminated> for Watcher {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Watcher, Terminated> =
        declare_static_async_dispatcher!(Watcher, Terminated, |ctx| async move {
            drop(ctx);
        });
}

#[tokio::test]
async fn terminated_delivery_emits_event() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let watcher = ActorLauncher::default()
        .spawn_ready(&spawner, Watcher)
        .await
        .expect("watcher init");
    let watched = ActorLauncher::default()
        .spawn_ready(&spawner, Cycle)
        .await
        .expect("watched init");

    watcher.watch(&watched, 1);

    let watched_state = watched.state().clone();
    drop(watched);
    watched_state.wait_for_terminal().await;

    let delivered = records_for("Cycle")
        .into_iter()
        .find(|r| !r.is_span && r.field("message") == Some("delivering terminated signal"))
        .expect("terminated delivery event");
    assert_eq!(delivered.level, Level::DEBUG);
    assert_eq!(delivered.field("kind"), Some("Finished"));
}

// An actor whose handler fails the actor (via ctx.fail), to exercise span Status.
#[derive(Debug, Clone)]
struct Kaboom;

struct Fragile;

declare_actor_rtti!(FRAGILE_RTTI, Fragile);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Fragile {
    const RTTI: &'static ActorRtti = FRAGILE_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = Kaboom;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Fragile>;
    type RunLoop = SequentialRunLoop<Fragile>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;
}

#[derive(Debug)]
struct Detonate;
declare_message!(Detonate, ());

impl MessageHandler<Detonate> for Fragile {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Fragile, Detonate> =
        declare_static_async_dispatcher!(Fragile, Detonate, |ctx| async move {
            let actor_cx = ctx.actor_context();
            let (_guard, _message, answer) = ctx.into_parts();
            actor_cx.fail(Kaboom);
            if let Some(answer) = answer {
                let _ = answer.send(());
            }
        });
}

#[tokio::test]
async fn failing_handler_span_records_error_type() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Fragile)
        .await
        .expect("init");
    let id = handle.state().id().as_usize().to_string();

    let state = handle.state().clone();
    handle.ask(Detonate).exchange().await.expect("ask");
    state.wait_for_terminal().await;

    let span = records_for("Fragile")
        .into_iter()
        .find(|r| {
            r.is_span
                && r.name == "factories.handle_message"
                && r.field("actor.id") == Some(id.as_str())
        })
        .expect("the failing handler's span");

    // error.type is a standard semantic field, emitted under plain `tracing`.
    assert!(
        span.field("error.type").is_some_and(|t| t.contains("Kaboom")),
        "the span records the error type, got {:?}",
        span.field("error.type")
    );
    // The OTel status mapping is only stamped under the opentelemetry feature.
    #[cfg(feature = "opentelemetry")]
    assert_eq!(span.field("otel.status_code"), Some("ERROR"));
    #[cfg(not(feature = "opentelemetry"))]
    assert_eq!(span.field("otel.status_code"), None);
}

#[tokio::test]
async fn successful_handler_span_has_no_error() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Pinged)
        .await
        .expect("init");
    let id = handle.state().id().as_usize().to_string();

    handle.ask(Ping).exchange().await.expect("ask");

    let span = records_for("Pinged")
        .into_iter()
        .find(|r| {
            r.is_span
                && r.name == "factories.handle_message"
                && r.field("actor.id") == Some(id.as_str())
        })
        .expect("the handler span");

    // A successful handler leaves error.type / otel.status_code unset (the spec
    // says instrumentation should leave status Unset on success).
    assert_eq!(span.field("error.type"), None);
    assert_eq!(span.field("otel.status_code"), None);
}

// An actor whose handler panics, to exercise the terminal abort path.
struct Doomed;

declare_actor_rtti!(DOOMED_RTTI, Doomed);

// SAFETY: The RTTI is declared for exactly this type.
unsafe impl Actor for Doomed {
    const RTTI: &'static ActorRtti = DOOMED_RTTI;

    type Channel = TokioMpscActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder = StaticOnlyBinder;
    type LockStrategy = UnguardedLock<Doomed>;
    type RunLoop = SequentialRunLoop<Doomed>;
    type TypedHandle = TypedActorHandle<Self>;
    type SharedStateExtension = ();
    type EventDriver = DefaultMailboxDriver;
}

#[derive(Debug)]
struct Boom;
declare_message!(Boom, ());

impl MessageHandler<Boom> for Doomed {
    type AccessMode = lock::Exclusive;

    const DISPATCHER: StaticDispatcher<Doomed, Boom> =
        declare_static_async_dispatcher!(Doomed, Boom, |_ctx| async move {
            panic!("boom");
        });
}

#[tokio::test]
async fn panicking_handler_emits_aborted_event() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Doomed)
        .await
        .expect("init");

    let state = handle.state().clone();
    handle.tell(Boom).send().await.expect("tell");

    // The handler panic unwinds the actor task; the dead-on-drop guard records
    // the abort and emits the event before the terminal transition.
    state.wait_for_terminal().await;

    let aborted = records_for("Doomed")
        .into_iter()
        .find(|r| !r.is_span && r.field("message") == Some("actor aborted"))
        .expect("actor aborted event");
    assert_eq!(aborted.level, Level::WARN, "an abort is WARN");
}

#[tokio::test]
async fn message_handler_span_carries_actor_and_message_fields() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Pinged)
        .await
        .expect("init");
    let id = handle.state().id().as_usize().to_string();

    handle.ask(Ping).exchange().await.expect("ask");

    // Find the span by *this* actor's id - other tests spawn `Pinged` too, so
    // matching on the instance id both isolates the lookup and is itself the
    // correlation assertion (no span without the right `actor.id` would match).
    let span = records_for("Pinged")
        .into_iter()
        .find(|r| {
            r.is_span
                && r.name == "factories.handle_message"
                && r.field("actor.id") == Some(id.as_str())
        })
        .expect("a message-handler span correlated to this actor instance");

    assert_eq!(span.level, Level::TRACE, "message spans are TRACE");
    assert_eq!(span.field("actor.name"), Some("Pinged"));
    assert_eq!(span.field("actor.message"), Some("Ping"));
    assert_eq!(span.field("actor.dispatch"), Some("ask"), "ask vs tell");
}

#[cfg(feature = "opentelemetry")]
#[tokio::test]
async fn message_handler_span_carries_otel_fields() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Pinged)
        .await
        .expect("init");

    handle.ask(Ping).exchange().await.expect("ask");

    let span = records_for("Pinged")
        .into_iter()
        .find(|r| r.is_span && r.name == "factories.handle_message")
        .expect("a message-handler span was emitted");

    // A dynamic operation name so exporters group per Actor.Message instead of
    // collapsing under the static span name. Kind stays INTERNAL (the default),
    // so we deliberately do NOT stamp otel.kind.
    assert_eq!(span.field("otel.name"), Some("Pinged.Ping"));
    assert_eq!(span.field("otel.kind"), None, "in-process dispatch is INTERNAL");
}

#[tokio::test]
async fn message_handler_span_parents_to_the_call_site() {
    install_subscriber();
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Pinged)
        .await
        .expect("init");

    // Send from inside a known caller span: the handler span must hang off it
    // (message-as-function-call), even though the handler runs on the actor task.
    let caller = tracing::info_span!("caller_site");
    let caller_id = caller.id().map(|id| id.into_u64());
    {
        let _entered = caller.enter();
        handle.ask(Ping).exchange().await.expect("ask");
    }

    assert!(caller_id.is_some(), "the caller span is enabled");
    let parented = all_records().into_iter().any(|r| {
        r.is_span && r.name == "factories.handle_message" && r.parent == caller_id
    });
    assert!(
        parented,
        "the handler span must be parented to the send-site span"
    );
}

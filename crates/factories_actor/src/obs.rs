//! Internal `tracing` instrumentation helpers.
//!
//! Every entry point here is a no-op without the `tracing` feature, so the run
//! loop and lifecycle call sites stay free of `#[cfg]` noise.
//!
//! The model (see the per-message span in
//! [`dispatch`](crate::actor::dispatch)):
//!
//! - **Lifecycle hooks** ([`instrument_on_start`] / [`instrument_on_stop`]) get
//!   *short-lived* `DEBUG` spans that wrap only the hook future - never the
//!   actor's whole lifetime, so there is no long-running span to stall an
//!   OpenTelemetry exporter.
//! - **Lifecycle transitions** ([`actor_started`] / [`actor_stopped`] /
//!   [`actor_aborted`]) are *events*, not spans, carrying `actor.id` +
//!   `actor.name` - the cross-ecosystem convention (Akka, Erlang/OTP, Orleans).
//! - The actor is identified by fields, not a wrapping span; association is by
//!   `actor.id` / `actor.name` plus the per-message span's causal parent chain.

use crate::actor::lifecycle::TerminationKind;
use crate::actor::supervision::ActorId;
use core::future::Future;

/// Run an actor's start-hook future under a short-lived `DEBUG` span.
///
/// `on_start` can fail the actor (via `ActorContext::fail`), which records the
/// error status onto this span directly (see [`record_current_span_error`]); the
/// `error.type` / `otel.status_code` fields are declared `Empty` here so that
/// `record` can fill them.
#[cfg(feature = "tracing")]
pub(crate) fn run_on_start<F: Future>(
    actor: &'static str,
    id: ActorId,
    fut: F,
) -> impl Future<Output = F::Output> {
    let span = tracing::debug_span!(
        "factories.on_start",
        actor.name = actor,
        actor.id = id.as_usize() as u64,
        error.type = tracing::field::Empty,
        otel.name = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
    );
    #[cfg(feature = "opentelemetry")]
    span.record("otel.name", alloc::format!("{actor}.on_start").as_str());

    tracing::Instrument::instrument(fut, span)
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) fn run_on_start<F: Future>(_: &'static str, _: ActorId, fut: F) -> F {
    fut
}

/// Record an error status on the currently-active span - the operation (handler
/// or start hook) that just failed the actor. Emits the standard `error.type`
/// attribute, plus the `otel.status_code` mapping under the `opentelemetry`
/// feature. A no-op without `tracing`, or if the current span didn't declare
/// these fields (e.g. `fail` called from outside an instrumented operation).
#[cfg(feature = "tracing")]
pub(crate) fn record_current_span_error(error_type: &'static str) {
    let span = tracing::Span::current();
    span.record("error.type", error_type);
    #[cfg(feature = "opentelemetry")]
    span.record("otel.status_code", "ERROR");
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) fn record_current_span_error(_: &'static str) {}

/// Run an actor's stop-hook future under a short-lived `DEBUG` span.
///
/// No status is recorded: the stop hook does not itself fail the actor (the
/// actor's outcome is already decided by the time it runs), so per the spec the
/// span's status stays `Unset`.
#[cfg(feature = "tracing")]
pub(crate) async fn run_on_stop<F: Future>(actor: &'static str, id: ActorId, fut: F) -> F::Output {
    use tracing::Instrument;

    let span = tracing::debug_span!(
        "factories.on_stop",
        actor.name = actor,
        actor.id = id.as_usize() as u64,
        otel.name = tracing::field::Empty,
    );
    #[cfg(feature = "opentelemetry")]
    span.record("otel.name", alloc::format!("{actor}.on_stop").as_str());

    fut.instrument(span).await
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) async fn run_on_stop<F: Future>(_: &'static str, _: ActorId, fut: F) -> F::Output {
    fut.await
}

/// Emit the "actor started" event, once the actor has reached `Running`.
#[cfg(feature = "tracing")]
pub(crate) fn actor_started(actor: &'static str, id: ActorId) {
    tracing::debug!(actor.name = actor, actor.id = id.as_usize() as u64, "actor started");
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) fn actor_started(_: &'static str, _: ActorId) {}

/// Emit the "actor stopped" event after the stop hook has run. A clean drain is
/// `DEBUG`; a handler-induced failure is `WARN`.
#[cfg(feature = "tracing")]
pub(crate) fn actor_stopped(actor: &'static str, id: ActorId, outcome: &'static str, failed: bool) {
    if failed {
        tracing::warn!(actor.name = actor, actor.id = id.as_usize() as u64, outcome, "actor stopped");
    } else {
        tracing::debug!(actor.name = actor, actor.id = id.as_usize() as u64, outcome, "actor stopped");
    }
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) fn actor_stopped(_: &'static str, _: ActorId, _: &'static str, _: bool) {}

/// Emit the "actor aborted" event for the terminal drop path (panic / task
/// abort), where neither the stop hook nor the clean stopped event ran. `WARN`,
/// since an abort is an abnormal outcome.
#[cfg(feature = "tracing")]
pub(crate) fn actor_aborted(actor: &'static str, id: ActorId) {
    tracing::warn!(actor.name = actor, actor.id = id.as_usize() as u64, "actor aborted");
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) fn actor_aborted(_: &'static str, _: ActorId) {}

/// Emit a `DEBUG` event as a `Terminated` signal is pushed to a watcher. The
/// fields name the *watched* actor (the one that terminated) and the outcome.
#[cfg(feature = "tracing")]
pub(crate) fn terminated_delivered(watched: &'static str, id: ActorId, kind: TerminationKind) {
    tracing::debug!(
        actor.name = watched,
        actor.id = id.as_usize() as u64,
        kind = ?kind,
        "delivering terminated signal",
    );
}

/// No-op without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
pub(crate) fn terminated_delivered(_: &'static str, _: ActorId, _: TerminationKind) {}

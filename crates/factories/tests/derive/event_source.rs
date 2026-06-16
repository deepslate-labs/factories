//! Event sources on a derived actor, both paths:
//!
//! - [`Ticker`] uses `#[event_source]` - stateless, autoref-detected, no
//!   `EventDriver` written by hand. The macro counterpart of the hand-written
//!   `Ticker` in `spawn.rs`.
//! - [`Beacon`] uses a hand-written stateful `EventDriver` wired through
//!   `#[actor(event_driver = ...)]` - the escape hatch for a source that needs
//!   private mutable state across turns, which `#[event_source]` deliberately
//!   leaves to a manual driver.

use std::sync::atomic::{AtomicU32, Ordering};

use factories::actor::dispatch::DispatchedActorMessage;
use factories::actor::event::{EventContext, EventDriver};
use factories::actor::{Actor, ActorContext};
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::{ActorLauncher, ActorMailbox};

use crate::util::assert_type_eq;

const TICK_BUDGET: u32 = 3;

#[derive(Default)]
pub struct TickerShared {
    fired: AtomicU32,
}

#[derive(Actor)]
#[actor(
    lock = UnguardedLock<Self>,
    run_loop = SequentialRunLoop<Self>,
    shared = TickerShared,
)]
struct Ticker {
    total: u32,
}

#[factories::messages]
impl Ticker {
    /// The event source: fire `Tick` self-messages until the handler has
    /// processed the budget (read through the shared counter), then defer to
    /// the mailbox - the actor's own lever against starvation. Stateless: every
    /// bit of coordination goes through `cx`, no driver state.
    #[event_source]
    async fn drive(
        cx: EventContext<'_, Self>,
        mailbox: &mut (impl ActorMailbox + Send),
    ) -> Option<DispatchedActorMessage> {
        if cx.extension().fired.load(Ordering::Acquire) < TICK_BUDGET {
            return Some(cx.message(Tick));
        }
        mailbox.receive().await
    }

    /// Bumps both the actor's own tally and the shared counter the event source
    /// watches, so the two coordinate without contending on the actor lock.
    #[handler]
    async fn tick(&mut self, #[context] actor: ActorContext<'_, Self>) {
        self.total += 1;
        actor.extension().fired.fetch_add(1, Ordering::AcqRel);
    }

    #[handler]
    async fn get_total(&self) -> u32 {
        self.total
    }
}

#[test]
fn derive_wires_the_generated_event_loop() {
    // Approach (a): the derive always routes through its generated loop, which
    // autoref-detects the `#[event_source]` impl.
    assert_type_eq::<<Ticker as Actor>::EventDriver, TickerEventLoop>();
}

#[tokio::test]
async fn event_source_fires_until_budget_then_defers() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Ticker { total: 0 })
        .await
        .expect("ticker init is infallible");

    // The driver drains its whole budget (coordinating with the handler via the
    // shared counter) before it ever polls the mailbox, so the first query sees
    // every tick.
    let total = handle.get_total().await.expect("ask");
    assert_eq!(
        total, TICK_BUDGET,
        "event source should fire its whole budget"
    );

    // No further ticks once the budget drained.
    let again = handle.get_total().await.expect("ask");
    assert_eq!(again, TICK_BUDGET, "no ticks after the budget drained");
}

// ---------------------------------------------------------------------------
// Stateful escape hatch: a hand-written `EventDriver` wired through
// `#[actor(event_driver = ...)]`. This is the path for a source that needs
// private mutable state across turns (here a countdown the loop preserves) -
// the capability `#[event_source]` deliberately leaves to a manual driver. The
// derive uses the named driver verbatim instead of generating one.
// ---------------------------------------------------------------------------

#[derive(Actor)]
#[actor(
    lock = UnguardedLock<Self>,
    run_loop = SequentialRunLoop<Self>,
    event_driver = CountdownDriver,
)]
struct Beacon {
    /// Seeds the driver's countdown.
    budget: u32,
    /// Counts the pings the handler has processed.
    pings: u32,
}

#[factories::messages]
impl Beacon {
    #[handler]
    async fn ping(&mut self) {
        self.pings += 1;
    }

    #[handler]
    async fn get_pings(&self) -> u32 {
        self.pings
    }
}

/// A stateful driver: counts down its own `remaining` field - private mutable
/// state the run loop preserves across turns - firing a `Ping` each turn until
/// it hits zero, then deferring to the mailbox.
struct CountdownDriver {
    remaining: u32,
}

// Seeded from the actor: the driver is built from `&actor` after init, so it
// can read the actor's state. This is the capability the stateless
// `#[event_source]` cannot offer (its only state, the extension, predates the
// actor).
impl From<&Beacon> for CountdownDriver {
    fn from(actor: &Beacon) -> Self {
        Self {
            remaining: actor.budget,
        }
    }
}

// `M: Send` because `next` is an `async` block capturing `&mut M` across its
// await: the loop is `ThreadSafe`, so `EventDriver::next` now demands a `Send`
// future. (`DefaultMailboxDriver` sidesteps this by returning `receive()`
// directly, capturing nothing extra.)
impl<M: ActorMailbox + Send> EventDriver<Beacon, M> for CountdownDriver {
    fn next<'a>(
        &'a mut self,
        cx: EventContext<'a, Beacon>,
        mailbox: &'a mut M,
    ) -> impl Future<Output = Option<DispatchedActorMessage>> + 'a {
        async move {
            if self.remaining > 0 {
                // Cancel-safe: no await sits between the mutation and the
                // return, so this branch cannot be dropped mid-update.
                self.remaining -= 1;
                return Some(cx.message(Ping));
            }
            mailbox.receive().await
        }
    }
}

#[test]
fn event_driver_attribute_overrides_the_generated_loop() {
    assert_type_eq::<<Beacon as Actor>::EventDriver, CountdownDriver>();
}

#[tokio::test]
async fn manual_stateful_driver_fires_seeded_budget() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(
            &spawner,
            Beacon {
                budget: 4,
                pings: 0,
            },
        )
        .await
        .expect("beacon init is infallible");

    // The driver fires its whole actor-seeded budget before touching the
    // mailbox, so the first query sees every ping.
    assert_eq!(handle.get_pings().await.expect("ask"), 4);
    // Countdown drained: no further pings.
    assert_eq!(handle.get_pings().await.expect("ask"), 4);
}

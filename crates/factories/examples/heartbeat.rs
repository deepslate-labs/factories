//! A self-driving actor: `#[event_source]` plus lifecycle hooks.
//!
//! `Heartbeat` drives itself on a timer. Each turn its event source either races
//! a short sleep to mint a self-`Tick` (up to a small budget) or falls back to
//! the mailbox. `#[on_start]` / `#[on_stop]` bracket its life.
//!
//! Run with: `cargo run -p factories --example heartbeat`

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use factories::actor::dispatch::DispatchedActorMessage;
use factories::actor::event::EventContext;
use factories::actor::lifecycle::StopReason;
use factories::actor::ActorContext;
use factories::prelude::*;
use factories::spawn::ActorMailbox;

/// How many self-ticks the heartbeat fires before it goes quiet and only
/// answers the mailbox.
const TICK_BUDGET: u32 = 3;

/// Lock-free state shared between the (stateless) event source and the handler.
/// `#[event_source]` keeps no driver state of its own, so the budget it counts
/// down lives here, where both halves can see it without contending on the
/// actor lock.
#[derive(Default)]
pub struct Pacer {
    fired: AtomicU32,
}

#[derive(Actor)]
#[actor(shared = Pacer)]
struct Heartbeat {
    /// The actor's own tally, bumped by the `Tick` handler.
    beats: u32,
}

#[factories::messages]
impl Heartbeat {
    /// The event source. Stateless: every turn it consults the shared `Pacer`,
    /// and while there is budget left it sleeps briefly, then returns a
    /// self-`Tick` built with `cx.message(..)`. Once the budget drains it hands
    /// the turn back to the mailbox - the actor's own lever against a runaway
    /// timer starving real messages.
    #[event_source]
    async fn drive(
        cx: EventContext<'_, Self>,
        mailbox: &mut (impl ActorMailbox + Send),
    ) -> Option<DispatchedActorMessage> {
        if cx.shared_data().fired.load(Ordering::Acquire) < TICK_BUDGET {
            // A heartbeat interval. Short so the example finishes in a blink.
            tokio::time::sleep(Duration::from_millis(20)).await;
            return Some(cx.message(Tick));
        }
        // Budget spent: behave like any ordinary actor and await real traffic.
        mailbox.receive().await
    }

    /// Handles a self-`Tick`: bumps the actor's tally and the shared counter the
    /// event source watches, keeping the two coordinated.
    #[handler]
    async fn tick(&mut self, #[context] cx: ActorContext<'_, Self>) {
        self.beats += 1;
        cx.shared_data().fired.fetch_add(1, Ordering::AcqRel);
        println!("  tick! beat #{}", self.beats);
    }

    /// Ask handler: report the current beat count.
    #[handler]
    async fn beats(&self) -> u32 {
        self.beats
    }

    /// Runs before `Running` is observable - the actor announces itself.
    #[on_start]
    async fn announce(&mut self, _cx: ActorContext<'_, Self>) {
        println!("[on_start] heartbeat coming online (budget = {TICK_BUDGET})");
    }

    /// Runs once on a clean drain. `StopReason` distinguishes a graceful finish
    /// (all handles dropped) from a failure, so the hook can react accordingly.
    #[on_stop]
    async fn farewell(self, reason: StopReason<'_, Self>, _cx: ActorContext<'_, Self>) {
        match reason {
            StopReason::Finished => {
                println!("[on_stop] graceful shutdown after {} beats", self.beats)
            }
            StopReason::Failed(_) => println!("[on_stop] shutting down after a failure"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let handle = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Heartbeat { beats: 0 })
        .await?;

    // Let the event source race through its budget. With a 20ms interval and a
    // budget of 3, well under 200ms is plenty.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // The budget is spent and the source now defers to the mailbox, so this ask
    // is served promptly and sees every tick.
    println!("main: observed {} beats", handle.beats().await?);

    // Capture the shared state so we can wait for the actor to actually wind
    // down, then drop the last handle: with no senders left the mailbox closes,
    // the loop drains, and `on_stop` runs with `StopReason::Finished`.
    let state = handle.state().clone();
    drop(handle);
    state.wait_for_terminal().await;

    Ok(())
}

//! Supervision via watch / `Terminated`.
//!
//! One actor (`Supervisor`) *watches* another (`Task`) and reacts when it dies.
//! Watching is unidirectional and weak: the supervisor learns of the death but
//! never keeps the watched actor alive. The death arrives as an ordinary
//! `Terminated` message, handled like any other.
//!
//! Run with: `cargo run -p factories --example supervision`

use factories::prelude::*;
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;

use std::sync::{Arc, Mutex};

// The supervisor's memory of who died and how. A watcher is just a normal
// actor, so it records into its own state; we keep that state in a shared
// extension so the spawner side can query it after the actor has handled the
// signal. `(tag, kind)` is exactly what `Terminated` exposes.
#[derive(Default, Clone)]
struct DeathLog(Arc<Mutex<Vec<(u64, TerminationKind)>>>);

impl DeathLog {
    fn record(&self, tag: u64, kind: TerminationKind) {
        self.0.lock().expect("death log").push((tag, kind));
    }

    fn snapshot(&self) -> Vec<(u64, TerminationKind)> {
        self.0.lock().expect("death log").clone()
    }
}

// The watcher. Serial-by-default; its only state is the shared death log.
#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>, shared = DeathLog)]
struct Supervisor;

#[factories::messages]
impl Supervisor {
    // Handling `Terminated` is what makes an actor a watcher. We take the whole
    // message (`#[message]`) because its payload is read through methods -
    // `.tag()` (the correlation key we passed to `watch`) and `.kind()` (how the
    // watched actor left: Finished / Failed / Aborted) - not struct fields.
    #[handler(message = Terminated)]
    fn on_terminated(
        &mut self,
        #[message] terminated: Terminated,
        #[context] cx: ActorContext<'_, Self>,
    ) {
        let (tag, kind) = (terminated.tag(), terminated.kind());
        println!("[supervisor] watched actor under tag {tag} left: {kind:?}");
        cx.shared_data().record(tag, kind);
    }

    // Query handler so the program can observe what the supervisor recorded.
    #[handler]
    async fn deaths(&self, #[context] cx: ActorContext<'_, Self>) -> Vec<(u64, TerminationKind)> {
        cx.shared_data().snapshot()
    }
}

// The child. It does a small unit of work, then has nothing left to do; once
// its last handle drops it finishes cleanly (a `Finished` termination).
#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Task {
    done: u32,
}

#[factories::messages]
impl Task {
    #[handler]
    async fn work(&mut self) -> u32 {
        self.done += 1;
        println!("[task] completed unit #{}", self.done);
        self.done
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spawner = TokioTaskSpawner::current();

    let supervisor = ActorLauncher::default()
        .spawn_ready(&spawner, Supervisor)
        .await?;
    let task = ActorLauncher::default()
        .spawn_ready(&spawner, Task { done: 0 })
        .await?;

    // Unidirectional, explicit: the supervisor watches the task under tag 42.
    // The tag is a correlation key chosen by the watcher - it will come back on
    // the `Terminated` signal, so one supervisor can tell its children apart.
    supervisor.watch(&task, 42);
    println!("[main] supervisor now watching task under tag 42");

    // Let the child do its work.
    task.work().await?;

    // Drop the task's last handle. With nothing left to keep it running, the
    // child's mailbox closes, its loop drains, and a `Terminated { tag: 42,
    // kind: Finished }` is pushed into the supervisor's mailbox. We grab the
    // shared lifecycle state first so we can await the child reaching its
    // terminal state without holding it alive.
    let task_state = task.state().clone();
    drop(task);
    task_state.wait_for_terminal().await;
    println!("[main] task dropped; it has reached its terminal state");

    // The `Terminated` was enqueued before this ask (the mailbox is FIFO), so by
    // the time the supervisor answers it has already handled the death.
    let deaths = supervisor.deaths().await?;
    println!("[main] supervisor recorded: {deaths:?}");

    assert_eq!(deaths, vec![(42, TerminationKind::Finished)]);
    println!("[main] confirmed: the watched task finished cleanly under tag 42");

    Ok(())
}

//! Actor failure from handlers: `die_on_err` (forward and consume modes) and
//! manual failing through `#[context]`.

use factories::actor::channel::ActorChannelSendable;
use factories::actor::state::{LifecycleState, SharedActorState};
use factories::actor::{Actor, ActorContext};
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::ActorLauncher;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Boom(u32);

#[derive(Actor)]
#[actor(error = Boom, lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Fragile {
    limit: u32,
}

#[factories::messages]
impl Fragile {
    /// Forward mode: the asker receives the full result, the actor dying is
    /// a side effect of the error.
    #[handler(die_on_err)]
    fn checked_sub(&mut self, value: u32) -> Result<u32, Boom> {
        self.limit = self.limit.checked_sub(value).ok_or(Boom(value))?;
        Ok(self.limit)
    }

    /// Consume mode: the answer is the Ok part, the error only feeds the
    /// actor's death.
    #[handler(die_on_err = consume)]
    fn strict_sub(&mut self, value: u32) -> Result<u32, Boom> {
        self.limit = self.limit.checked_sub(value).ok_or(Boom(value))?;
        Ok(self.limit)
    }

    /// Manual failing through the actor context.
    #[handler]
    fn poison(&mut self, #[context] actor: ActorContext<'_, Self>) {
        actor.fail(Boom(0));
    }
}

/// Forward mode on the concurrent default loop, exercising its error check.
#[derive(Actor)]
#[actor(error = Boom)]
struct FragileConcurrent {
    limit: u32,
}

#[factories::messages]
impl FragileConcurrent {
    #[handler(die_on_err)]
    fn deplete(&mut self, value: u32) -> Result<u32, Boom> {
        self.limit = self.limit.checked_sub(value).ok_or(Boom(value))?;
        Ok(self.limit)
    }
}

// -- Tests ----------------------------------------------------------------------

/// Wait until the actor's lifecycle reaches `Dead`.
async fn wait_dead<A: Actor>(state: &SharedActorState<A>) {
    state.wait_for_terminal().await;
}

#[tokio::test]
async fn die_on_err_forwards_error_and_kills() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Fragile { limit: 10 })
        .await
        .expect("fragile init is infallible");

    // Healthy ask: full result as the answer, actor stays alive.
    assert_eq!(
        handle
            .ask(CheckedSub { value: 3 })
            .exchange()
            .await
            .expect("ask"),
        Ok(7)
    );
    assert_eq!(handle.state().lifecycle(), LifecycleState::Running);

    // Failing ask: the asker still receives the error - the death is a side
    // effect, observable as lifecycle + actor error.
    assert_eq!(
        handle
            .ask(CheckedSub { value: 100 })
            .exchange()
            .await
            .expect("ask"),
        Err(Boom(100))
    );

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().failed_error(), Some(&Boom(100)));
}

#[tokio::test]
async fn die_on_err_consume_closes_answer_and_kills() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Fragile { limit: 10 })
        .await
        .expect("fragile init is infallible");

    // Consume mode unwraps the answer to the Ok part.
    assert_eq!(
        handle
            .ask(StrictSub { value: 4 })
            .exchange()
            .await
            .expect("ask"),
        6
    );

    // On error the answer channel just closes; the error feeds the death.
    let answer = handle.ask(StrictSub { value: 100 }).exchange().await;
    assert!(
        answer.is_err(),
        "the answer channel must close when the death consumes the error"
    );

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().failed_error(), Some(&Boom(100)));
}

#[tokio::test]
async fn context_fail_kills_actor() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Fragile { limit: 0 })
        .await
        .expect("fragile init is infallible");

    handle.tell(Poison).send().await.expect("tell");

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().failed_error(), Some(&Boom(0)));
}

#[tokio::test]
async fn die_on_err_on_concurrent_loop() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, FragileConcurrent { limit: 5 })
        .await
        .expect("fragile init is infallible");

    assert_eq!(
        handle
            .ask(Deplete { value: 100 })
            .exchange()
            .await
            .expect("ask"),
        Err(Boom(100))
    );

    wait_dead(handle.state()).await;
    assert_eq!(handle.state().failed_error(), Some(&Boom(100)));
}

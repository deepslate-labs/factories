use crate::actor::handle::TypedActorHandle;
use crate::actor::state::{LifecycleState, SharedActorState};
use crate::actor::{Actor, ActorInit};
use crate::spawn::{ActorTaskSpawner, CreatableChannel, IntoActorInit, SpawnableRunLoop};
use typed_builder::TypedBuilder;

/// Builder for the standard actor assembly process.
///
/// This is the front door for spawning actors whose parts implement the assembly
/// contracts. It performs exactly the public layer-0 steps - everything it does
/// can be done by hand:
///
/// 1. [`CreatableChannel::create`] - channel + mailbox from options
/// 2. [`SharedActorState::new`]
/// 3. [`SpawnableRunLoop::run_with`] - the loop future from config + parts
/// 4. [`ActorTaskSpawner::spawn`] - task handle, attached to the shared state
/// 5. [`TypedActorHandle::assemble`]
#[derive(TypedBuilder)]
pub struct ActorBuilder<A: Actor>
where
    A::Channel: CreatableChannel,
    A::RunLoop: SpawnableRunLoop<A>,
    A::Error: Send + Sync + 'static,
{
    /// Options used to create the channel.
    #[builder(default, default_where(<<A as Actor>::Channel as CreatableChannel>::CreationOptions: Default))]
    channel_options: <A::Channel as CreatableChannel>::CreationOptions,

    /// Configuration consumed by the run loop.
    #[builder(default, default_where(<<A as Actor>::RunLoop as SpawnableRunLoop<A>>::Config: Default))]
    loop_config: <A::RunLoop as SpawnableRunLoop<A>>::Config,

    /// The runtime binder placed on the actor identity.
    #[builder(default, default_where(<A as Actor>::RuntimeBinder: Default))]
    binder: A::RuntimeBinder,
}

/// All parts defaultable: `ActorBuilder::default().spawn(...)` skips the
/// builder ceremony for the standard kit.
impl<A: Actor> Default for ActorBuilder<A>
where
    A::Channel: CreatableChannel,
    A::RunLoop: SpawnableRunLoop<A>,
    A::Error: Send + Sync + 'static,
    <A::Channel as CreatableChannel>::CreationOptions: Default,
    <A::RunLoop as SpawnableRunLoop<A>>::Config: Default,
    A::RuntimeBinder: Default,
{
    fn default() -> Self {
        Self {
            channel_options: Default::default(),
            loop_config: Default::default(),
            binder: Default::default(),
        }
    }
}

impl<A: Actor> ActorBuilder<A>
where
    A::Channel: CreatableChannel,
    A::RunLoop: SpawnableRunLoop<A>,
    A::Error: Send + Sync + 'static,
{
    /// Assemble and fire. The initializer crosses into the spawned task, where
    /// [`ActorInit::init`] constructs the actor.
    ///
    /// Messages sent before init completes queue in the mailbox. If init fails,
    /// the error lands in the shared state, the mailbox closes and senders
    /// observe [`crate::actor::channel::ActorChannelSendError::ActorDead`].
    pub fn spawn<I, M>(self, spawner: &impl ActorTaskSpawner, init: I) -> TypedActorHandle<A>
    where
        I: IntoActorInit<A, M>,
        I::Init: Send + 'static,
        <I::Init as ActorInit<A>>::Fut: Send,
    {
        let (channel, mailbox) = A::Channel::create(self.channel_options);
        let shared = SharedActorState::new();

        let fut = A::RunLoop::run_with(self.loop_config, init.into_init(), shared.clone(), mailbox);
        let task = spawner.spawn(fut);
        let _ = shared.attach_task(task);

        TypedActorHandle::assemble(channel, self.binder, shared)
    }

    /// Spawn and wait until init has resolved.
    ///
    /// Returns the actor's init error if initialization failed. An actor that
    /// ran and exited normally before this observed `Running` yields `Ok`.
    pub async fn spawn_ready<I, M>(
        self,
        spawner: &impl ActorTaskSpawner,
        init: I,
    ) -> Result<TypedActorHandle<A>, A::Error>
    where
        I: IntoActorInit<A, M>,
        I::Init: Send + 'static,
        <I::Init as ActorInit<A>>::Fut: Send,
        A::Error: Clone,
    {
        let handle = self.spawn(spawner, init);

        match handle.state().wait_leave_starting().await {
            LifecycleState::Dead => match handle.state().get_error() {
                Some(error) => Err(error.clone()),
                None => Ok(handle),
            },
            _ => Ok(handle),
        }
    }
}

use crate::actor::Actor;
use crate::runtime::standard::StandardChannel;
use typed_builder::TypedBuilder;

/// Standard actor that can easily be spawned.
#[derive(TypedBuilder)]
pub struct StandardActor<A: Actor + ?Sized>
where
    A::Channel: StandardChannel,
{
    #[builder(default, default_where(A::RunLoop: Default))]
    run_loop: A::RunLoop,

    #[builder(default, default_where(<<A as Actor>::Channel as StandardChannel>::CreationOptions: Default))]
    channel_options: <<A as Actor>::Channel as StandardChannel>::CreationOptions,
}

use crate::actor::{Actor, ActorInit};
use crate::runtime::init::InitFn;

/// Conversion into an [`ActorInit`].
///
/// Spawn entry points accept anything convertible: actor values and custom
/// initializers (everything implementing [`ActorInit`]) pass through
/// unchanged, closures producing the init future get wrapped in
/// [`InitFn`]:
///
/// ```ignore
/// builder.spawn_ready(&spawner, MyActor { value: 10 });
/// builder.spawn_ready(&spawner, MyInit { config });
/// builder.spawn_ready(&spawner, || async move { Ok(MyActor::connect(addr).await?) });
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot initialize an actor of type `{A}`",
    note = "expected the actor itself, an `ActorInit<{A}>`, a closure `|| async {{ ... }}` \
            returning `Result<{A}, Error>` or something else that implements `ActorInit<{A}>`"
)]
pub trait IntoActorInit<A: Actor, Marker> {
    /// The initializer this converts into.
    type Init: ActorInit<A>;

    /// Perform the conversion.
    fn into_init(self) -> Self::Init;
}

/// [`IntoActorInit`] marker: the value already is an [`ActorInit`].
#[derive(Debug)]
pub struct IsInit;

impl<A: Actor, I: ActorInit<A>> IntoActorInit<A, IsInit> for I {
    type Init = I;

    fn into_init(self) -> I {
        self
    }
}

/// [`IntoActorInit`] marker: the value is a closure producing the init future.
#[derive(Debug)]
pub struct IsInitFn;

impl<A, F, Fut> IntoActorInit<A, IsInitFn> for F
where
    A: Actor,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<A, A::Error>>,
{
    type Init = InitFn<F>;

    fn into_init(self) -> InitFn<F> {
        InitFn::new(self)
    }
}

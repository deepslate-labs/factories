//! `ActorTemplate`: reusable component bundles, with explicit keys overriding
//! individual members.

use factories_actor::actor::{Actor, ActorInit, IdentityActorInit, StaticOnlyBinder};
use factories_actor::runtime::kanal::SimpleKanalActorChannel;
use factories_actor::runtime::lock::UnguardedLock;
use factories_actor::runtime::registry::RegistryBinder;
use factories_actor::runtime::sequential_loop::SequentialRunLoop;
use factories_actor::runtime::template::ActorTemplate;
use factories_actor::runtime::tokio::TokioTaskSpawner;
use factories_actor::spawn::ActorBuilder;

use crate::actor::CustomError;
use crate::util::assert_type_eq;

struct SequentialSet;

impl ActorTemplate for SequentialSet {
    type Channel = SimpleKanalActorChannel;
    type Error = core::convert::Infallible;
    type RuntimeBinder<A: Actor> = StaticOnlyBinder;
    type LockStrategy<A: Actor> = UnguardedLock<A>;
    type RunLoop<A: Actor> = SequentialRunLoop<A>;
}

#[derive(Actor)]
#[actor(template = SequentialSet)]
struct Templated {
    total: u32,
}

#[factories_actor::messages]
impl Templated {
    #[handler]
    fn bump(&mut self, by: u32) -> u32 {
        self.total += by;
        self.total
    }
}

/// Explicit keys override individual template members.
#[derive(Actor)]
#[actor(template = SequentialSet, error = CustomError, binder = RegistryBinder<Self>)]
struct TemplatedOverride;

// -- Tests ----------------------------------------------------------------------

#[test]
fn template_supplies_components() {
    assert_type_eq::<<Templated as Actor>::Channel, SimpleKanalActorChannel>();
    assert_type_eq::<<Templated as Actor>::Error, core::convert::Infallible>();
    assert_type_eq::<<Templated as Actor>::RuntimeBinder, StaticOnlyBinder>();
    assert_type_eq::<<Templated as Actor>::LockStrategy, UnguardedLock<Templated>>();
    assert_type_eq::<<Templated as Actor>::RunLoop, SequentialRunLoop<Templated>>();
}

#[test]
fn explicit_keys_override_template_members() {
    assert_type_eq::<<TemplatedOverride as Actor>::Error, CustomError>();
    assert_type_eq::<<TemplatedOverride as Actor>::RuntimeBinder, RegistryBinder<TemplatedOverride>>();
    // Untouched members still come from the template.
    assert_type_eq::<<TemplatedOverride as Actor>::LockStrategy, UnguardedLock<TemplatedOverride>>();
    assert_type_eq::<<TemplatedOverride as Actor>::RunLoop, SequentialRunLoop<TemplatedOverride>>();
}

#[tokio::test]
async fn templated_actor_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Templated>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Templated { total: 0 }).init(),
        )
        .await
        .expect("templated init is infallible");

    assert_eq!(handle.ask(Bump { by: 3 }).exchange().await.expect("ask"), 3);
    assert_eq!(handle.ask(Bump { by: 4 }).exchange().await.expect("ask"), 7);
}

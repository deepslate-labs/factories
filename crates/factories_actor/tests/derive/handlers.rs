//! `#[messages]` basics: message generation from signatures, decomposition of
//! existing messages, additivity (methods stay callable) and automatic
//! dynamic-dispatch registration.

use factories_actor::actor::channel::ActorChannelSendable;
use factories_actor::actor::handle::ActorHandle;
use factories_actor::actor::{ActorInit, IdentityActorInit};
use factories_actor::message::Message;
use factories_actor::message::envelope::MessageEnvelope;
use factories_actor::runtime::tokio::TokioTaskSpawner;
use factories_actor::spawn::ActorBuilder;

use crate::actor::{Customized, Defaulted, Get};
use crate::util::assert_type_eq;

/// Existing message decomposed into handler parameters by field name.
#[derive(Debug, Message)]
#[message(answer = u32)]
pub struct AddBoth {
    pub left: u32,
    pub right: u32,
}

#[factories_actor::messages]
impl Customized {
    /// Stays a plain method (the macro is additive), additionally reachable
    /// through the generated `Touch` message.
    #[handler]
    pub fn touch(&mut self) {
        self.hits += 1;
    }

    /// `&self` receiver: runs under shared access (UnguardedLock supports it).
    #[handler]
    async fn hits_now(&self) -> u32 {
        self.hits
    }

    /// Decomposes the existing `AddBoth` message instead of generating one.
    #[handler(message = AddBoth)]
    fn add_both(&mut self, left: u32, right: u32) -> u32 {
        self.hits += left + right;
        self.hits
    }
}

#[factories_actor::messages]
impl Defaulted {
    /// On a registry-bound actor: the handler must be reachable dynamically
    /// without an explicit `register_dynamic_handler!`.
    #[handler]
    fn add(&mut self, amount: u32) {
        self.value += amount;
    }

    /// Target of the envelope-forwarding test in [`crate::markers`].
    #[handler]
    pub fn probe(&mut self) -> u32 {
        self.value
    }
}

// -- Tests ----------------------------------------------------------------------

#[test]
fn handler_methods_stay_plain_methods() {
    let mut customized = Customized { hits: 0 };
    customized.touch();
    assert_eq!(customized.hits, 1);
}

#[test]
fn generated_message_shapes() {
    assert_type_eq::<<Touch as Message>::Answer, ()>();
    assert_type_eq::<<HitsNow as Message>::Answer, u32>();
    assert_type_eq::<<Add as Message>::Answer, ()>();
    assert_eq!(<Touch as Message>::RTTI.name(), "Touch");
    assert_eq!(<HitsNow as Message>::RTTI.name(), "HitsNow");
}

#[tokio::test]
async fn method_handlers_roundtrip() {
    let spawner = TokioTaskSpawner::current();

    let handle = ActorBuilder::<Customized>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Customized { hits: 0 }).init(),
        )
        .await
        .expect("customized init is infallible");

    // Generated unit message, tell-style.
    handle.tell(Touch).send().await.expect("tell");

    // Generated ask through a `&self`/shared/async handler.
    assert_eq!(handle.ask(HitsNow).exchange().await.expect("ask"), 1);

    // Existing message decomposed into parameters.
    assert_eq!(
        handle
            .ask(AddBoth { left: 2, right: 3 })
            .exchange()
            .await
            .expect("ask"),
        6
    );
}

#[tokio::test]
async fn method_handlers_register_dynamically() {
    let spawner = TokioTaskSpawner::current();

    let typed = ActorBuilder::<Defaulted>::builder()
        .build()
        .spawn_ready(
            &spawner,
            IdentityActorInit::new(Defaulted { value: 7 }).init(),
        )
        .await
        .expect("defaulted init is infallible");
    let erased = typed.clone().erase_type();

    // The #[messages]-generated handler registered itself: the message binds
    // dynamically on the registry-bound actor.
    let envelope = MessageEnvelope::new(Add { amount: 5 }, None);
    erased
        .prepare_send_dynamic(envelope)
        .expect("Add must bind dynamically")
        .send()
        .await
        .expect("send must succeed");

    assert_eq!(typed.ask(Get).exchange().await.expect("ask"), 12);
}

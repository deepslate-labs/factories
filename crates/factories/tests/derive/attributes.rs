//! Attributes on `#[messages]` methods: a handler's doc comments are copied onto
//! its generated message, and a `#[cfg]` on a handler, an event source or a
//! lifecycle hook gates every generated piece along with the method itself.

// The gated-out items below are stripped before name resolution, so their
// types are spelled by path rather than imported (an import would be unused).
use factories::prelude::*;
use factories::runtime::tokio::TokioTaskSpawner;

#[derive(Actor)]
struct Gated {
    value: u32,
}

#[factories::messages]
impl Gated {
    /// Documented: this comment is copied onto the generated `Bump` message.
    #[handler]
    fn bump(&mut self, amount: u32) -> u32 {
        self.value += amount;
        self.value
    }

    /// `Missing` is not a type anywhere in this crate: this compiles only if
    /// the gate reaches the generated message, the `MessageHandler` impl, the
    /// dynamic registration and the typed-handle method - not just the method.
    #[cfg(any())]
    #[handler]
    fn gone(&mut self, missing: Missing) -> Missing {
        missing
    }

    /// The positive twin: gated in, fully wired.
    #[cfg(all())]
    #[handler]
    fn kept(&self) -> u32 {
        self.value
    }

    /// A gated-out event source: its `ActorEventSource` impl must vanish with
    /// it, or the derive's generated driver would route through nothing.
    #[cfg(any())]
    #[event_source]
    async fn drive(
        cx: factories::actor::event::EventContext<'_, Self>,
        mailbox: &mut (impl factories::spawn::ActorMailbox + Send),
    ) -> Option<factories::actor::dispatch::DispatchedActorMessage> {
        Missing::drive(cx, mailbox).await
    }

    /// A gated-out stop hook: the routing impl must vanish with it.
    #[cfg(any())]
    #[on_stop]
    async fn farewell(
        self,
        _reason: factories::actor::lifecycle::StopReason<'_, Self>,
        _cx: factories::actor::ActorContext<'_, Self>,
    ) {
        Missing::farewell()
    }
}

#[tokio::test]
async fn cfg_gated_handlers_come_and_go_with_their_gate() {
    let handle = ActorLauncher::default()
        .spawn_ready(&TokioTaskSpawner::current(), Gated { value: 1 })
        .await
        .expect("gated init is infallible");

    assert_eq!(handle.bump(2).await.expect("ask"), 3);
    assert_eq!(handle.kept().await.expect("ask"), 3);
}

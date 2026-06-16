//! Shared benchmark fixtures: an equivalent "counter" actor implemented in
//! `factories` and in `kameo`, plus spawn helpers.
//!
//! Both frameworks are exercised through their *default* configuration so the
//! comparison is out-of-the-box-vs-out-of-the-box:
//!
//! - factories: `TokioMpscActorChannel` (tokio mpsc, bounded 64) +
//!   `ConcurrentRunLoop` + `TokioMutexLock`, the `#[derive(Actor)]` defaults.
//! - kameo: `mailbox::bounded(64)` (its `DEFAULT_MAILBOX_CAPACITY`).
//!
//! The handlers do near-zero work (a wrapping increment / a field read) so the
//! measurement is dominated by dispatch + mailbox + loop overhead, not the
//! handler body.

/// `factories` fixtures.
pub mod fac {
    use factories::actor::Actor;
    use factories::runtime::tokio::TokioTaskSpawner;
    use factories::spawn::ActorLauncher;

    /// Concurrent-loop counter - the `#[derive(Actor)]` defaults.
    #[derive(Actor)]
    pub struct Counter {
        pub value: u64,
    }

    /// Spawn a counter on the current Tokio runtime. Must be called from within
    /// a runtime context (it resolves `TokioTaskSpawner::current()`).
    pub async fn spawn(start: u64) -> CounterHandle {
        ActorLauncher::default()
            .spawn_ready(&TokioTaskSpawner::current(), Counter { value: start })
            .await
            .expect("counter init is infallible")
    }

    #[factories::messages]
    impl Counter {
        /// Fire-and-forget increment (`Inc` message, answer `()`).
        #[handler]
        pub fn inc(&mut self) {
            self.value = self.value.wrapping_add(1);
        }

        /// Increment carrying a large (256-byte) payload - too big for the
        /// envelope's inline storage, so factories must box it too. Used to test
        /// whether the inline-payload advantage is what makes factories faster.
        #[handler]
        pub fn inc_big(&mut self, _payload: [u64; 32]) {
            self.value = self.value.wrapping_add(1);
        }

        /// Request/response read (`Get` message, answer `u64`). Takes `&mut
        /// self` (exclusive) to mirror kameo, whose handlers are all `&mut`.
        #[handler]
        pub async fn get(&mut self) -> u64 {
            self.value
        }
    }
}

/// `factories` fixtures using the sequential loop + lock-eliding `UnguardedLock`
/// (the best-case single-threaded path). Lives in its own module so the
/// generated `Inc`/`Get` message types do not collide with [`fac`].
pub mod fac_seq {
    use factories::actor::Actor;
    use factories::runtime::lock::UnguardedLock;
    use factories::runtime::sequential_loop::SequentialRunLoop;
    use factories::runtime::tokio::TokioTaskSpawner;
    use factories::spawn::ActorLauncher;

    #[derive(Actor)]
    #[actor(run_loop = SequentialRunLoop<Self>, lock = UnguardedLock<Self>)]
    pub struct Counter {
        pub value: u64,
    }

    /// Spawn a sequential-loop counter on the current Tokio runtime.
    pub async fn spawn(start: u64) -> CounterHandle {
        ActorLauncher::default()
            .spawn_ready(&TokioTaskSpawner::current(), Counter { value: start })
            .await
            .expect("counter init is infallible")
    }

    #[factories::messages]
    impl Counter {
        #[handler]
        pub fn inc(&mut self) {
            self.value = self.value.wrapping_add(1);
        }

        #[handler]
        pub async fn get(&self) -> u64 {
            self.value
        }
    }
}

/// `kameo` fixtures.
pub mod kam {
    use kameo::Actor;
    use kameo::actor::{ActorRef, Spawn};
    use kameo::message::{Context, Message};

    #[derive(Actor)]
    pub struct Counter {
        pub value: u64,
    }

    /// Spawn a counter with kameo's default bounded(64) mailbox.
    pub fn spawn(start: u64) -> ActorRef<Counter> {
        Counter::spawn(Counter { value: start })
    }

    /// Fire-and-forget increment.
    pub struct Inc;

    impl Message<Inc> for Counter {
        type Reply = ();

        async fn handle(&mut self, _: Inc, _: &mut Context<Self, Self::Reply>) -> Self::Reply {
            self.value = self.value.wrapping_add(1);
        }
    }

    /// Request/response read.
    pub struct Get;

    impl Message<Get> for Counter {
        type Reply = u64;

        async fn handle(&mut self, _: Get, _: &mut Context<Self, Self::Reply>) -> Self::Reply {
            self.value
        }
    }

    /// Increment carrying a large (256-byte) payload - kameo already boxes every
    /// message, so this only changes the box's size for kameo.
    pub struct IncBig(pub [u64; 32]);

    impl Message<IncBig> for Counter {
        type Reply = ();

        async fn handle(&mut self, _: IncBig, _: &mut Context<Self, Self::Reply>) -> Self::Reply {
            self.value = self.value.wrapping_add(1);
        }
    }
}

/// Build the runtime every benchmark runs on: a fixed-size multi-threaded Tokio
/// runtime so all contenders are measured on identical executor config.
pub fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("failed to build benchmark runtime")
}

//! Injected per-actor context (`Extension` / `ExtensionSet`): a value set at
//! spawn is readable in handlers via `cx.extensions()`, and *inheritable* values
//! flow to a child seeded with `inherit_from(parent.extensions())` while *local*
//! ones do not.

use factories::actor::{Actor, ActorContext};
use factories::declare_extension;
use factories::runtime::lock::UnguardedLock;
use factories::runtime::sequential_loop::SequentialRunLoop;
use factories::runtime::tokio::TokioTaskSpawner;
use factories::spawn::ActorLauncher;

declare_extension!(GREETING: &'static str, local);

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Reader;

#[factories::messages]
impl Reader {
    #[handler]
    async fn greeting(&self, #[context] cx: ActorContext<'_, Self>) -> Option<&'static str> {
        cx.extensions().get(GREETING).copied()
    }
}

#[tokio::test]
async fn handler_reads_an_injected_extension() {
    let spawner = TokioTaskSpawner::current();
    let handle = ActorLauncher::default()
        .with_extension(GREETING, "hello")
        .spawn_ready(&spawner, Reader)
        .await
        .expect("infallible init");

    assert_eq!(handle.greeting().await.expect("ask"), Some("hello"));
}

#[tokio::test]
async fn a_missing_extension_reads_as_none() {
    let spawner = TokioTaskSpawner::current();
    let handle = ActorLauncher::default()
        .spawn_ready(&spawner, Reader)
        .await
        .expect("infallible init");

    assert_eq!(handle.greeting().await.expect("ask"), None);
}

// -- inheritance ---------------------------------------------------------------

declare_extension!(SHARED_TAG: u64, inheritable);
declare_extension!(LOCAL_TAG: u64, local);

#[derive(Actor)]
#[actor(lock = UnguardedLock<Self>, run_loop = SequentialRunLoop<Self>)]
struct Tagged;

#[factories::messages]
impl Tagged {
    #[handler]
    async fn tags(&self, #[context] cx: ActorContext<'_, Self>) -> (Option<u64>, Option<u64>) {
        (
            cx.extensions().get(SHARED_TAG).copied(),
            cx.extensions().get(LOCAL_TAG).copied(),
        )
    }
}

#[tokio::test]
async fn inheritable_extensions_flow_to_a_child_but_locals_do_not() {
    let spawner = TokioTaskSpawner::current();

    let parent = ActorLauncher::default()
        .with_extension(SHARED_TAG, 7)
        .with_extension(LOCAL_TAG, 9)
        .spawn_ready(&spawner, Tagged)
        .await
        .expect("infallible init");

    // A child seeded from the parent's set - exactly what `ctx.launcher()` sugar
    // would expand to (`inherit_from(self.extensions())`).
    let child = ActorLauncher::default()
        .inherit_from(parent.state().extensions())
        .spawn_ready(&spawner, Tagged)
        .await
        .expect("infallible init");

    assert_eq!(
        parent.tags().await.expect("ask"),
        (Some(7), Some(9)),
        "the actor it was set on sees both",
    );
    assert_eq!(
        child.tags().await.expect("ask"),
        (Some(7), None),
        "the child inherits only the inheritable one",
    );
}

#[tokio::test]
async fn an_explicit_extension_overrides_an_inherited_one_either_order() {
    let spawner = TokioTaskSpawner::current();

    let parent = ActorLauncher::default()
        .with_extension(SHARED_TAG, 7)
        .spawn_ready(&spawner, Tagged)
        .await
        .expect("infallible init");

    // Inherit, then override.
    let inherit_then_set = ActorLauncher::default()
        .inherit_from(parent.state().extensions())
        .with_extension(SHARED_TAG, 99)
        .spawn_ready(&spawner, Tagged)
        .await
        .expect("infallible init");

    // Override, then inherit - the explicit value still wins (order-independent).
    let set_then_inherit = ActorLauncher::default()
        .with_extension(SHARED_TAG, 99)
        .inherit_from(parent.state().extensions())
        .spawn_ready(&spawner, Tagged)
        .await
        .expect("infallible init");

    assert_eq!(
        inherit_then_set.tags().await.expect("ask").0,
        Some(99),
        "explicit overrides inherited (inherit then set)",
    );
    assert_eq!(
        set_then_inherit.tags().await.expect("ask").0,
        Some(99),
        "explicit overrides inherited (set then inherit)",
    );
}

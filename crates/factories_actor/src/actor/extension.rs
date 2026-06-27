//! The pointer-keyed extension system, re-exported.
//!
//! It now lives in its own crate, [`factories_extension`], so consumers that
//! want the mechanism without the actor framework (e.g. the capture config) can
//! depend on just that. Re-exported here so `crate::actor::extension::*` paths
//! — and downstream `factories::actor::extension` — keep working unchanged.
//! `declare_extension!` is re-exported at the `factories_actor` crate root.

pub use factories_extension::{Extension, ExtensionSet};

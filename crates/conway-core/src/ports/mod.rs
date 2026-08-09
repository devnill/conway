//! Port traits: the binding contract every implementation crate compiles
//! against.
//!
//! These signatures are load-bearing (architecture §4). No default method on
//! any port may perform I/O. The implementations `conway-core` is permitted
//! to contain are the feature-gated test fakes (`feature = "fakes"`,
//! WI-008) plus two narrowly-scoped production fallbacks in
//! `crate::routing` (`MinimalRouter`, `AlwaysClosedHealthRegistry`; board
//! item 01KZFC1KNGQ51TZ0BG7P7RAY9H) that back `Conway::explain_routing`'s
//! honest degenerate answer when a caller supplies its own `Router`
//! (`ConwayBuilder::with_router`) and there is no `conway-routing`
//! `DeclarativeRouter` left to project an `ExplainReport` through. Every
//! other implementation lives in a dedicated crate.

mod artifact;
mod backend;
mod capability_index;
mod events;
mod permission;
mod plugin;
mod routing;
mod session;
mod subagent;

pub use artifact::*;
pub use backend::*;
pub use capability_index::*;
pub use events::*;
pub use permission::*;
pub use plugin::*;
pub use routing::*;
pub use session::*;
pub use subagent::*;

#[cfg(test)]
mod tests {
    use super::*;

    /// Mechanical proof that every port is object-safe: a function that only
    /// takes `&dyn Trait` parameters compiles if and only if every trait
    /// listed is dyn-compatible. This is the exact shape `RuntimeDeps` (in
    /// `conway-runtime`) needs to hold trait objects for each port.
    #[allow(dead_code, clippy::too_many_arguments)]
    fn _assert_object_safe(
        _: &dyn Backend,
        _: &dyn Plugin,
        _: &dyn Tool,
        _: &dyn PermissionGate,
        _: &dyn SessionStore,
        _: &dyn Router,
        _: &dyn HealthRegistry,
        _: &dyn SubagentHost,
        _: &dyn EventSink,
        _: &dyn RouterFactory,
        _: &dyn ArtifactWriter,
        _: &dyn BackendFactory,
    ) {
    }
}

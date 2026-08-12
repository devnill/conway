//! Port traits: the binding contract every implementation crate compiles
//! against.
//!
//! These signatures are load-bearing (architecture §4). No default method on
//! any port may perform I/O. The implementations `conway-core` is permitted
//! to contain fall into three kinds, and the distinction between the second
//! and third matters -- do not collapse them:
//!
//! 1. The feature-gated test fakes (`feature = "fakes"`, WI-008).
//! 2. PRODUCTION FALLBACKS that answer a real call: `crate::routing`'s
//!    `MinimalRouter`/`AlwaysClosedHealthRegistry` (board item
//!    01KZFC1KNGQ51TZ0BG7P7RAY9H), which back `Conway::explain_routing`'s
//!    honest degenerate answer when a caller supplies its own `Router`
//!    (`ConwayBuilder::with_router`) and there is no `conway-routing`
//!    `DeclarativeRouter` left to project an `ExplainReport` through. These
//!    are production code, not test doubles: real callers receive what they
//!    compute.
//! 3. ONE TEST-FIXTURE CONSTRUCTOR: `crate::ports::artifact`'s private
//!    `NoopArtifactWriter`, reachable only through
//!    [`ArtifactWriteHandle::noop`] (board item 01KZJ5S3ZC8SPWTX94C4HTEC2R),
//!    which lets a `ContextHookCtx` be built for a test without implementing
//!    `ArtifactWriter` by hand. It backs NO production call path --
//!    `conway-runtime`'s `agent_loop` always supplies the real
//!    `AgentArtifactWriter` -- so it is NOT a production fallback in sense 2,
//!    and its presence is not precedent for adding another.
//!
//! Kinds 2 and 3 are alike in exactly one respect: both are unconditionally
//! available rather than gated behind `feature = "fakes"`, and neither
//! performs I/O. Kind 3 is unconditional for a specific reason rather than a
//! general one -- `conway`'s `[dependencies]` never enables `fakes` (only its
//! `[dev-dependencies]` do), so a gated constructor would be unreachable by
//! the third-party author it exists to serve (GP-03/P-6). Every other
//! implementation lives in a dedicated crate.

mod artifact;
mod backend;
mod capability_index;
mod events;
mod hook_runner;
mod permission;
mod plugin;
mod routing;
mod session;
mod subagent;

pub use artifact::*;
pub use backend::*;
pub use capability_index::*;
pub use events::*;
pub use hook_runner::*;
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
        _: &dyn HookRunner,
    ) {
    }
}

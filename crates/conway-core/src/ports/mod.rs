//! Port traits: the binding contract every implementation crate compiles
//! against.
//!
//! These signatures are load-bearing (architecture §4). No default method on
//! any port may perform I/O. The implementations `conway-core` is permitted
//! to contain fall into two kinds:
//!
//! 1. PRODUCTION FALLBACKS that answer a real call: `crate::routing`'s
//!    `MinimalRouter`/`AlwaysClosedHealthRegistry` , which back `Conway::explain_routing`'s
//!    honest degenerate answer when a caller supplies its own `Router`
//!    (`ConwayBuilder::with_router`) and there is no `conway-routing`
//!    `DeclarativeRouter` left to project an `ExplainReport` through. These
//!    are production code, not test doubles: real callers receive what they
//!    compute.
//! 2. TEST-FIXTURE CONSTRUCTORS: `crate::ports::artifact`'s private
//!    `NoopArtifactWriter`, reachable only through
//!    [`ArtifactWriteHandle::noop`],
//!    which lets a `ContextHookCtx` be built for a test without implementing
//!    `ArtifactWriter` by hand; and [`ToolCtx::for_test`], which lets a
//!    `ToolCtx` be built for a test without implementing `SubagentHost` or
//!    `EventSink` by hand. Neither backs a production call path --
//!    `conway-runtime`'s `agent_loop` always supplies the real
//!    `AgentArtifactWriter`, and `conway_runtime::tools::runner` always
//!    builds every `ToolCtx` field itself -- so neither is a production
//!    fallback in sense 1.
//!
//!    `ArtifactWriteHandle::noop`'s own doc originally said its presence was
//!    "not precedent for adding another" -- narrower than intended: what it
//!    ruled out was a THIRD kind here (a scriptable double set living in
//!    this crate, gated or not -- that gap is `conway-testkit`'s job, never
//!    this crate's own), not a second constructor of this SAME kind for a
//!    different struct. `ToolCtx::for_test` (board item
//!    01KZQ3AZWG3NNJNZEJFX21MDJT) is that second instance, evaluated on its
//!    own merits rather than assumed from this comment: `ToolCtx` has two
//!    trait-object fields needing one, `ContextHookCtx` had one, and the
//!    reachability argument below applies to both identically. It is NOT a
//!    silent no-op default the way `ArtifactWriteHandle::noop` is --
//!    unlike a hook fixture that rarely writes, a `Tool::invoke` fixture
//!    usually wants to observe a started subagent or an emitted event, so
//!    it takes concrete doubles as required parameters instead of
//!    defaulting them; see that constructor's own doc for the full
//!    reasoning. A third, unrelated struct reaching for this same shape
//!    still needs its own justification -- this paragraph is not a general
//!    license, only the correction of an overclaim.
//!
//! All of kind 2 is unconditionally available, and none of it performs I/O.
//! It is unconditional for a specific reason: a feature-gated constructor
//! living in THIS crate would be reachable only from inside this workspace
//! (this crate has no facade sitting in front of it to forward a feature
//! through), so gating it would have reproduced the exact reachability gap
//! the full test-double set used to have -- a built-in gets no privileged
//! API. That full set (`FakeBackend`, `FakeStore`, `FakeSubagentHost`, ...)
//! used to be a third kind here, gated behind `feature = "fakes"` on this
//! crate; it now lives in `conway-testkit`, a crate of its own that depends
//! on this one (never the reverse -- T1) and that `conway`'s facade
//! forwards to third parties behind its own `testkit` feature.
//! Every other implementation lives in a dedicated crate.

mod artifact;
mod backend;
mod capability_index;
mod events;
mod hook_runner;
mod observer;
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
pub use observer::*;
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
        _: &dyn ToolObserver,
    ) {
    }
}

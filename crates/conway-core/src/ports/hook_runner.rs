//! The `HookRunner` port, a settled design decision.

use async_trait::async_trait;

use crate::error::HookFailure;
use crate::hook::{HookAnswer, HookInvocation};

/// Invokes one hook and reports its outcome.
///
/// **A PORT, not a concrete type.** A production implementation performs
/// I/O -- spawning a process, at minimum -- and `conway-core` performs
/// none, so this trait lives here while every implementation lives outside
/// it. Today's implementation (`conway_tools::hook_runner::
/// ProcessHookRunner`) spawns the
/// configured command fresh per event, writes the payload to stdin as
/// JSON, and reads the answer from stdout plus exit status -- but nothing
/// about this trait's signature commits to that modality; a future
/// implementation reached over the long-lived plugin transport instead
/// would satisfy the identical contract.
///
/// **WIRED: [`HookRunner::run`] has
/// a real call site.** `conway_runtime::permission::PermissionBroker::
/// decide` invokes it once per enabled `pre_tool_use` hook, at the SAME
/// tier as its `deny`-pattern check -- before the mode gate, the cache,
/// pattern-allow grants, and `AutoAllow` -- so a denying hook is enforced
/// under every permission mode, `AutoAllow` included (see `decide`'s own
/// doc for the full placement rationale). `PermissionBroker` reads the
/// answer's `permission` field
/// ([`crate::hook::HookAnswer::permission`]/[`crate::hook::
/// HookPermissionVerdict`]), a type with no `Allow` variant at all: a hook
/// may only narrow a permission verdict (deny it, or say nothing), never
/// widen one.
///
/// The seam is exactly what this doc used to promise, now real:
/// `ConwayBuilder::with_hook_runner` injects an `Arc<dyn HookRunner>` on
/// the identical surface `with_permission_gate`/`with_context_hook` already
/// use -- a built-in gets no privileged API -- `conway`'s `plugin`
/// extension-surface module re-exports
/// this trait (and the domain types its signature names) so a third party
/// can implement it without depending on `conway-core` directly, and
/// `conway-runtime` reaches this port ONLY through `conway_core::ports` --
/// never through `conway-tools`: the
/// runner arrives as an already-constructed `Arc<dyn HookRunner>` handed in
/// by the facade, a sibling crate's concern.
///
/// **Not called at all (no `with_hook_runner`) is still the default, and
/// is still a true no-op** -- `PermissionBroker`'s own `hook_runner` field
/// defaults to `None`, and its hook-check step short-circuits on that
/// before it ever reads an installed `[hooks].rules[]` entry or performs
/// any I/O. A `pre_tool_use` rule declared in config with no runner ever
/// injected parses, validates, and is silently never consulted -- see
/// `conway::config::schema::HooksConfig`'s own doc for that precise
/// disclosure.
///
/// **What is still NOT wired:** every event OTHER than `pre_tool_use`
/// remains exactly the forward declaration it always was -- this item
/// dispatches one event through this port, not the whole `[hooks]`
/// section. A later item wiring a second event reuses this same port and
/// this same fail-closed contract, adding only its own dispatch call site.
///
/// **Fail-closed, uniformly, at every invocation** -- not merely at
/// config-load time. A nonzero exit, a timeout, a missing/unexecutable
/// command, or unparseable stdout ALL become `Err(HookFailure)`; never a
/// panic, and never a silent `Ok` standing in for "nothing happened." See
/// [`HookFailure`]'s own doc for the full enumeration.
///
/// No default method: any default here would have to either perform I/O
/// (forbidden -- this crate does none, see `crate::agent`'s own module
/// doc) or fabricate an answer, and an "always succeeds" fabrication is
/// exactly the fail-open trap this port exists to close off.
#[async_trait]
pub trait HookRunner: Send + Sync + 'static {
    async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure>;
}

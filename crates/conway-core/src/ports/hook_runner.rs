//! The `HookRunner` port (board item 01KZRZY1MNM872BZ6AKEBG3SKE, decision
//! 01KZT642CEZ20K92DYWBTPE2XZ).

use async_trait::async_trait;

use crate::error::HookFailure;
use crate::hook::{HookAnswer, HookInvocation};

/// Invokes one hook and reports its outcome.
///
/// **A PORT, not a concrete type.** A production implementation performs
/// I/O -- spawning a process, at minimum -- and `conway-core` performs
/// none, so this trait lives here while every implementation lives outside
/// it. Today's implementation (`conway_tools::hook_runner::
/// ProcessHookRunner`, decision 01KZRZBQ2ACF40QGK8E9AVGMT3) spawns the
/// configured command fresh per event, writes the payload to stdin as
/// JSON, and reads the answer from stdout plus exit status -- but nothing
/// about this trait's signature commits to that modality; a future
/// implementation reached over the long-lived plugin transport instead
/// would satisfy the identical contract.
///
/// **NOT YET WIRED — read this before assuming an injection point exists.**
/// `ConwayBuilder` has no `with_hook_runner` method today and `conway`'s
/// extension-surface module does not re-export this trait. Nothing calls
/// [`HookRunner::run`] anywhere in the tree. The port exists so the
/// implementation has a contract to satisfy and so the shape is settled
/// before a consumer arrives; the injection point lands with the first
/// consumer, board item 01KZS00JP5QNBJSSHNFP9C47GM (`pre_tool_use` wired
/// into the permission decision), which is what makes it reachable.
///
/// The DESTINATION is the seam `PermissionGate`/`ContextHook` already use —
/// an `Arc<dyn HookRunner>` injected through `ConwayBuilder`, so a third
/// party supplies a runner on the identical surface a built-in uses
/// (GP-03/P-6), and `conway-runtime` never depends on `conway-tools` to
/// reach one (decision 01KZT642CEZ20K92DYWBTPE2XZ). That is where this is
/// going, not where it is.
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

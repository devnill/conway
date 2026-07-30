//! The `SubagentHost` port: the cycle-breaker (architecture §4.6).
//!
//! The developer API (`SessionHandle::fork`/`spawn`) and the
//! `conway_subagent` tool both call this same trait (decision 2, mechanically
//! enforced): the tool is a thin wrapper with no privileged access.
//!
//! ## `caller` on `steer`/`await_result`/`cancel` (board item
//! 01KYT8TS0EBKJHYNJRF6S88NRH)
//!
//! Every implementation MUST enforce that `caller` may act on `target` only
//! when `target` is within `caller`'s own subtree (itself, or any
//! descendant reachable by walking `parent` links) -- the fix for "any
//! agent can steer/await/cancel any other agent" (previously, these three
//! methods took only `target`, with nothing checking who was asking).
//! Violating this MUST return [`RuntimeError::AgentNotInSubtree`], never a
//! panic (P-10: both ids may be model-supplied) -- an unknown `target`
//! stays [`RuntimeError::AgentNotFound`], distinct from a known-but-foreign
//! one. This mirrors [`RuntimeError::AskRequiresFork`]'s shape: enforced at
//! THIS trait boundary (P-1), not only at the `conway_steer`/
//! `conway_await`/`conway_cancel` tool callsites, so no other caller --
//! including a future out-of-process plugin -- can bypass it.
//!
//! **Root/operator exemption, stated explicitly:** there is no separate
//! "operator" flag or bypass anywhere in this trait. An operator/embedder
//! call is simply one whose `caller` is the session's own root agent
//! (`conway::SessionHandle`'s `steer`/`await_agent`/`cancel` always pass
//! `self.root` as `caller`, having already confirmed `target` belongs to
//! that same session via `ensure_agent_in_session`) -- and a root's own
//! subtree covers its entire session by construction, so a root-originated
//! call satisfies the very same check every other caller is held to,
//! without needing a bypass. A MODEL-invoked `conway_steer`/`conway_await`/
//! `conway_cancel` tool call always passes `ToolCtx::agent_id` (the
//! runtime-assigned identity of the actual invoking agent, never
//! model-supplied -- see `conway-tools`' `subagent/control.rs`) as
//! `caller`, so a model can never forge a wider `caller` than its own true
//! identity.

use async_trait::async_trait;

use crate::agent::{AgentResult, AgentTreeSnapshot, AskOutcome, SubagentSpec};
use crate::error::RuntimeError;
use crate::ids::AgentId;

#[async_trait]
pub trait SubagentHost: Send + Sync + 'static {
    async fn start(&self, parent: AgentId, spec: SubagentSpec) -> Result<AgentId, RuntimeError>;

    /// `text`'s attribution (`AgentMessage::Steer::from`) MUST derive from
    /// `caller` -- deriving it from `target`'s own tree parent (the
    /// pre-fix behavior) is what let a forged steer look authentic to its
    /// recipient, since a steer lands with parent authority by convention.
    async fn steer(
        &self,
        caller: AgentId,
        target: AgentId,
        text: String,
    ) -> Result<(), RuntimeError>;

    /// Always terminates: the supervisor synthesizes a result on budget
    /// exhaustion, cancellation, or task panic. A parent's pending
    /// `conway_subagent` tool call can never hang on this call.
    async fn await_result(
        &self,
        caller: AgentId,
        target: AgentId,
    ) -> Result<AgentResult, RuntimeError>;

    async fn cancel(
        &self,
        caller: AgentId,
        target: AgentId,
        reason: String,
    ) -> Result<(), RuntimeError>;

    fn tree(&self) -> AgentTreeSnapshot;

    /// Runs `spec` (fork-only by convention) as an ephemeral child of
    /// `parent` and returns the child's FULL concatenated `TextDelta` reply
    /// in [`AskOutcome::text`] -- NOT [`AgentResult::summary`], which
    /// truncates at `DEFAULT_SUMMARY_LIMIT = 2000` chars.
    ///
    /// Implementations MUST subscribe to the `EventBus` BEFORE
    /// `launch_agent` so the first `TextDelta` is not missed, and MUST
    /// agent-id-check the child's `AgentFinished` so a sibling finishing
    /// does not resolve the drain. `ask` is fork+await-text, NOT a third
    /// subagent primitive (P-1): no mode parameter.
    async fn ask(&self, parent: AgentId, spec: SubagentSpec) -> Result<AskOutcome, RuntimeError>;
}

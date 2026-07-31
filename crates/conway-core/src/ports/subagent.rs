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
//!
//! ## `caller` on `start`/`ask`/`tree` (board item 01KYTP0PGKJ4VCJP5TD39A1WHF)
//!
//! `674bb65` (the item above) closed `steer`/`await_result`/`cancel` but
//! left `start`, `ask`, and `tree` unguarded: `start`/`ask` took only
//! `parent` and acted on it directly, with nothing checking that the caller
//! was entitled to attach a child there, and `tree` took no caller at all
//! and returned the WHOLE runtime-wide tree to anyone holding a
//! `ToolCtx::subagents` handle -- i.e. every tool. Composed, this was
//! cross-tree exfiltration in one call: `tree()` to discover a sibling's
//! `AgentId` (an ordinary, unprivileged read every tool already had), then
//! `ask(sibling, SubagentSpec { mode: Fork, .. })` to fork that sibling's
//! ENTIRE context (GP-02: a fork inherits everything up to the fork point)
//! and read the reply back as plain model output.
//!
//! This item closes all three with the SAME mechanism `674bb65` already
//! established, not a second one (P-1, GP-02 -- no third subagent
//! primitive, no bypass flag):
//!
//! - `start`/`ask` gain a `caller: AgentId` parameter, checked against
//!   `parent` with the identical `ensure_own_subtree` rule `steer`/
//!   `await_result`/`cancel` already use -- `parent` outside `caller`'s own
//!   subtree is [`RuntimeError::AgentNotInSubtree`], an unknown `parent` is
//!   [`RuntimeError::AgentNotFound`]. `ask` performs no separate check of
//!   its own: it composes `start` (P-1's own "ask is fork+await-text, not a
//!   third primitive" rule), so passing `caller` straight through to its
//!   internal `start(caller, parent, spec)` call is what enforces this for
//!   `ask` too -- exactly the same "one check, reused" shape `674bb65`
//!   itself used for the trio it fixed. A `caller` and `parent` that are
//!   the SAME agent (the ordinary case: an agent starting/asking a child of
//!   ITSELF) always passes trivially, since an agent's own subtree always
//!   contains itself.
//! - `tree` gains a `caller: AgentId` parameter and returns `caller`'s own
//!   subtree (itself, plus every descendant), never a foreign branch. For
//!   the session's root agent this is, correctly, the WHOLE tree -- the
//!   root's subtree IS the tree, by construction, same as the trio's
//!   root/operator exemption above.
//!
//! The SAME root/operator-exemption mechanism applies: `conway::
//! SessionHandle::fork`/`spawn` pass `self.root` as `caller` (having
//! already confirmed `parent` belongs to the session via
//! `ensure_agent_in_session`, so the check always succeeds for that path,
//! with no bypass needed), and the model-invoked `conway_subagent`/
//! `conway_ask` tools always pass `ToolCtx::agent_id` as BOTH `caller` and
//! `parent` (a tool call always starts/asks a child of the CALLING agent
//! itself -- there is no model-facing argument that names a different
//! parent, so nothing here removes any existing capability).

use async_trait::async_trait;

use crate::agent::{AgentResult, AgentTreeSnapshot, AskOutcome, SubagentSpec};
use crate::error::RuntimeError;
use crate::ids::AgentId;

#[async_trait]
pub trait SubagentHost: Send + Sync + 'static {
    /// `caller` must own `parent` (`parent` is `caller` itself, or a
    /// descendant of `caller` -- see this module's own doc, "`caller` on
    /// `start`/`ask`/`tree`"). A `parent` outside `caller`'s own subtree is
    /// [`RuntimeError::AgentNotInSubtree`]; an unknown `parent` is
    /// [`RuntimeError::AgentNotFound`]. Never a panic (P-10: both ids may be
    /// model-supplied).
    async fn start(
        &self,
        caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AgentId, RuntimeError>;

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

    /// `caller`'s own subtree (itself, plus every descendant) -- never a
    /// foreign branch. See this module's own doc, "`caller` on
    /// `start`/`ask`/`tree`". For the session's root agent this is the
    /// whole tree, correctly: the root's subtree IS the tree.
    fn tree(&self, caller: AgentId) -> AgentTreeSnapshot;

    /// Runs `spec` (fork-only by convention) as an ephemeral child of
    /// `parent` and returns the child's FULL concatenated `TextDelta` reply
    /// in [`AskOutcome::text`] -- NOT [`AgentResult::summary`], which
    /// truncates at `DEFAULT_SUMMARY_LIMIT = 2000` chars.
    ///
    /// Implementations MUST subscribe to the `EventBus` BEFORE
    /// `launch_agent` so the first `TextDelta` is not missed, and MUST
    /// agent-id-check the child's `AgentFinished` so a sibling finishing
    /// does not resolve the drain. `ask` is fork+await-text, NOT a third
    /// subagent primitive (P-1): no mode parameter. `caller` must own
    /// `parent`, exactly as `start` requires -- see this module's own doc.
    async fn ask(
        &self,
        caller: AgentId,
        parent: AgentId,
        spec: SubagentSpec,
    ) -> Result<AskOutcome, RuntimeError>;
}

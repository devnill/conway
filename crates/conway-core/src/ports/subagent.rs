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

use std::sync::Arc;

use async_trait::async_trait;

use crate::agent::{AgentResult, AgentTreeSnapshot, AskOutcome, SubagentSpec};
use crate::error::{RuntimeError, SubagentError};
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

/// A [`SubagentHost`] bound to ONE caller -- the `ToolCtx`-facing capability
/// a subagent-managing tool actually gets, in place of the raw
/// `Arc<dyn SubagentHost>` it used to hold (the same widening `cd`'s
/// [`crate::ports::CwdHandle`] did for the cwd capability: a concrete handle
/// on `ToolCtx`, not a host-tier trait object).
///
/// **Bakes the caller's own [`AgentId`] in -- structurally, not by
/// convention.** Every method below takes NO `caller` (or, for
/// [`Self::start`]/[`Self::ask`], `parent`) parameter at all: the id
/// supplied to [`Self::new`] is used for every one of them, always. This is
/// P-1 ("an agent may only act within its own subtree") made structural at
/// the tool surface rather than merely enforced by convention at every call
/// site: today, every `conway-tools` subagent tool call already passes
/// `ToolCtx::agent_id` as `caller` (and, for `start`/`ask`, also as
/// `parent` -- there is no tool argument that names a different one; see
/// `conway-tools`' `subagent/tools.rs` and `ask.rs`), so this handle does
/// not remove any capability a tool has today. What it removes is the
/// ABILITY TO EXPRESS the opposite: with the raw `Arc<dyn SubagentHost>`, a
/// future or third-party tool COULD pass a different `caller` (a bug, or a
/// deliberately malicious plugin); with this handle, there is no `caller`
/// parameter for such code to supply one to. `steer`/`await_result`/
/// `cancel`'s `target` -- legitimately model-supplied, since a tool call
/// must be able to name WHICH child it acts on -- stays a parameter; only
/// the identity of the ACTOR is fixed.
///
/// **Translates [`RuntimeError`] -> [`SubagentError`] at this boundary, and
/// only here (P-14: one implementation).** [`SubagentHost`]'s five fallible
/// methods return `RuntimeError` -- the host-tier taxonomy, `#[non_exhaustive]`
/// and shared with every other runtime concern (routing, backends, the
/// store, ...). A tool has no business matching on any of that; every
/// method here narrows to [`SubagentError`], the tool-facing taxonomy
/// [`crate::error::ToolError`] can map cleanly (`From<SubagentError> for
/// ToolError`, in `conway-core`'s `error` module) via a single per-variant
/// conversion, `map_err`'d once at the bottom of every method rather than
/// restated at every call site or, worse, inside every tool.
///
/// **Cheap-`Clone`, preserving [`crate::ports::ToolCtx`]'s clone contract**
/// (that type's own doc: "every field is an `Arc`, `Copy`, or otherwise
/// cheap to clone"): `host` is an `Arc` refcount bump, `agent_id` is `Copy`.
#[derive(Clone)]
pub struct SubagentHandle {
    host: Arc<dyn SubagentHost>,
    agent_id: AgentId,
}

impl std::fmt::Debug for SubagentHandle {
    // Manual impl: `Arc<dyn SubagentHost>` carries no `Debug` bound (mirrors
    // `ToolCtx`'s own manual `Debug`, which renders its `subagents` field
    // the same placeholder way today).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubagentHandle")
            .field("agent_id", &self.agent_id)
            .field("host", &"<dyn SubagentHost>")
            .finish()
    }
}

impl SubagentHandle {
    /// Wraps `host`, baking `agent_id` in as the one caller identity (and,
    /// for `start`/`ask`, parent) every method below uses -- see this
    /// type's own doc for why nothing here lets that identity be
    /// overridden per call.
    pub fn new(host: Arc<dyn SubagentHost>, agent_id: AgentId) -> Self {
        Self { host, agent_id }
    }

    /// Starts (forks or spawns, per `spec.mode`) a child of THIS handle's
    /// own agent -- there is no parameter to name a different parent (see
    /// this type's own doc); [`SubagentError::NotInSubtree`] can therefore
    /// never surface from this call (the handle's own agent is always
    /// within its own subtree), but [`SubagentError::UnknownAgent`] and
    /// [`SubagentError::Host`] both still can (a stale/foreign `agent_id`
    /// this handle was constructed with, or infrastructure failure).
    pub async fn start(&self, spec: SubagentSpec) -> Result<AgentId, SubagentError> {
        self.host
            .start(self.agent_id, self.agent_id, spec)
            .await
            .map_err(translate)
    }

    /// Sends `text` to `target` as a steer message. `target` is the only
    /// model-supplied identity here (a tool call must be able to name WHICH
    /// child it steers); the caller sending it is always this handle's own
    /// agent.
    pub async fn steer(&self, target: AgentId, text: String) -> Result<(), SubagentError> {
        self.host
            .steer(self.agent_id, target, text)
            .await
            .map_err(translate)
    }

    /// Blocks for `target`'s terminal result. Always terminates (see
    /// [`SubagentHost::await_result`]'s own doc); a pending call can never
    /// hang on this handle either.
    pub async fn await_result(&self, target: AgentId) -> Result<AgentResult, SubagentError> {
        self.host
            .await_result(self.agent_id, target)
            .await
            .map_err(translate)
    }

    /// Cancels `target` with `reason`.
    pub async fn cancel(&self, target: AgentId, reason: String) -> Result<(), SubagentError> {
        self.host
            .cancel(self.agent_id, target, reason)
            .await
            .map_err(translate)
    }

    /// This handle's own agent's subtree (itself, plus every descendant) --
    /// never a foreign branch. Infallible, mirroring
    /// [`SubagentHost::tree`]'s own signature exactly.
    pub fn tree(&self) -> AgentTreeSnapshot {
        self.host.tree(self.agent_id)
    }

    /// Runs `spec` (fork-only by convention) as an ephemeral child of THIS
    /// handle's own agent, returning the child's full reply text. See
    /// [`Self::start`]'s own doc for why there is no separate `parent`
    /// parameter to name a different one.
    pub async fn ask(&self, spec: SubagentSpec) -> Result<AskOutcome, SubagentError> {
        self.host
            .ask(self.agent_id, self.agent_id, spec)
            .await
            .map_err(translate)
    }
}

/// The ONE place a [`RuntimeError`] a wrapped [`SubagentHost`] call returns
/// becomes a [`SubagentError`] (P-14) -- every [`SubagentHandle`] method
/// funnels through this, rather than restating the mapping per method.
///
/// Deliberately exhaustive over `RuntimeError`'s current variant set, with
/// no wildcard arm: `RuntimeError` is `#[non_exhaustive]` only to crates
/// OTHER than this one (its own definition lives in this crate's `error`
/// module), so a future variant added there fails THIS match at compile
/// time, forcing a deliberate decision about its `SubagentError` mapping
/// rather than letting it silently fall into [`SubagentError::Host`].
fn translate(err: RuntimeError) -> SubagentError {
    let rendered = err.to_string();
    match err {
        RuntimeError::AgentNotFound { agent } => SubagentError::UnknownAgent { agent },
        RuntimeError::AgentNotInSubtree { caller, target } => {
            SubagentError::NotInSubtree { caller, target }
        }
        RuntimeError::AskRequiresFork { mode } => SubagentError::AskRequiresFork { mode },
        RuntimeError::AgentNotInSession { .. }
        | RuntimeError::AgentNotLive { .. }
        | RuntimeError::BudgetExceeded { .. }
        | RuntimeError::Cancelled { .. }
        | RuntimeError::Backend(_)
        | RuntimeError::Routing(_)
        | RuntimeError::Store(_)
        | RuntimeError::Tool(_)
        | RuntimeError::ForkContextOverflow { .. } => SubagentError::Host { detail: rendered },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use chrono::Utc;

    use super::*;
    use crate::agent::{Budget, ResultStatus};
    use crate::content::Usage;
    use crate::ids::SessionId;

    /// A `SubagentHost` double that records the exact `(caller, target)` (or
    /// `(caller, parent)`) it was called with and, when configured via
    /// [`Self::with_error`], fails every fallible call with a scripted
    /// `RuntimeError` -- enough to drive [`SubagentHandle`] through every
    /// [`translate`] arm and to prove structurally that `caller`/`parent`
    /// are always whatever [`SubagentHandle::new`] baked in, never anything
    /// a call site supplied (there is no such parameter to supply).
    #[derive(Default)]
    struct RecordingHost {
        error: Option<RuntimeError>,
        /// `(caller, parent_or_target)` from the most recent call, per
        /// method.
        last_start: Mutex<Option<(AgentId, AgentId)>>,
        last_steer: Mutex<Option<(AgentId, AgentId)>>,
        last_await: Mutex<Option<(AgentId, AgentId)>>,
        last_cancel: Mutex<Option<(AgentId, AgentId)>>,
        last_tree: Mutex<Option<AgentId>>,
        last_ask: Mutex<Option<(AgentId, AgentId)>>,
    }

    impl RecordingHost {
        fn with_error(error: RuntimeError) -> Self {
            Self {
                error: Some(error),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl SubagentHost for RecordingHost {
        async fn start(
            &self,
            caller: AgentId,
            parent: AgentId,
            _spec: SubagentSpec,
        ) -> Result<AgentId, RuntimeError> {
            *self.last_start.lock().unwrap() = Some((caller, parent));
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(AgentId::new()),
            }
        }

        async fn steer(
            &self,
            caller: AgentId,
            target: AgentId,
            _text: String,
        ) -> Result<(), RuntimeError> {
            *self.last_steer.lock().unwrap() = Some((caller, target));
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        async fn await_result(
            &self,
            caller: AgentId,
            target: AgentId,
        ) -> Result<AgentResult, RuntimeError> {
            *self.last_await.lock().unwrap() = Some((caller, target));
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(AgentResult::new(
                    target,
                    SessionId::new(),
                    ResultStatus::Completed,
                    "ok",
                )),
            }
        }

        async fn cancel(
            &self,
            caller: AgentId,
            target: AgentId,
            _reason: String,
        ) -> Result<(), RuntimeError> {
            *self.last_cancel.lock().unwrap() = Some((caller, target));
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            }
        }

        fn tree(&self, caller: AgentId) -> AgentTreeSnapshot {
            *self.last_tree.lock().unwrap() = Some(caller);
            AgentTreeSnapshot {
                root: caller,
                nodes: Vec::new(),
                at: Utc::now(),
            }
        }

        async fn ask(
            &self,
            caller: AgentId,
            parent: AgentId,
            _spec: SubagentSpec,
        ) -> Result<AskOutcome, RuntimeError> {
            *self.last_ask.lock().unwrap() = Some((caller, parent));
            match &self.error {
                Some(err) => Err(err.clone()),
                None => Ok(AskOutcome {
                    text: "ok".into(),
                    usage: Usage::default(),
                    status: ResultStatus::Completed,
                    transcript_ref: SessionId::new(),
                }),
            }
        }
    }

    fn fork_spec() -> SubagentSpec {
        SubagentSpec::fork("do it", Budget::default())
    }

    /// Dependency-free async-test helper (`conway-core` has no `tokio`/
    /// `futures-executor` dev-dependency): every future exercised by this
    /// module's tests resolves on its first poll (`RecordingHost` never
    /// truly awaits), so a single poll with a no-op waker suffices. Mirrors
    /// `ports::plugin`'s own `block_on` test helper exactly.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    // ---- Structural guarantee: no caller/parent parameter exists ----

    /// `start`/`ask` always pass the handle's OWN agent id as both `caller`
    /// AND `parent` to the wrapped host -- and there is no argument on
    /// `SubagentHandle::start`/`ask` a caller could use to make it pass
    /// anything else. This test is a behavioral witness of that structural
    /// fact (the fact itself is that these methods' signatures, `start(&self,
    /// spec: SubagentSpec)` and `ask(&self, spec: SubagentSpec)`, have no
    /// `caller`/`parent` parameter at all -- checkable by reading the
    /// signatures above, not by a runtime assertion).
    #[test]
    fn start_and_ask_pass_the_handles_own_agent_id_as_both_caller_and_parent() {
        let agent_id = AgentId::new();
        let host = Arc::new(RecordingHost::default());
        let handle = SubagentHandle::new(host.clone(), agent_id);

        block_on(handle.start(fork_spec())).unwrap();
        assert_eq!(*host.last_start.lock().unwrap(), Some((agent_id, agent_id)));

        block_on(handle.ask(fork_spec())).unwrap();
        assert_eq!(*host.last_ask.lock().unwrap(), Some((agent_id, agent_id)));
    }

    /// `steer`/`await_result`/`cancel`/`tree` always pass the handle's own
    /// agent id as `caller`, regardless of what `target` a call names --
    /// again, there is no parameter through which a caller could supply a
    /// different `caller`.
    #[test]
    fn steer_await_cancel_and_tree_always_pass_the_handles_own_agent_id_as_caller() {
        let agent_id = AgentId::new();
        let target = AgentId::new();
        let host = Arc::new(RecordingHost::default());
        let handle = SubagentHandle::new(host.clone(), agent_id);

        block_on(handle.steer(target, "hi".into())).unwrap();
        assert_eq!(*host.last_steer.lock().unwrap(), Some((agent_id, target)));

        block_on(handle.await_result(target)).unwrap();
        assert_eq!(*host.last_await.lock().unwrap(), Some((agent_id, target)));

        block_on(handle.cancel(target, "stop".into())).unwrap();
        assert_eq!(*host.last_cancel.lock().unwrap(), Some((agent_id, target)));

        handle.tree();
        assert_eq!(*host.last_tree.lock().unwrap(), Some(agent_id));
    }

    /// A clone of a `SubagentHandle` shares the same underlying `host` `Arc`
    /// and carries the same baked-in `agent_id` -- the cheap-`Clone`
    /// contract `ToolCtx` relies on.
    #[test]
    fn clones_share_the_host_and_carry_the_same_agent_id() {
        let agent_id = AgentId::new();
        let target = AgentId::new();
        let host = Arc::new(RecordingHost::default());
        let handle = SubagentHandle::new(host.clone(), agent_id);
        let clone = handle.clone();

        block_on(clone.steer(target, "hi".into())).unwrap();
        assert_eq!(*host.last_steer.lock().unwrap(), Some((agent_id, target)));
    }

    // ---- translate: every RuntimeError variant this item's mapping names ----

    #[test]
    fn agent_not_found_translates_to_unknown_agent() {
        let agent_id = AgentId::new();
        let target = AgentId::new();
        let host = Arc::new(RecordingHost::with_error(RuntimeError::AgentNotFound {
            agent: target,
        }));
        let handle = SubagentHandle::new(host, agent_id);

        let err = block_on(handle.await_result(target)).unwrap_err();
        assert_eq!(err, SubagentError::UnknownAgent { agent: target });
    }

    #[test]
    fn agent_not_in_subtree_translates_to_not_in_subtree() {
        let agent_id = AgentId::new();
        let target = AgentId::new();
        let host = Arc::new(RecordingHost::with_error(RuntimeError::AgentNotInSubtree {
            caller: agent_id,
            target,
        }));
        let handle = SubagentHandle::new(host, agent_id);

        let err = block_on(handle.cancel(target, "x".into())).unwrap_err();
        assert_eq!(
            err,
            SubagentError::NotInSubtree {
                caller: agent_id,
                target
            }
        );
    }

    #[test]
    fn ask_requires_fork_translates_to_ask_requires_fork() {
        let agent_id = AgentId::new();
        let mode = crate::log::SubagentMode::Spawn;
        let host = Arc::new(RecordingHost::with_error(RuntimeError::AskRequiresFork {
            mode,
        }));
        let handle = SubagentHandle::new(host, agent_id);

        let err = block_on(handle.ask(fork_spec())).unwrap_err();
        assert_eq!(err, SubagentError::AskRequiresFork { mode });
    }

    /// Every other `RuntimeError` variant -- `Store`, the `Tool(Internal)`
    /// smuggle channel `conway-runtime` uses today, and every other
    /// variant this crate defines -- becomes `SubagentError::Host`,
    /// carrying the original `Display` string through as `detail`.
    #[test]
    fn every_other_runtime_error_variant_translates_to_host() {
        let agent_id = AgentId::new();
        let cases = vec![
            RuntimeError::Store(crate::error::StoreError::Io {
                detail: "disk full".into(),
            }),
            RuntimeError::Tool(crate::error::ToolError::Internal {
                detail: "invalid SubagentSpec".into(),
            }),
            RuntimeError::AgentNotInSession {
                agent: agent_id,
                session: SessionId::new(),
            },
            RuntimeError::AgentNotLive { agent: agent_id },
            RuntimeError::BudgetExceeded { agent: agent_id },
            RuntimeError::Cancelled {
                agent: agent_id,
                reason: "r".into(),
            },
        ];
        for runtime_err in cases {
            let rendered = runtime_err.to_string();
            let host = Arc::new(RecordingHost::with_error(runtime_err));
            let handle = SubagentHandle::new(host, agent_id);
            let err = block_on(handle.start(fork_spec())).unwrap_err();
            assert_eq!(err, SubagentError::Host { detail: rendered });
        }
    }

    /// `SubagentHandle` must stay object-safe-free / trivially usable as a
    /// plain value on `ToolCtx` (no `dyn` needed) -- proven by the fact
    /// every test above constructs and clones it directly.
    #[test]
    fn subagent_handle_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SubagentHandle>();
    }
}

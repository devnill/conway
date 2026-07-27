//! `AgentTree`: the multi-agent tree (WI-083, architecture §7).
//!
//! Owns attachment/lookup of every agent in a runtime's tree(s), structural
//! `agent_path` resolution (§4.3's `PermissionRequest` precondition),
//! cancellation propagation, and the terminal-result publication guarantee
//! that makes `await_result` always resolve -- the supervisor
//! ([`crate::supervisor`]) is this guarantee's other half; this module owns
//! the state it publishes into.
//!
//! ## Reconciliations against the WI-083 spec's illustrative `AgentNode`
//!
//! The spec's implementation-notes sketch shows one `AgentNode` struct
//! carrying `result_tx: watch::Sender<Option<AgentResult>>` and a plain
//! `status: AgentStatus` field, constructed by the caller and handed
//! wholesale to `attach`. Two changes from that sketch:
//! - **`result_tx` is tree-owned, not caller-supplied.** `attach` creates
//!   the `watch::channel` itself and keeps the `Sender` in its own
//!   bookkeeping ([`TreeEntry`]), never handing it back out; callers get a
//!   result only through [`AgentTree::await_result`] or
//!   [`AgentTree::snapshot`]. This is what lets [`AgentTree::publish_result`]
//!   be the *only* place a result is ever written -- if the caller held the
//!   `Sender` too, nothing would stop a second writer from bypassing the
//!   set-once guarantee.
//! - **`status`/`steps_taken` are derived, not stored.** Both are fully
//!   determined by whether (and how) a result has been published, so
//!   storing them separately would just be a second, independently-mutable
//!   copy of the same fact -- a staleness bug waiting to happen. `snapshot`
//!   computes them from the watch channel's current value on every call
//!   (mirroring the WI-082 `tree()` stub this item supersedes, which did the
//!   same thing for the same reason).
//!
//! This crate's own [`AgentNode`] (this module's input to `attach`) is a
//! distinct type from [`conway_core::agent::AgentNode`] (the snapshot's flat
//! *output* projection, unchanged, pre-existing) -- the two are shaped for
//! opposite directions of the same data and share a name only because both
//! are naturally called "an agent node". `snapshot` builds the latter from
//! this module's bookkeeping; nothing else in this crate should need to name
//! the core type.
//!
//! `Event::AgentSpawned` requires a non-optional `SubagentMode` `kind`,
//! which has no variant for "root" -- a root agent is *started*, not
//! *spawned*. `attach` therefore emits `Event::AgentSpawned` only when
//! `node.kind.is_some()` (a real fork/spawn child), never for a root. This
//! also gives the architecture §8 "`AgentSpawned` precedes every other event
//! bearing that `agent_id`" guarantee for free: `attach` is synchronous and
//! always returns before its caller can spawn the agent's task (and thus
//! before anything else can be emitted under that id). WI-084's `subagent.rs`
//! (not yet implemented) should call `attach` and rely on this emission
//! rather than emitting its own `AgentSpawned` -- see this module's doc for
//! why a second emission site would double-fire the event.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use chrono::Utc;
use conway_core::agent::{
    AgentNode as CoreAgentNode, AgentResult, AgentStatus, AgentTreeSnapshot, Budget, ResultStatus,
    SubagentMode,
};
use conway_core::error::{RuntimeError, ToolError};
use conway_core::event::Event;
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SessionId};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::events::EventBus;

/// One agent's tree-membership descriptor, as supplied to
/// [`AgentTree::attach`]. See the module doc's reconciliation note for how
/// this differs from the spec's illustrative sketch and from
/// [`conway_core::agent::AgentNode`] (a same-named but differently-shaped
/// sibling type).
#[derive(Clone)]
pub struct AgentNode {
    pub id: AgentId,
    pub parent: Option<AgentId>,
    pub session: SessionId,
    /// `None` for a root agent; `Some(mode)` for a fork/spawn child. Drives
    /// whether `attach` emits `Event::AgentSpawned` (see the module doc).
    pub kind: Option<SubagentMode>,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub budget: Budget,
    /// Structural cancellation: a child's token must be
    /// `parent.cancel.child_token()` so [`AgentTree::cancel`] on an ancestor
    /// cancels the whole subtree without a manual walk. Deriving this
    /// correctly is the caller's responsibility -- `attach` does not
    /// validate it.
    pub cancel: CancellationToken,
    /// Forwarded verbatim into `Event::AgentSpawned::inherited_upto` when
    /// `kind.is_some()`; ignored (and should be `None`) for a root.
    pub inherited_upto: Option<LogSeq>,
    /// Whether this agent is an ephemeral `/ask`-style aside (decision
    /// 01KYD1TWXMZD4BT842CMJT1AED). Stamped verbatim into
    /// `Event::AgentSpawned::ephemeral` (when `kind.is_some()`) and read back
    /// by [`AgentTree::ephemeral_of`] for `Event::AgentFinished::ephemeral`.
    /// Defaults to `false` for every non-facade-`/ask` path: a root is never
    /// ephemeral, and a `conway_subagent` fork/spawn child is never ephemeral
    /// either (only `conway`'s facade-level `SessionHandle::ask` constructs a
    /// child `SessionMeta` with `ephemeral: true`).
    pub ephemeral: bool,
}

/// Tree-internal bookkeeping for one attached agent: the caller-supplied
/// descriptor plus the result-publication state `attach` creates for it.
struct TreeEntry {
    node: AgentNode,
    result_tx: watch::Sender<Option<AgentResult>>,
    /// `tokio::sync::watch::Sender::send` silently discards the value (and
    /// returns `Err`) when the channel has zero live receivers -- it does
    /// NOT store the value for the next `subscribe()`. Keeping one receiver
    /// alive for the entry's whole lifetime is what makes `publish_result`
    /// reliable regardless of whether anything has called `await_result`
    /// yet; never read from directly (`result_tx.borrow()` / a freshly
    /// `subscribe()`d receiver are used for that).
    _keepalive_rx: watch::Receiver<Option<AgentResult>>,
    resolved: AtomicBool,
}

/// The multi-agent tree: attachment, structural lookups, cancellation
/// propagation, and the terminal-result publication guarantee that makes
/// `await_result` always resolve (architecture §7, WI-083's core
/// objective -- the MAST "failure to recognize termination" mitigation).
pub struct AgentTree {
    nodes: RwLock<HashMap<AgentId, TreeEntry>>,
    bus: Arc<EventBus>,
}

impl AgentTree {
    pub fn new(bus: Arc<EventBus>) -> Self {
        Self {
            nodes: RwLock::new(HashMap::new()),
            bus,
        }
    }

    /// Attaches `node`. Errors [`RuntimeError::AgentNotFound`] if
    /// `node.parent` is `Some` and not already attached; errors (see
    /// [`already_attached`]) if `node.id` is already present.
    ///
    /// When `node.kind` is `Some(mode)`, emits exactly one
    /// `Event::AgentSpawned` before returning -- see the module doc for why
    /// this placement is what makes the ordering guarantee hold.
    pub fn attach(&self, node: AgentNode) -> Result<(), RuntimeError> {
        let mut nodes = self.nodes.write().expect("agent tree lock poisoned");
        if nodes.contains_key(&node.id) {
            return Err(already_attached(node.id));
        }
        if let Some(parent) = node.parent {
            if !nodes.contains_key(&parent) {
                return Err(RuntimeError::AgentNotFound { agent: parent });
            }
        }

        let (result_tx, keepalive_rx) = watch::channel(None);
        let spawn_event = node.kind.map(|kind| Event::AgentSpawned {
            kind,
            parent: node.parent,
            agent_def: node.agent_def.clone(),
            inherited_upto: node.inherited_upto,
            ephemeral: node.ephemeral,
        });
        let (session, id) = (node.session, node.id);

        nodes.insert(
            id,
            TreeEntry {
                node,
                result_tx,
                _keepalive_rx: keepalive_rx,
                resolved: AtomicBool::new(false),
            },
        );
        // Released before emitting: `EventBus::emit` is synchronous and
        // cheap, but holding a write lock across any avoidable extra work is
        // against this crate's lock discipline (never more than the minimum
        // needed for the mutation itself).
        drop(nodes);

        if let Some(event) = spawn_event {
            self.bus.emit(session, id, event);
        }
        Ok(())
    }

    /// The root->`agent` chain, including `agent` itself. Empty if `agent`
    /// is unknown (used to populate `PermissionRequest::agent_path`, §4.3).
    pub fn path(&self, agent: AgentId) -> Vec<AgentId> {
        let nodes = self.nodes.read().expect("agent tree lock poisoned");
        let mut chain = Vec::new();
        let mut current = Some(agent);
        while let Some(id) = current {
            match nodes.get(&id) {
                Some(entry) => {
                    chain.push(id);
                    current = entry.node.parent;
                }
                None => return Vec::new(),
            }
        }
        chain.reverse();
        chain
    }

    /// A cancellation token that is a structural child of `parent`'s own
    /// token (`parent.cancel.child_token()`), so cancelling `parent` (or any
    /// ancestor) cancels the returned token too, without ever exposing
    /// `parent`'s own token directly. Added for WI-084's fork/spawn path
    /// (`subagent.rs`), which needs to derive a new child's token per
    /// [`AgentNode::cancel`]'s contract but has no other way to reach an
    /// already-attached node's token — every other method here either trips
    /// a token (`cancel`) or reads publication state, never returns one.
    pub(crate) fn child_cancel_token(
        &self,
        parent: AgentId,
    ) -> Result<CancellationToken, RuntimeError> {
        let nodes = self.nodes.read().expect("agent tree lock poisoned");
        let entry = nodes
            .get(&parent)
            .ok_or(RuntimeError::AgentNotFound { agent: parent })?;
        Ok(entry.node.cancel.child_token())
    }

    /// Trips `agent`'s `CancellationToken`. Because every child's token is
    /// (by construction, see [`AgentNode::cancel`]) a `child_token()` of its
    /// parent's, this structurally cancels the entire subtree in one call.
    pub fn cancel(&self, agent: AgentId, reason: String) -> Result<(), RuntimeError> {
        let nodes = self.nodes.read().expect("agent tree lock poisoned");
        let entry = nodes
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?;
        tracing::info!(agent = %agent, reason = %reason, "AgentTree::cancel");
        entry.node.cancel.cancel();
        Ok(())
    }

    /// Publishes `agent`'s terminal result. Set-once: the first call wins
    /// (`Ok(true)`), every later call for the same agent is silently
    /// discarded (`Ok(false)`) -- a normal completion racing a supervisor
    /// synthesis always yields exactly one observable value.
    pub fn publish_result(
        &self,
        agent: AgentId,
        result: AgentResult,
    ) -> Result<bool, RuntimeError> {
        let nodes = self.nodes.read().expect("agent tree lock poisoned");
        let entry = nodes
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?;
        if entry
            .resolved
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let _ = entry.result_tx.send(Some(result));
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Reads `agent`'s `ephemeral` flag from its attached [`AgentNode`], for
    /// stamping `Event::AgentFinished::ephemeral` at every emission site (the
    /// live `AgentLoop::finish` and the supervisor's synthesized finish).
    /// Returns `false` for an unknown agent (a synthesized finish for an agent
    /// that was never attached, e.g. a bare test mock), matching the
    /// non-ephemeral default every pre-`/ask` path already had.
    pub fn ephemeral_of(&self, agent: AgentId) -> bool {
        let nodes = self.nodes.read().expect("agent tree lock poisoned");
        nodes.get(&agent).map(|entry| entry.node.ephemeral).unwrap_or(false)
    }

    /// Flips `agent`'s `ephemeral` flag in place and returns the agent's
    /// own `SessionId` (so the caller can emit under it without a second
    /// lookup) — the runtime-tree half of the facade's ephemeral→persistent
    /// promote (B3). Added for `Runtime::promote_agent`; unlike
    /// [`ephemeral_of`](Self::ephemeral_of)'s read, an unknown agent is an
    /// error (`RuntimeError::AgentNotFound`), matching every other mutating
    /// lookup here — a promote targeting a non-attached agent is a caller
    /// bug, not a defaultable condition. One-way in practice (the facade
    /// only ever passes `false`, and the store layer refuses the reverse)
    /// but the setter itself is value-agnostic: the tree records what it is
    /// told, it does not adjudicate lifecycle policy.
    pub fn set_ephemeral(&self, agent: AgentId, ephemeral: bool) -> Result<SessionId, RuntimeError> {
        let mut nodes = self.nodes.write().expect("agent tree lock poisoned");
        let entry = nodes
            .get_mut(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?;
        entry.node.ephemeral = ephemeral;
        Ok(entry.node.session)
    }

    /// Awaits `agent`'s terminal result. Always terminates once *something*
    /// publishes one -- the supervisor's guarantee -- with no unresolvable
    /// path. Holds no tree lock while awaiting.
    pub async fn await_result(&self, agent: AgentId) -> Result<AgentResult, RuntimeError> {
        let mut rx = {
            let nodes = self.nodes.read().expect("agent tree lock poisoned");
            let entry = nodes
                .get(&agent)
                .ok_or(RuntimeError::AgentNotFound { agent })?;
            entry.result_tx.subscribe()
        };
        loop {
            let current = rx.borrow().clone();
            if let Some(result) = current {
                return Ok(result);
            }
            rx.changed()
                .await
                .expect("the tree holds this entry's Sender for `agent`'s whole lifetime");
        }
    }

    /// A point-in-time snapshot of every attached agent.
    pub fn snapshot(&self) -> AgentTreeSnapshot {
        let nodes = self.nodes.read().expect("agent tree lock poisoned");
        let projected: Vec<CoreAgentNode> = nodes
            .values()
            .map(|entry| {
                let finished = entry.result_tx.borrow().clone();
                let (status, steps_taken) = match &finished {
                    None => (AgentStatus::Running, 0),
                    Some(result) => (status_for(&result.status), result.steps_taken),
                };
                CoreAgentNode {
                    agent_id: entry.node.id,
                    session: entry.node.session,
                    parent: entry.node.parent,
                    mode: entry.node.kind,
                    agent_def: entry.node.agent_def.clone(),
                    role: entry.node.role.clone(),
                    status,
                    steps_taken,
                    budget: entry.node.budget.clone(),
                    // Same source `Event::AgentSpawned::ephemeral` is stamped
                    // from (see `attach`); ephemeral children stay IN the
                    // snapshot (P-2 provenance) -- this flag is how a
                    // consumer distinguishes them from persistent subagents.
                    ephemeral: entry.node.ephemeral,
                }
            })
            .collect();

        // `AgentTreeSnapshot::root` has no way to name "the roots" plural
        // (WI-082's own documented gap, unchanged by this item): prefer an
        // actual root (no parent) over an arbitrary node when more than one
        // has been started, as a best-effort tie-break rather than a fix.
        let root = projected
            .iter()
            .find(|n| n.parent.is_none())
            .or_else(|| projected.first())
            .map(|n| n.agent_id)
            .unwrap_or_default();

        AgentTreeSnapshot {
            root,
            nodes: projected,
            at: Utc::now(),
        }
    }
}

/// `RuntimeError` (conway-core, out of this item's file scope) has no
/// "duplicate agent" variant. `ToolError::Internal` is the same fallback
/// `runtime.rs`'s `NoSubagentHost` stub already uses for a gap shaped like
/// this one (see that module's doc comment) -- reused here rather than
/// inventing a second ad hoc mapping for the same kind of absence.
fn already_attached(id: AgentId) -> RuntimeError {
    RuntimeError::Tool(ToolError::Internal {
        detail: format!("agent {id} is already attached to the tree"),
    })
}

/// Maps a terminal `ResultStatus` to the tree's coarser `AgentStatus`.
/// `ResultStatus` is `#[non_exhaustive]`; unrecognized future variants map
/// to `Finished` rather than failing to compile or panicking (mirrors the
/// WI-082 `tree()` stub this item supersedes).
fn status_for(status: &ResultStatus) -> AgentStatus {
    match status {
        ResultStatus::Completed => AgentStatus::Finished,
        ResultStatus::Failed { .. } => AgentStatus::Failed,
        ResultStatus::Cancelled { .. } => AgentStatus::Cancelled,
        ResultStatus::BudgetExceeded { .. } => AgentStatus::Finished,
        ResultStatus::Rejected { .. } => AgentStatus::Finished,
        _ => AgentStatus::Finished,
    }
}

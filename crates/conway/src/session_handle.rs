//! `SessionHandle`, `TurnHandle`, `SessionSpec`: the Slice 1 consumer-facing
//! surface over one running `conway-runtime::Runtime` session (WI-101).
//!
//! All `SessionHandle` methods are thin delegations to `Runtime`; no method
//! takes `&mut self` -- every state change routes through the runtime, not
//! through local mutation.
//!
//! **Relocation note (disclosed, per WI-100's own F-100-1 deviation #3):**
//! `SessionHandle` and `SessionSpec` were previously a minimal stub living
//! in `crate::conway` (WI-100 landed only `id()`/`root()`, with an explicit
//! comment that WI-101 owns moving them here). This file is that move,
//! plus the full surface this item specifies.

use std::future::poll_fn;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use conway_core::agent::{AgentResult, AgentTreeSnapshot, Budget};
use conway_core::content::{ContentBlock, ToolResult};
use conway_core::error::{RuntimeError, StoreError};
use conway_core::event::{Envelope, Event};
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter};
use conway_core::ports::{SessionStore, SubagentHost};
use conway_core::provenance::ContextReport;
use conway_runtime::runtime::Runtime;
use futures_core::Stream;
use tokio::sync::Mutex as AsyncMutex;

use crate::error::{ConwayError, Result};
use crate::event_stream::EventStream;
use crate::subagent_spec::{ForkSpec, SpawnSpec};

/// The parameters for `Conway::new_session`.
///
/// Every field defaults to `None`/`vec![]` via `#[derive(Default)]`;
/// `Conway::new_session` resolves each absent field from its `ConwayConfig`
/// at call time. `SessionSpec::default()` itself is necessarily
/// config-agnostic (`Default::default()` takes no arguments) -- the
/// "defaulted" shape described in terms of `config.default_role`/
/// `config.cwd`/`config.limits` describes the *effective*, post-resolution
/// session `new_session` produces, not the literal struct this type's
/// `Default` impl returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionSpec {
    /// Overrides the store-assigned session id (WI-119). `None` mints a
    /// fresh [`SessionId`] as before; `Some` is passed straight through to
    /// `RootSpec::session`, so `Runtime::start_root`'s own internal
    /// `store.create` becomes the single, authoritative creation call under
    /// exactly that id.
    pub id: Option<SessionId>,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub cwd: Option<PathBuf>,
    pub budget: Option<Budget>,
    pub labels: Vec<String>,
    /// Opt-in multi-turn keep-alive: passed straight through to
    /// `RootSpec::keep_alive` (`conway_runtime::runtime::RootSpec` -- see
    /// that field's own doc for the confirmed bug this fixes and why it
    /// must stay opt-in, not universal). `false` (`SessionSpec::default()`)
    /// preserves this crate's pre-existing behavior exactly: the session's
    /// root agent task terminates after its first `Completed` turn, and a
    /// second `SessionHandle::prompt` on the same handle silently runs no
    /// turn. `true` is what `conway-cli`'s TUI opts its own root session
    /// into, so a second chat message in the same process actually runs.
    pub keep_alive: bool,
}

/// A live handle onto one running session: `id()`/`root()` are static, and
/// every other method is a thin delegation to the `Arc<Runtime>` this
/// `Conway` assembled. Cheap to `Clone` -- every field is `Arc`/`Copy`.
#[derive(Clone)]
pub struct SessionHandle {
    rt: Arc<Runtime>,
    session: SessionId,
    root: AgentId,
    store: Arc<dyn SessionStore>,
}

impl SessionHandle {
    pub(crate) fn new(
        rt: Arc<Runtime>,
        session: SessionId,
        root: AgentId,
        store: Arc<dyn SessionStore>,
    ) -> Self {
        Self {
            rt,
            session,
            root,
            store,
        }
    }

    pub fn id(&self) -> SessionId {
        self.session
    }

    pub fn root(&self) -> AgentId {
        self.root
    }

    /// Delegates to `Runtime::prompt(self.root, text)` with no
    /// transformation of `text`, then returns a [`TurnHandle`] over a
    /// broadcast subscription taken out *before* the prompt is appended --
    /// so the turn's own first events can never be missed by a
    /// subscribe-after-append race.
    ///
    /// **Concurrent-call footgun:** if two `prompt` calls race and both land
    /// before the agent's own idle loop wakes to consume them, both
    /// `UserTurn` records are durably appended regardless (no data lost --
    /// whichever turn runs next re-reads the full session history and sees
    /// both), but the wake signal itself is a single-permit
    /// `tokio::sync::Notify` (`conway_runtime::agent_loop::ResumeGate`'s own
    /// doc) shared by the whole agent -- a second `notify_one()` before the
    /// first is consumed is not queued, just coalesced into the same
    /// permit. The practical effect: both callers' `TurnHandle`s each hold
    /// their own event subscription, but both end up observing the SAME
    /// underlying turn's `TurnFinished`/`AgentFinished`, not one turn each.
    /// Harmless for `conway-cli`'s TUI (input is strictly sequential -- one
    /// prompt in flight at a time) but a footgun for any caller issuing
    /// concurrent `prompt`s against the same session.
    pub async fn prompt(&self, text: impl Into<String>) -> Result<TurnHandle> {
        let stream = EventStream::live(self.session, Some(self.root), self.rt.subscribe());
        self.rt.prompt(self.root, text.into()).await?;
        Ok(TurnHandle::new(
            self.rt.clone(),
            self.session,
            self.root,
            stream,
        ))
    }

    /// The `/ask` fork-ask primitive: forks this session's agent at its
    /// CURRENT head into a fresh, ephemeral child -- inheriting this
    /// session's entire context and tool set, since a fork always does (see
    /// `crate::fork_child`'s doc for the shared fork+resume sequence) -- then
    /// drives the child's first turn with `text`, exactly as `prompt` would
    /// for a normal turn.
    ///
    /// Returns a [`TurnHandle`] over the CHILD, in the same shape `prompt`
    /// returns one over `self` -- so a caller (e.g. the `/ask` TUI command,
    /// a separate slice) subscribes and renders it exactly like a normal
    /// prompt turn, with no special-casing. `self`'s own transcript and live
    /// agent are untouched: no record is appended to `self.session`, and
    /// `self.root` never sees the question -- fork semantics already
    /// guarantee this (a child is bounded at its fork seq; the parent only
    /// ever reads its own ancestry, never a child's turns).
    ///
    /// The child is born with `SessionMeta::ephemeral: true` -- set once, at
    /// fork time, in the child's own header; there is no way to flip it
    /// later, `SessionStore` being append-only with no meta-update or delete
    /// method. This is what keeps it out of `Conway::sessions`'s default
    /// listing and `SessionStore::children` -- see those methods' own docs
    /// for the default-exclude filtering this depends on. The child remains
    /// reachable only through this call's own returned `TurnHandle`/child
    /// `SessionId`, or via `SessionFilter{include_ephemeral: true, ..}`.
    ///
    /// **Tool inheritance:** the child's `SessionMeta.agent_def` defaults to
    /// this session's own (`crate::fork_child::fork_child`'s fallback -- see
    /// that function's doc), so `resume_root`'s tool resolution lands on the
    /// same tool set this session's own agent runs with. Tool calls the
    /// child makes during the ask are real and permanent -- only the
    /// transcript is ephemeral; that is intended, not a gap (a throwaway
    /// *question* does not imply throwaway tool side effects).
    ///
    /// **Disclosed simplification:** the child's `budget` is always
    /// `Budget::default()` (`conway-core`'s baseline), not this session's own
    /// configured budget -- `SessionHandle` has no reference to that
    /// configuration to read it from, the same disclosed deviation
    /// `ForkSpec::new`'s own doc already makes for `SessionHandle::fork`.
    pub async fn ask(&self, text: impl Into<String>) -> Result<TurnHandle> {
        let parent_meta = self.store.meta(&self.session).await?;
        let at = self.store.head(&self.session).await?;
        let child = crate::fork_child::fork_child(
            &self.rt,
            &self.store,
            self.session,
            parent_meta,
            at,
            crate::fork_child::ForkChildRequest {
                agent_def: None,
                role: None,
                tools: None,
                budget: Budget::default(),
                ephemeral: true,
            },
        )
        .await?;
        child.prompt(text).await
    }

    /// Every envelope emitted for this session (no agent filter beyond
    /// that -- see `events_from`'s doc on why session alone is already
    /// agent-scoped in this architecture).
    pub fn events(&self) -> EventStream {
        EventStream::live(self.session, None, self.rt.subscribe())
    }

    /// Replays persisted envelopes for this session from `seq` onward, then
    /// switches to the live broadcast. See
    /// [`EventStream::replay_then_live`] and [`record_to_event`] for the
    /// disclosed reconciliations this method's replay batch depends on --
    /// in particular, what the replay/live junction does and does not
    /// guarantee about duplicates.
    ///
    /// Session-scoping note: `SessionStore` keys one session per agent
    /// (`SessionId` docs: "one agent's append-only log"), so a session's
    /// own live envelopes are already exactly that agent's envelopes --
    /// filtering by `session` alone (as `EventStream::live`/
    /// `replay_then_live` do) cannot admit another agent's events, since
    /// those are published under a different `SessionId`.
    pub async fn events_from(&self, seq: LogSeq) -> Result<EventStream> {
        // Subscribing before reading is what guarantees NO GAP: everything
        // broadcast from this instant onward is captured live, so nothing
        // can fall through the seam uncaptured. The cost is the mirror-image
        // risk -- a record persisted (and broadcast live) in the gap
        // between this subscribe and the store read below lands in *both*
        // the replay batch and on `live`. `EventStream::replay_then_live`
        // is handed `subscribed_at` precisely so it can detect and drop
        // that live-side duplicate at the junction; see its doc for the
        // mechanism and its disclosed residual gap.
        let live = self.rt.subscribe();
        let subscribed_at = Utc::now();
        let records = self
            .store
            .read(&self.session, SeqRange::new(seq, None))
            .await?;
        let replay = records
            .iter()
            .filter_map(record_to_event)
            .map(|(_, ts, event)| Envelope {
                seq: 0, // renumbered by `EventStream::replay_then_live`
                ts,
                session: self.session,
                agent: self.root,
                event,
            })
            .collect();
        Ok(EventStream::replay_then_live(
            self.session,
            None,
            replay,
            subscribed_at,
            live,
        ))
    }

    /// A snapshot of the whole agent tree this `Conway`'s `Runtime` knows
    /// about (`Runtime::tree`, sync, no I/O). Delegated unchanged: neither
    /// `Runtime::tree` nor `AgentTreeSnapshot` offers a way to scope the
    /// snapshot to one session's own subtree, so (matching this item's
    /// "thin delegation" objective) this method does not attempt to filter
    /// it -- disclosed rather than silently narrowed or widened.
    pub fn tree(&self) -> AgentTreeSnapshot {
        self.rt.tree()
    }

    /// Delegates to `Runtime::context_report`, which is itself synchronous
    /// (an in-memory read, no I/O) -- this method is `async` only to match
    /// the binding criterion's signature; there is nothing to await.
    pub async fn context_report(&self, agent: AgentId) -> Result<ContextReport> {
        Ok(self.rt.context_report(agent)?)
    }

    /// The persisted `ContextReport` for `agent`'s historical `turn`
    /// (carried from the capstone review, F-114-1-adjacent): a thin
    /// delegation to `Runtime::context_report_at`, which -- unlike
    /// `context_report` above -- reads durably from the store rather than
    /// the live `last_report` slot, so it works even across a process
    /// restart (see that method's own doc for `resolve_session`'s
    /// live-then-store-scan fallback).
    pub async fn context_report_at(&self, agent: AgentId, turn: u32) -> Result<ContextReport> {
        Ok(self.rt.context_report_at(agent, turn).await?)
    }

    /// The *effective* transcript for `agent`: its own records, prefixed by
    /// its full fork ancestry (recursively resolved), matching
    /// `conway_session::TranscriptResolver::resolve`'s semantics.
    ///
    /// **Reconciliation (disclosed):** this item's binding notes name
    /// `conway_session::TranscriptResolver` as the mechanism. It cannot be
    /// used directly here: `crates/conway/Cargo.toml` (WI-096, out of this
    /// item's file scope) gates the `conway-session` dependency behind the
    /// optional `jsonl-store` feature, but `SessionHandle` is core surface
    /// and must stay feature-independent (this item's own test matrix runs
    /// `--no-default-features`). Depending on `conway_session::` here
    /// unconditionally would not compile under that configuration; gating
    /// just this method behind `#[cfg(feature = "jsonl-store")]` would
    /// silently remove a criterion-mandated method under a feature
    /// combination nothing else requires it to disappear under. Instead,
    /// this method (and its private helper, `resolve_prefix`) reimplements
    /// `TranscriptResolver`'s ancestry walk directly against
    /// `conway_core::ports::SessionStore` (always available, unconditional
    /// port trait) -- the same algorithm, without that type's LRU
    /// memoization (sibling forks each re-walk their shared prefix; a
    /// correctness/performance tradeoff, not a correctness gap).
    ///
    /// Also resolves `agent` to its owning `SessionId` first
    /// (`Runtime::agent_session`/`resolve_session`, the only existing
    /// AgentId -> SessionId lookups in this workspace, are both
    /// `pub(crate)` to `conway-runtime` and unreachable from here), by the
    /// same list-and-match fallback `Runtime::resolve_session`'s own doc
    /// describes as an accepted O(session count) MVP cost.
    ///
    /// **Spawned children show a parent prefix here** even though their
    /// *context* is clean-slate -- see [`SessionHandle::spawn`]'s doc for
    /// why (`SessionMeta.origin` is recorded on spawn too, and this
    /// method's ancestry walk does not distinguish fork from spawn).
    pub async fn transcript(&self, agent: AgentId) -> Result<Vec<LogRecord>> {
        let session = self.resolve_agent_session(agent).await?;
        self.effective_transcript(session).await
    }

    async fn resolve_agent_session(&self, agent: AgentId) -> Result<SessionId> {
        if agent == self.root {
            return Ok(self.session);
        }
        // `include_ephemeral: true` -- this is an identity lookup (does
        // `agent` belong to some session this store knows about?), not a
        // catalog browse; the default exclude-ephemeral filter exists to
        // hide `/ask` scratchpads from *listings* (`Conway::sessions`,
        // `sessions tree`), not to make them unresolvable by an agent id a
        // caller already legitimately holds (e.g. from `SessionHandle::ask`'s
        // own returned `TurnHandle`).
        let sessions = self
            .store
            .list(SessionFilter {
                include_ephemeral: true,
                ..SessionFilter::default()
            })
            .await?;
        sessions
            .into_iter()
            .find(|meta| meta.agent_id == agent)
            .map(|meta| meta.id)
            .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound { agent }))
    }

    async fn effective_transcript(&self, session: SessionId) -> Result<Vec<LogRecord>> {
        let head = self.store.head(&session).await?;
        self.resolve_prefix(session, head).await
    }

    /// Mirrors `conway_session::TranscriptResolver::resolve_prefix`: walks
    /// `session`'s fork ancestry (via `SessionMeta.origin`) up to a root,
    /// bounding each ancestor at its own fork point, then concatenates from
    /// the root down, ending with `session`'s own records up to `upto`.
    async fn resolve_prefix(&self, session: SessionId, upto: LogSeq) -> Result<Vec<LogRecord>> {
        const MAX_ANCESTRY_DEPTH: usize = 256;

        // Walk upward, collecting each level's (session, own-record-bound)
        // pair, then read root-to-leaf.
        let mut chain = vec![(session, upto)];
        loop {
            let (cur, _) = *chain
                .last()
                .expect("chain always has at least the starting session");
            let meta = self.store.meta(&cur).await?;
            match meta.origin {
                Some(origin) => chain.push((origin.parent, origin.at_seq)),
                None => break,
            }
            if chain.len() > MAX_ANCESTRY_DEPTH {
                return Err(ConwayError::Runtime(RuntimeError::Store(
                    StoreError::Corrupt {
                        session,
                        line: 0,
                        detail: format!("fork ancestry exceeds max depth ({MAX_ANCESTRY_DEPTH})"),
                    },
                )));
            }
        }

        let mut records = Vec::new();
        for (sid, bound) in chain.into_iter().rev() {
            let batch = self
                .store
                .read(&sid, SeqRange::new(LogSeq::ZERO, Some(bound)))
                .await?;
            records.extend(batch);
        }
        Ok(records)
    }
}

// ---------------------------------------------------------------------
// WI-102: subagent surface (fork/spawn/steer/await_agent/cancel).
//
// Pure delegation to `Runtime`'s `impl SubagentHost` (conway-runtime,
// WI-084) -- see `crate::subagent_spec` for `ForkSpec`/`SpawnSpec` and
// their `From` conversions into `conway_core::agent::SubagentSpec`. GP-02:
// `fork` inherits the forker's entire context plus a directive; `spawn` is
// clean-slate -- kept as distinct methods/types rather than one
// mode-flagged call, matching `ForkSpec`/`SpawnSpec`'s own split.
//
// This block intentionally names none of the four storage/tree-internal
// port and helper types fork/spawn *logic* would touch (the session-log
// port, the fork-ancestry resolver, the context assembler, or the
// in-memory multi-agent tree type) -- that logic lives in conway-runtime,
// not here; this block only calls through `SubagentHost` and reads
// `Runtime::tree()`'s already-public snapshot. `tests/
// session_handle_subagent.rs` greps everything from this marker onward to
// check that (see that test for the exact identifiers, deliberately not
// spelled out here so the rule this comment describes doesn't itself trip
// the check it describes). It is scoped to this block rather than the
// whole file because the methods above this marker (WI-101, unmodified by
// this item) already reference one of those types legitimately, for a
// concern this item has nothing to do with.
// ---------------------------------------------------------------------
impl SessionHandle {
    /// Forks a live agent: GP-02's "inherit everything, plus a directive"
    /// mode. Delegates to `Runtime::start` (`impl SubagentHost`) with
    /// `spec.into()` unmodified beyond the `ForkSpec` -> `SubagentSpec`
    /// conversion itself.
    ///
    /// **T-1 (disclosed, unresolved in the architecture):** the fork
    /// overflow policy -- what happens when the inherited context plus
    /// `spec.directive` already exceeds the target model's window -- has no
    /// settled design. This method does not add an `on_overflow` field or
    /// otherwise paper over it: whatever typed error the runtime returns
    /// (today, `RuntimeError::ForkContextOverflow`, terminal, no truncation
    /// or escalation) surfaces to the caller unchanged, wrapped only in
    /// `ConwayError::Runtime`.
    ///
    /// Rejects `from` with `Err(ConwayError::Runtime)` when it does not
    /// belong to this session's agent tree -- see
    /// `SessionHandle::ensure_agent_in_session`'s doc for exactly what error
    /// that produces (`RuntimeError::AgentNotFound` vs. `AgentNotInSession`).
    pub async fn fork(&self, from: AgentId, spec: ForkSpec) -> Result<AgentId> {
        self.ensure_agent_in_session(from)?;
        self.rt
            .start(from, spec.into())
            .await
            .map_err(ConwayError::Runtime)
    }

    /// Spawns a fresh agent: GP-02's clean-slate mode. Delegates to
    /// `Runtime::start` (`impl SubagentHost`) with `spec.into()` unmodified
    /// beyond the `SpawnSpec` -> `SubagentSpec` conversion itself.
    ///
    /// Rejects `from` with `Err(ConwayError::Runtime)` when it does not
    /// belong to this session's agent tree -- see
    /// `SessionHandle::ensure_agent_in_session`'s doc.
    ///
    /// **Transcript quirk (disclosed):** "clean-slate" describes the
    /// spawned child's *context* only -- `inherited` stays `None`, so
    /// nothing from `from`'s history is fed to the model. It does not
    /// describe [`SessionHandle::transcript`]'s output for that child:
    /// `SessionMeta.origin` is still recorded on spawn (for tree
    /// reconstructability), and `transcript`'s ancestry walk follows any
    /// `Some(origin)` unconditionally, without distinguishing fork from
    /// spawn. So a spawned child's *transcript* still shows the parent's
    /// prefix, even though its *context* never did. An embedder building a
    /// history UI from `transcript()` should account for this.
    pub async fn spawn(&self, from: AgentId, spec: SpawnSpec) -> Result<AgentId> {
        self.ensure_agent_in_session(from)?;
        self.rt
            .start(from, spec.into())
            .await
            .map_err(ConwayError::Runtime)
    }

    /// Delivers `text` to `target` as a steer message, landing at `target`'s
    /// next turn boundary (WI-085). Delegates to `Runtime::steer` (`impl
    /// SubagentHost`) with `text` converted and otherwise unmodified.
    ///
    /// Rejects `target` with `Err(ConwayError::Runtime)` when it does not
    /// belong to this session's agent tree -- `Arc<Runtime>` (and its
    /// runtime-wide, unscoped `tree()`) is shared across every
    /// `SessionHandle` a `Conway` produces, so without this check any handle
    /// could steer another session's agent. See
    /// `SessionHandle::ensure_agent_in_session`'s doc for exactly what error
    /// that produces (`RuntimeError::AgentNotFound` vs. `AgentNotInSession`).
    pub async fn steer(&self, target: AgentId, text: impl Into<String>) -> Result<()> {
        self.ensure_agent_in_session(target)?;
        self.rt
            .steer(target, text.into())
            .await
            .map_err(ConwayError::Runtime)
    }

    /// Awaits `target`'s terminal result. Always resolves `Ok` -- including
    /// when `target` finished `BudgetExceeded` or `Cancelled` -- since the
    /// runtime's supervisor guarantees a result is published no matter how
    /// the agent ends; only an unknown `target` produces `Err`. Delegates to
    /// `Runtime::await_result` (`impl SubagentHost`) unmodified.
    ///
    /// Rejects `target` with `Err(ConwayError::Runtime)` when it does not
    /// belong to this session's agent tree -- `AgentResult` is another
    /// session's data, and reading it across the session boundary is an
    /// isolation violation just as steering/cancelling it would be. See
    /// `SessionHandle::ensure_agent_in_session`'s doc.
    pub async fn await_agent(&self, target: AgentId) -> Result<AgentResult> {
        self.ensure_agent_in_session(target)?;
        self.rt
            .await_result(target)
            .await
            .map_err(ConwayError::Runtime)
    }

    /// Cancels `target` with `reason`. Delegates to `Runtime::cancel` (`impl
    /// SubagentHost`) with `reason` converted and otherwise unmodified.
    ///
    /// Rejects `target` with `Err(ConwayError::Runtime)` when it does not
    /// belong to this session's agent tree -- without this check any handle
    /// could hard-cancel another session's agent, since `cancel` is a
    /// mutating control-plane op reached through the same runtime-wide
    /// `Arc<Runtime>` every `SessionHandle` shares. See
    /// `SessionHandle::ensure_agent_in_session`'s doc.
    ///
    /// Called through the `SubagentHost` trait explicitly (`SubagentHost::
    /// cancel(...)`, not `self.rt.cancel(...)`): `Runtime` also has its own
    /// inherent, synchronous `cancel` method (pre-existing, used elsewhere
    /// in this crate's own dependency graph) with the same name and a
    /// compatible-looking signature; Rust's method resolution prefers an
    /// inherent method over a trait method with the same receiver type, so
    /// a plain `self.rt.cancel(...)` call would silently bind to that
    /// inherent method instead of the trait method this criterion is about
    /// -- harmless here (the trait impl is a pure pass-through to that same
    /// inherent method, confirmed in `conway-runtime`'s `subagent.rs`), but
    /// named explicitly so this delegation's intent (going through
    /// `SubagentHost`, the port this item's criteria are specified against)
    /// isn't left to an incidental method-resolution tie-break.
    pub async fn cancel(&self, target: AgentId, reason: &str) -> Result<()> {
        self.ensure_agent_in_session(target)?;
        SubagentHost::cancel(self.rt.as_ref(), target, reason.to_string())
            .await
            .map_err(ConwayError::Runtime)
    }

    /// Verifies `agent` is reachable from `self.root` by walking
    /// `AgentNode.parent` links in `Runtime::tree()`'s snapshot -- the
    /// "session-ownership check" the WI-102 binding notes describe. Called
    /// as the first step of every method above that takes an `AgentId` not
    /// already known to be `self.root` (`fork`, `spawn`, `steer`,
    /// `await_agent`, `cancel`): `Arc<Runtime>` is shared across every
    /// `SessionHandle` a `Conway` produces and `tree()` is runtime-wide, so
    /// without this check any handle could act on -- or, for `await_agent`,
    /// read the result of -- another session's agent.
    ///
    /// This check is deliberately structural (not a `session` field comparison): every
    /// agent in this workspace gets its own `SessionId` (one agent's
    /// append-only log per `SessionId`'s own doc), so a forked or spawned
    /// child's `AgentNode.session` is never equal to `self.session` -- only
    /// reachability via `parent` proves membership in this handle's tree.
    ///
    /// **F-102-1, resolved (WI-119):** `conway_core::error::RuntimeError`
    /// now has a dedicated `AgentNotInSession { agent, session }` variant
    /// (rendering `"agent {agent} does not belong to session {session}"`),
    /// added specifically to close the gap this method's previous doc
    /// disclosed. This method now distinguishes the two failure shapes:
    /// `agent` absent from `Runtime::tree()` entirely -> `AgentNotFound`
    /// (unknown to this runtime, full stop); `agent` present in the tree but
    /// not a descendant of `self.root` -> `AgentNotInSession` (a real agent,
    /// just not one this handle may act on).
    fn ensure_agent_in_session(&self, agent: AgentId) -> Result<()> {
        if agent == self.root {
            return Ok(());
        }
        let snapshot = self.rt.tree();
        let mut parent_of = std::collections::HashMap::new();
        let mut present = false;
        for node in &snapshot.nodes {
            if node.agent_id == agent {
                present = true;
            }
            parent_of.insert(node.agent_id, node.parent);
        }
        if !present {
            return Err(ConwayError::Runtime(RuntimeError::AgentNotFound { agent }));
        }
        let mut cursor = agent;
        loop {
            match parent_of.get(&cursor) {
                Some(Some(parent)) if *parent == self.root => return Ok(()),
                Some(Some(parent)) => cursor = *parent,
                _ => break,
            }
        }
        Err(ConwayError::Runtime(RuntimeError::AgentNotInSession {
            agent,
            session: self.session,
        }))
    }
}

/// Synthesizes an `(seq, ts, Event)` from one persisted `LogRecord`, for
/// `SessionHandle::events_from`'s replay batch. Returns `None` for
/// `LogRecord::Header` (no `seq`, not a replayable occurrence) and for any
/// future variant this `#[non_exhaustive]` enum grows that this function
/// does not yet know about.
///
/// **Disclosed gap:** no committed mapping between `LogRecord` (persisted,
/// one entry per session-log line) and `Event` (the live, ephemeral
/// broadcast wire format) exists anywhere in this workspace -- confirmed by
/// grep, not merely unwritten. They are independent representations of
/// different cardinality: e.g. live, one `Assistant` record's worth of a
/// turn corresponds to a run of `TextDelta`s plus one `TurnFinished`, and
/// `UserTurn`/`ForkDirective`/`ParentSteer` have no `Event` counterpart at
/// all today. This function uses the one faithful mapping that does exist
/// where it exists -- `AgentResultRecord` -> `Event::AgentFinished`,
/// matching exactly what `conway-runtime`'s agent loop emits live for that
/// occurrence, and `Assistant` -> `Event::TurnFinished{usage, stop}`, same
/// rationale -- and falls back to `Event::AgentProgress{note}` (the one
/// variant that exists precisely for free-text informational replay) for
/// every record kind with no faithful equivalent, rather than inventing a
/// new `Event` variant outside this item's file scope (`conway-core` owns
/// that enum).
fn record_to_event(record: &LogRecord) -> Option<(LogSeq, DateTime<Utc>, Event)> {
    match record {
        LogRecord::Header(_) => None,
        LogRecord::UserTurn { seq, ts, text, .. } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("user turn: {text}"),
            },
        )),
        LogRecord::Assistant {
            seq,
            ts,
            usage,
            stop,
            ..
        } => Some((
            *seq,
            *ts,
            Event::TurnFinished {
                usage: *usage,
                stop: *stop,
            },
        )),
        LogRecord::ToolCallRecord { seq, ts, call } => Some((
            *seq,
            *ts,
            Event::ToolCallProposed {
                call_id: call.call_id.clone(),
                tool: call.name.clone(),
                args: call.arguments.clone(),
            },
        )),
        LogRecord::ToolResultRecord { seq, ts, result } => Some((
            *seq,
            *ts,
            Event::ToolCallFinished {
                call_id: result.call_id.clone(),
                is_error: result.is_error,
                preview: tool_result_preview(result),
            },
        )),
        LogRecord::ForkDirective {
            seq, ts, text, by, ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("fork directive from {by}: {text}"),
            },
        )),
        LogRecord::ParentSteer {
            seq,
            ts,
            text,
            from,
            ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("parent steer from {from}: {text}"),
            },
        )),
        LogRecord::SystemNote {
            seq,
            ts,
            text,
            reason,
            ..
        } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!("{reason}: {text}"),
            },
        )),
        LogRecord::AgentResultRecord { seq, ts, result } => Some((
            *seq,
            *ts,
            Event::AgentFinished {
                result: result.clone(),
            },
        )),
        LogRecord::ContextReportRecord { seq, ts, report } => Some((
            *seq,
            *ts,
            Event::AgentProgress {
                note: format!(
                    "context report: {} segments, {} tokens",
                    report.segments.len(),
                    report.total_tokens_est
                ),
            },
        )),
        _ => None,
    }
}

/// The first text block's text, truncated to 200 chars -- mirrors
/// `conway-runtime`'s own live `ToolCallFinished.preview` derivation
/// (`crates/conway-runtime/src/tools/runner.rs`'s `preview_text`, private
/// to that crate and thus not reusable here).
fn tool_result_preview(result: &ToolResult) -> String {
    const PREVIEW_LIMIT: usize = 200;
    let text = result
        .blocks
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or_default();
    text.chars().take(PREVIEW_LIMIT).collect()
}

/// A prompt in flight: wraps one internal, agent-scoped [`EventStream`]
/// subscription taken out before the prompt was appended (see
/// `SessionHandle::prompt`).
pub struct TurnHandle {
    rt: Arc<Runtime>,
    session: SessionId,
    agent: AgentId,
    inner: AsyncMutex<TurnHandleInner>,
}

struct TurnHandleInner {
    stream: EventStream,
    /// `text()` stops draining as soon as it sees `Event::AgentFinished`
    /// (a turn that both streams and terminates the agent within the same
    /// generation), buffering the result here so a later `result()` call
    /// on the same handle resolves it instead of re-draining a stream that
    /// has nothing left to yield -- the mechanism the binding criterion
    /// asks for ("`text()` then `result()` on the same handle must not
    /// deadlock").
    buffered_result: Option<AgentResult>,
}

impl TurnHandle {
    fn new(rt: Arc<Runtime>, session: SessionId, agent: AgentId, stream: EventStream) -> Self {
        Self {
            rt,
            session,
            agent,
            inner: AsyncMutex::new(TurnHandleInner {
                stream,
                buffered_result: None,
            }),
        }
    }

    /// Concatenates every `Event::TextDelta` observed for this turn, up to
    /// (not including) the first `Event::TurnFinished` -- or, if the agent
    /// finishes within the same generation without a distinct
    /// `TurnFinished`, up to `Event::AgentFinished` (whose `AgentResult` is
    /// buffered for a subsequent `result()` call).
    pub async fn text(&self) -> Result<String> {
        let mut inner = self.inner.lock().await;
        let mut text = String::new();
        while let Some(envelope) = next_envelope(&mut inner.stream).await {
            match envelope.event {
                Event::TextDelta { text: delta } => text.push_str(&delta),
                Event::TurnFinished { .. } => break,
                Event::AgentFinished { result } => {
                    inner.buffered_result = Some(result);
                    break;
                }
                _ => {}
            }
        }
        Ok(text)
    }

    /// Resolves on `Event::AgentFinished` -- including when the terminal
    /// `AgentResult.status` is `BudgetExceeded` or `Cancelled`: both are
    /// still delivered as one `AgentFinished` event (architecture §8: every
    /// `AgentSpawned` is eventually followed by exactly one
    /// `AgentFinished`), never as a stream error.
    ///
    /// **`SessionSpec::keep_alive` sessions:** the "exactly one
    /// `AgentFinished`" pairing above holds for the session's root agent as
    /// a WHOLE, not per turn. A keep-alive turn that completes with no
    /// pending work does not emit `AgentFinished` at all -- it idle-awaits
    /// the next prompt instead (`conway_runtime::agent_loop::AgentLoop`'s
    /// own doc on its natural-completion branch); the ONE `AgentFinished`
    /// a keep-alive session ever produces arrives only when the session
    /// itself really ends (cancel/deadline/budget), for whichever turn is
    /// in flight at that moment. Concretely: `let turn =
    /// handle.prompt(x).await?; turn.result().await` will hang for the
    /// lifetime of the session if `x`'s turn completes normally -- consume
    /// a keep-alive session's individual turns via [`Self::text`] or
    /// [`Self::events`] instead, and reserve `result()` for the case where
    /// the caller genuinely wants to block until the whole session
    /// terminates.
    ///
    /// The `AgentNotFound` error below is not expected to occur in
    /// practice (it only fires if the runtime's broadcast bus itself ends,
    /// which happens only when every `Arc<Runtime>` -- including the one
    /// this handle and its owning `SessionHandle` hold -- has already been
    /// dropped); it exists so this method has a total, typed return rather
    /// than panicking on an unreachable-but-not-provably-impossible stream
    /// end.
    pub async fn result(&self) -> Result<AgentResult> {
        let mut inner = self.inner.lock().await;
        if let Some(result) = inner.buffered_result.take() {
            return Ok(result);
        }
        loop {
            match next_envelope(&mut inner.stream).await {
                Some(envelope) => {
                    if let Event::AgentFinished { result } = envelope.event {
                        return Ok(result);
                    }
                }
                None => {
                    return Err(ConwayError::Runtime(RuntimeError::AgentNotFound {
                        agent: self.agent,
                    }));
                }
            }
        }
    }

    /// A fresh, independent, live event subscription scoped to this turn's
    /// agent -- distinct from the internal stream `text()`/`result()`
    /// drain, so calling this does not consume events those methods still
    /// need (and vice versa).
    pub fn events(&self) -> EventStream {
        EventStream::live(self.session, Some(self.agent), self.rt.subscribe())
    }
}

async fn next_envelope(stream: &mut EventStream) -> Option<Envelope> {
    poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}

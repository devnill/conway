//! `impl SubagentHost for Runtime` (WI-084, architecture §4.6, §5.1, §5.2):
//! the cycle-breaking fork/spawn entry point every tool call and developer
//! API goes through (decision 2). Fork and spawn are both, mechanically,
//! "create a child session, resolve its starting context, attach it to the
//! tree, and launch its `AgentLoop`" — the only real difference between
//! them is *how* the starting context is resolved: a fork's `InheritedPrefix`
//! is the parent's own effective transcript up to the fork point (GP-02:
//! the ENTIRE context up to the fork point, not a truncated slice); a
//! spawn's context has no inherited prefix at all, by design.
//!
//! ## `InheritedPrefix` and sibling sharing
//!
//! [`conway_session::TranscriptResolver`] resolves a *session's own*
//! effective transcript (ancestors' prefix, in full, concatenated with that
//! session's own records up to its current head) — it has no public method
//! that resolves an arbitrary ancestor at an arbitrary bound directly. This
//! module gets exactly that (the parent's prefix at `at_seq`, and nothing
//! of the child's own) by exploiting timing: right after `store.fork`
//! creates the child (and *before* this module appends the child's own
//! `ForkDirective` record — i.e. while the child has zero own records),
//! `resolver.resolve(store, &child)` necessarily walks up to the parent,
//! computes the parent's prefix at `at_seq`, and — because the child itself
//! owns nothing yet — returns exactly that prefix *as its own return
//! value* (the resolver's own short-circuit for `level_upto ==
//! LogSeq::ZERO` is a plain `Arc::clone`, not a fresh allocation, so this
//! is the very same `Arc` `resolve` just memoized under `(parent,
//! at_seq)`, not a second copy of it). `start` below uses that return
//! value directly as `InheritedPrefix::records`; it does not re-fetch
//! through `TranscriptResolver::peek_prefix` (a `#[doc(hidden)] pub`,
//! TEST-ONLY seam per that method's own doc — re-fetching through it here,
//! after already having the answer, would only add a theoretical race
//! against the shared LRU evicting the just-written entry before the
//! second lookup, for no benefit). Three siblings forked at the same
//! `(parent, at_seq)` each trigger the same cache key, so all three
//! `resolve` calls return `Arc::clone`s of the identical backing
//! allocation (`Arc::ptr_eq`) — sibling sharing falls out of
//! `conway-session`'s own memoization, with no second cache added here
//! (per this item's binding notes). Tests assert that sharing via
//! `peek_prefix` directly (a legitimate test-only use of the seam); this
//! module's own production path never calls it.
//!
//! ## `InheritedPrefix::from` at fork depth >= 2
//!
//! A grandchild's (or deeper descendant's) `InheritedPrefix.records` is the
//! WHOLE effective transcript up to the fork point (GP-02) — the root's
//! own records, then every intermediate ancestor's own records in turn, up
//! to and including the immediate parent's — concatenated in order, per
//! `TranscriptResolver`'s "local units everywhere, the inherited prefix
//! always flows through in full" contract (that module's own docs). The
//! bundle is nonetheless stamped with a SINGLE `InheritedPrefix.from`: the
//! immediate parent's session id. That field means "who handed me this
//! context" — not "who originally authored each record" — and
//! `ContextBuilder` (`context/builder.rs`) carries that same single `from`
//! onto every `Provenance::Inherited` segment it produces from `records`,
//! regardless of which ancestor a given record actually originated in.
//! This is a deliberate, coordinator-ruled semantic (WI-084 rework), not an
//! oversight: recovering true per-record authorship at arbitrary depth
//! would require per-record session tracking that does not exist upstream
//! — neither `conway_core::log::LogRecord` nor `conway_session`'s resolver
//! carries an originating-session field per record — which is out of this
//! item's scope. It is queued as a refinement question rather than
//! attempted here.
//!
//! Once resolved, the `InheritedPrefix` is stored once on the child's
//! `AgentLoop` (`agent_loop::AgentLoop::inherited`) and never recomputed —
//! see that field's own doc for why later parent appends can never change
//! it (the fork is a snapshot; `conway-session`'s `fork.rs` enforces this by
//! construction, and `conway-session`'s memoized cache entries are
//! themselves immutable once written).
//!
//! ## `RuntimeError::InvalidSpec` does not exist
//!
//! This item's own acceptance notes cite `RuntimeError::InvalidSpec` for
//! rejected specs. `conway_core::error::RuntimeError` is `#[non_exhaustive]`
//! and, per its committed definition, has no such variant (out of this
//! crate's scope to add one) — see [`invalid_spec`] for the mapping this
//! item uses instead, following the same "closest fit" convention
//! `runtime.rs`'s (now-removed) `NoSubagentHost` stub and `tree.rs`'s
//! `already_attached` already established.
//!
//! Relatedly, the spec's "every child has a budget, by construction"
//! criterion describes a runtime check this item cannot perform: committed
//! `SubagentSpec::budget` is a non-`Option<Budget>` `Budget` value, and
//! `Budget::max_steps` is a required `u32` (default 40) with no "unset"
//! sentinel — there is no way for a spec to arrive here with an absent
//! budget or an absent `max_steps`. The property holds vacuously, by the
//! type, rather than by a runtime check added here.
//!
//! ## `steer` is not implemented by this item
//!
//! Real mailbox delivery (`AgentHandle`'s inbox, turn-boundary drain) is
//! WI-085's job; no criterion in this item exercises `steer`. Rather than
//! invent undocumented behavior ahead of that item's design, `steer`
//! returns a typed "not yet available" error, mirroring the same pattern
//! this crate already uses elsewhere for a real gap (`runtime.rs`'s
//! removed `NoSubagentHost` did the same for every method before this item
//! gave four of the five a real implementation).
//!
//! ## `CacheMode` is not wired from `SubagentSpec::cache_hint`
//!
//! `SubagentSpec::cache_hint` is documented as "never correctness-bearing"
//! and meaningful only as a *hint*. No criterion in this item requires a
//! particular `CacheMode` selection, and no mechanism anywhere in this
//! crate yet selects a concrete `CacheMode` from caller intent — even
//! `runtime.rs`'s `start_root` hardcodes `CacheMode::None` for every root
//! agent. This item does the same for fork/spawn children, for the same
//! reason: inventing a selection policy here, with no criterion pinning its
//! shape, would be scope creep this crate has no mandate for yet.

use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{AgentResult, AgentTreeSnapshot, SubagentMode, SubagentSpec};
use conway_core::capabilities::CacheMode;
use conway_core::config::DEFAULT_MAX_PARALLEL_TOOLS;
use conway_core::error::{ConwayError, RuntimeError, ToolError};
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SeqRange, SessionId};
use conway_core::log::{ForkOrigin, LogRecord, SessionMeta, SessionStatus};
use conway_core::ports::SubagentHost;
use conway_core::provenance::Provenance;
use conway_core::segment::CacheTtl;

use crate::agent_loop::{AgentLoop, AgentSpec};
use crate::context::{InheritedPrefix, SystemPromptSpec};
use crate::runtime::Runtime;
use crate::tree::AgentNode;

#[async_trait]
impl SubagentHost for Runtime {
    /// Fork or spawn `spec` under `parent`, per architecture §5.1/§5.2:
    ///
    /// 1. Validate `spec` (mode/`agent_def` pairing — see the module doc on
    ///    why nothing further is checked).
    /// 2. Resolve `parent`'s session and its current head (`at_seq`), the
    ///    freeze point.
    /// 3. Fork: `store.fork(parent_session, at_seq, meta)` (exactly once,
    ///    copies zero records) then resolve the `InheritedPrefix` (see the
    ///    module doc). Spawn: `store.create(meta)`, recording
    ///    `ForkOrigin{parent, at_seq, mode: Spawn}` in the header purely so
    ///    the tree is reconstructible from headers alone — context
    ///    assembly ignores it (`inherited` stays `None`; `AgentLoop` always
    ///    reads a session's *own* records straight from the store,
    ///    regardless of what its header's `origin` says).
    /// 4. Append the head record: `LogRecord::ForkDirective` (fork) or
    ///    `LogRecord::UserTurn` (spawn) — `agent_loop::split_head` (WI-081,
    ///    unmodified) already turns either into the right `HeadSegment`.
    /// 5. Attach to the tree (`Runtime::launch_agent` -> `AgentTree::attach`
    ///    emits `Event::AgentSpawned` for us — see the module doc's carried
    ///    note on why this code must not emit it a second time) and launch
    ///    the child's `AgentLoop` under the supervisor.
    async fn start(&self, parent: AgentId, spec: SubagentSpec) -> Result<AgentId, RuntimeError> {
        spec.validate().map_err(invalid_spec)?;

        let parent_session = self.agent_session(parent)?;
        let parent_meta = self.loop_deps().store.meta(&parent_session).await?;
        let at_seq = self.loop_deps().store.head(&parent_session).await?;

        let agent_id = AgentId::new();
        let mut agent_path = self.tree_ref().path(parent);
        agent_path.push(agent_id);

        let agent_def = spec
            .agent_def
            .as_ref()
            .and_then(|r| self.agent_defs().get(r.0.as_str()));
        let role = spec
            .role
            .clone()
            .or_else(|| agent_def.and_then(|d| d.role.clone()))
            .unwrap_or_else(|| RoleAlias::new("default"));
        let system_prompt = agent_def.map(|d| SystemPromptSpec {
            agent_def: d.name.clone(),
            text: d.system_prompt.clone(),
        });
        let tools = spec
            .tools
            .clone()
            .or_else(|| agent_def.map(|d| d.tools.clone()));
        let pin = agent_def.and_then(|d| d.model.clone());

        let now = Utc::now();
        let mut meta = SessionMeta {
            id: SessionId::new(),
            agent_id,
            origin: None,
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role.clone()),
            created: now,
            cwd: parent_meta.cwd.clone(),
            labels: Vec::new(),
            status: SessionStatus::Active,
        };

        let (session_id, inherited, inherited_upto) = match spec.mode {
            SubagentMode::Fork => {
                // `meta.origin` is left `None`: `store.fork` sets it itself
                // from its own `parent`/`at` arguments (defaulting `mode` to
                // `Fork` when the caller's `meta.origin` was `None`) — see
                // `conway-session`'s `fork.rs`.
                let sid = self
                    .loop_deps()
                    .store
                    .fork(&parent_session, at_seq, meta)
                    .await?;

                // Resolving the (still record-empty) child forces
                // `(parent_session, at_seq)` into the resolver's cache and
                // hands the result straight back as `resolve`'s own return
                // value (`Arc::clone`, not a fresh allocation — see the
                // module doc): the child's own zero records mean the
                // resolver's `level_upto == LogSeq::ZERO` short-circuit
                // returns exactly the parent's memoized prefix. No
                // re-fetch through the `#[doc(hidden)]` `peek_prefix` test
                // seam is needed (or wanted — see the module doc for why a
                // second cache lookup here could theoretically race an LRU
                // eviction under concurrent forks).
                let records = self
                    .resolver()
                    .resolve(self.loop_deps().store.as_ref(), &sid)
                    .await?;
                let inherited = InheritedPrefix {
                    from: parent_session,
                    seq_range: SeqRange::new(LogSeq::ZERO, Some(at_seq)),
                    records,
                };
                (sid, Some(inherited), Some(at_seq))
            }
            SubagentMode::Spawn => {
                // Recorded for tree reconstructability only (see the
                // module doc) — context assembly never reads it.
                meta.origin = Some(ForkOrigin {
                    parent: parent_session,
                    at_seq,
                    mode: SubagentMode::Spawn,
                });
                let sid = self.loop_deps().store.create(meta).await?;
                (sid, None, None)
            }
        };

        let head_record = match spec.mode {
            SubagentMode::Fork => LogRecord::ForkDirective {
                seq: LogSeq::ZERO,
                ts: now,
                text: spec.prompt.clone(),
                by: parent,
                prov: Provenance::ForkDirective { by: parent },
            },
            SubagentMode::Spawn => LogRecord::UserTurn {
                seq: LogSeq::ZERO,
                ts: now,
                text: spec.prompt.clone(),
                prov: Provenance::UserPrompt,
            },
        };
        self.loop_deps()
            .store
            .append(&session_id, head_record)
            .await?;

        let cancel = self.tree_ref().child_cancel_token(parent)?;
        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills: Vec::new(),
            tools,
            role: role.clone(),
            pin,
            budget: spec.budget.clone(),
            cache_mode: CacheMode::None,
            cache_ttl: CacheTtl::FiveMinutes,
            headroom_override: None,
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            report_slot: Some(last_report.clone()),
        };

        let agent_loop = AgentLoop {
            agent_id,
            session: session_id,
            parent: Some(parent),
            agent_path,
            cwd: parent_meta.cwd,
            deps: self.loop_deps().clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
            inherited,
        };

        let node = AgentNode {
            id: agent_id,
            parent: Some(parent),
            session: session_id,
            kind: Some(spec.mode),
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role),
            budget: spec.budget,
            cancel,
            inherited_upto,
        };

        self.launch_agent(node, agent_loop, last_report)?;
        Ok(agent_id)
    }

    /// Not yet available — real mailbox delivery is WI-085's job. See the
    /// module doc.
    async fn steer(&self, _target: AgentId, _text: String) -> Result<(), RuntimeError> {
        Err(steering_unavailable())
    }

    /// Delegates to [`crate::tree::AgentTree::await_result`], which already
    /// provides every guarantee this method needs (unknown agent ->
    /// `AgentNotFound`; a finished agent's result returned immediately; no
    /// tree lock held across the await).
    async fn await_result(&self, target: AgentId) -> Result<AgentResult, RuntimeError> {
        self.tree_ref().await_result(target).await
    }

    /// Delegates to the existing `Runtime::cancel` (WI-082/083), whose
    /// signature already matches this trait method exactly.
    async fn cancel(&self, target: AgentId, reason: String) -> Result<(), RuntimeError> {
        Runtime::cancel(self, target, reason)
    }

    /// Delegates to the existing `Runtime::tree` (WI-082/083).
    fn tree(&self) -> AgentTreeSnapshot {
        Runtime::tree(self)
    }
}

/// `RuntimeError` has no `InvalidSpec` variant — see the module doc.
/// `SubagentSpec::validate()`'s own error type is `ConwayError::Config`;
/// this maps it to `RuntimeError::Tool(ToolError::Internal{..})`, the same
/// "closest fit" fallback already established elsewhere in this crate for
/// gaps shaped like this one.
fn invalid_spec(err: ConwayError) -> RuntimeError {
    RuntimeError::Tool(ToolError::Internal {
        detail: format!("invalid SubagentSpec: {err}"),
    })
}

fn steering_unavailable() -> RuntimeError {
    RuntimeError::Tool(ToolError::Internal {
        detail: "steering is unavailable until WI-085 implements mailbox delivery".to_string(),
    })
}

/// A thin, non-owning delegate to `Runtime`'s real `SubagentHost` impl
/// (above), used only to break the `Runtime -> LoopDeps -> subagents`
/// reference cycle a literal `Arc<Runtime>` in `LoopDeps::subagents` would
/// create: `Runtime::new` must return the very `Arc<Runtime>` it also hands
/// every agent task (via `LoopDeps`) for tool dispatch, and storing a
/// *strong* copy of that same `Arc` inside the `Arc<LoopDeps>` every one of
/// those tasks also holds would mean `Runtime` never drops, even once every
/// external handle and every agent task is gone.
///
/// `Runtime::new` builds this from the `Weak<Runtime>` `Arc::new_cyclic`
/// hands its constructor closure (see that method's doc), so `upgrade`
/// fails only once every strong `Arc<Runtime>` — including the runtime's
/// own agent tasks' clones — has already been dropped: a runtime that is
/// still doing anything at all still upgrades successfully.
///
/// Deliberately not `pub`: `impl SubagentHost for Runtime` above is this
/// crate's one true implementation (satisfying the criterion that it has
/// "no additional public methods"); this type is construction plumbing, not
/// part of the crate's public surface.
pub(crate) struct WeakRuntimeHost(Weak<Runtime>);

impl WeakRuntimeHost {
    pub(crate) fn new(runtime: Weak<Runtime>) -> Self {
        Self(runtime)
    }

    fn upgrade(&self) -> Result<Arc<Runtime>, RuntimeError> {
        self.0.upgrade().ok_or_else(|| {
            RuntimeError::Tool(ToolError::Internal {
                detail: "subagent host unavailable: the runtime has already been dropped"
                    .to_string(),
            })
        })
    }
}

#[async_trait]
impl SubagentHost for WeakRuntimeHost {
    async fn start(&self, parent: AgentId, spec: SubagentSpec) -> Result<AgentId, RuntimeError> {
        self.upgrade()?.start(parent, spec).await
    }

    async fn steer(&self, target: AgentId, text: String) -> Result<(), RuntimeError> {
        self.upgrade()?.steer(target, text).await
    }

    async fn await_result(&self, target: AgentId) -> Result<AgentResult, RuntimeError> {
        self.upgrade()?.await_result(target).await
    }

    async fn cancel(&self, target: AgentId, reason: String) -> Result<(), RuntimeError> {
        // `Runtime` has its own inherent, sync `cancel` method (WI-082/083)
        // that method resolution prefers over this trait method of the
        // same name -- fully qualified syntax forces the trait impl above.
        SubagentHost::cancel(&*self.upgrade()?, target, reason).await
    }

    fn tree(&self) -> AgentTreeSnapshot {
        match self.upgrade() {
            Ok(runtime) => SubagentHost::tree(&*runtime),
            // Mirrors `runtime.rs`'s (now-removed) `NoSubagentHost::tree`
            // fallback shape for the one case where there is genuinely no
            // runtime left to ask.
            Err(_) => AgentTreeSnapshot {
                root: AgentId::default(),
                nodes: Vec::new(),
                at: Utc::now(),
            },
        }
    }
}

//! `Conway`: the live, assembled facade over one `conway-runtime::Runtime`
//! (WI-100). Constructed exclusively via `crate::builder::ConwayBuilder::build`.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use conway_core::agent::{AgentDefRef, Budget};
use conway_core::capabilities::RequiredCaps;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SessionId};
use conway_core::log::{SessionFilter, SessionMeta};
use conway_core::ports::SessionStore;
use conway_core::routing::RouteRequest;
use conway_routing::{DeclarativeRouter, ExplainReport, RoutingExplain};
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime};

use crate::config::{ConfigWarning, ConwayConfig};
use crate::error::{ConwayError, Result};
use crate::session_handle::{SessionHandle, SessionSpec};
use crate::subagent_spec::ForkSpec;

/// The live, assembled facade: one `Runtime`, its resolved config, and (when
/// the builder compiled its own router rather than receiving an injected
/// one) the concrete router `explain_routing` projects through.
///
/// Cheap to `Clone`: every field is an `Arc`.
#[derive(Clone)]
pub struct Conway {
    rt: Arc<Runtime>,
    config: Arc<ConwayConfig>,
    // Also cloned into every `SessionHandle` this `Conway` mints
    // (`new_session`), which needs it for `SessionHandle::transcript`'s
    // ancestry walk (WI-101).
    store: Arc<dyn SessionStore>,
    router_explain: Option<Arc<DeclarativeRouter>>,
    warnings: Arc<Vec<ConfigWarning>>,
}

impl Conway {
    pub(crate) fn new(
        rt: Arc<Runtime>,
        config: ConwayConfig,
        store: Arc<dyn SessionStore>,
        router_explain: Option<Arc<DeclarativeRouter>>,
        warnings: Vec<ConfigWarning>,
    ) -> Self {
        Self {
            rt,
            config: Arc::new(config),
            store,
            router_explain,
            warnings: Arc::new(warnings),
        }
    }

    /// Non-fatal warnings surfaced by `config::load` (currently only
    /// headroom-vs-context-window warnings). Empty when this `Conway` was
    /// built via `ConwayBuilder::from_parts`, which bypasses `load` entirely.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// The resolved configuration this `Conway` was built from.
    pub fn config(&self) -> &ConwayConfig {
        &self.config
    }

    /// Creates a new session and starts its root agent.
    ///
    /// `spec`'s `None`/empty fields are resolved from `self.config` here, at
    /// call time rather than at builder time, so one `Conway` can serve
    /// differently-configured sessions (`SessionSpec::default()` itself is
    /// config-agnostic; see that type's own doc).
    ///
    /// **Reconciliation (disclosed):** the binding implementation notes
    /// describe this method's sequence as "build `SessionMeta` ->
    /// `store.create` -> `Runtime::start_root`", but the already-committed
    /// `Runtime::start_root` (WI-082) already builds the `SessionMeta` and
    /// calls `SessionStore::create` internally. Calling `store.create` again
    /// here with the same id would double-create the session (an error
    /// against both `FakeStore` and `JsonlSessionStore`, which reject a
    /// duplicate id). Instead, this method generates the `SessionId` itself
    /// and passes it through `RootSpec::session`, so `start_root`'s own
    /// internal `store.create` call is the single, authoritative creation --
    /// this still satisfies "creates a session via `SessionStore::create`",
    /// it just avoids invoking that call a second time.
    ///
    /// **Caller-chosen id (WI-119):** `spec.id` -- when `Some` -- is passed
    /// through unchanged instead of the freshly minted `SessionId` below.
    /// `RootSpec::session` (WI-082) already supports this at the runtime
    /// layer; this is the facade-side wiring to reach it. An id already
    /// present in the store surfaces as `start_root`'s own
    /// `SessionStore::create` failure --
    /// `Err(ConwayError::Runtime(RuntimeError::Store(StoreError::AlreadyExists
    /// { .. })))`, propagated unchanged through the `?` below -- typed and
    /// distinct from every other failure this method can produce, not a
    /// generic error.
    ///
    /// **Disclosed gap:** `RootSpec` (`conway-runtime`, WI-082) has no field
    /// for `SessionSpec::labels` or for `config.limits.max_parallel_tools`,
    /// so neither reaches the created session/agent through this method --
    /// out of this item's file scope to add.
    pub async fn new_session(&self, spec: SessionSpec) -> Result<SessionHandle> {
        let role = spec
            .role
            .unwrap_or_else(|| self.config.default_role.clone());
        let cwd = spec.cwd.unwrap_or_else(|| self.config.cwd.clone());
        let budget = spec.budget.unwrap_or_else(|| self.default_budget());
        let agent_def = spec.agent_def.map(AgentDefRef);

        let session = spec.id.unwrap_or_default();
        let root_spec = RootSpec {
            session: Some(session),
            agent_def,
            role: Some(role),
            tools: None,
            budget,
            cwd,
            prompt: None,
        };
        let root = self.rt.start_root(root_spec).await?;

        Ok(SessionHandle::new(
            self.rt.clone(),
            session,
            root,
            self.store.clone(),
        ))
    }

    /// `config.limits` resolved into a `Budget`. `max_tool_calls` has no
    /// facade config counterpart and is always `None`.
    fn default_budget(&self) -> Budget {
        let limits = &self.config.limits;
        Budget {
            max_steps: limits.max_steps,
            deadline: if limits.deadline_secs == 0 {
                None
            } else {
                Some(Utc::now() + ChronoDuration::seconds(limits.deadline_secs as i64))
            },
            max_tokens: if limits.max_tokens == 0 {
                None
            } else {
                Some(limits.max_tokens)
            },
            max_tool_calls: None,
        }
    }

    /// The "why did this model run, and why not the others" report for
    /// `role`, projected through the concrete `DeclarativeRouter` this
    /// `Conway` compiled itself.
    ///
    /// When the builder instead received an injected `Router`
    /// (`ConwayBuilder::with_router`), there is no concrete
    /// `DeclarativeRouter` to project through -- `conway_routing::RoutingExplain`
    /// is defined over that concrete type, not the `Router` trait object --
    /// so this returns a degraded, empty report (`entries: vec![]`,
    /// `headroom_tokens: 0`), mirroring `RoutingExplain::explain`'s own
    /// fallback for an unrecognized role.
    pub fn explain_routing(&self, role: &RoleAlias) -> ExplainReport {
        let req = RouteRequest {
            role: role.clone(),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 0,
            agent_id: AgentId::new(),
        };
        match &self.router_explain {
            Some(router) => RoutingExplain::new(router).explain(&req),
            None => ExplainReport {
                role: role.clone(),
                pin: None,
                est_tokens: 0,
                required: RequiredCaps::default(),
                headroom_tokens: 0,
                entries: Vec::new(),
                generated_at: Utc::now(),
            },
        }
    }

    /// Reattaches to a persisted session (WI-103), now as a DRIVABLE handle
    /// (WI-119).
    ///
    /// **Resolved (WI-119):** this method's previous doc disclosed a real
    /// gap -- `conway-runtime` exposed only `start_root`, which cannot be
    /// repurposed for resume (it unconditionally `store.create`s, which
    /// every committed `SessionStore` rejects for an id that already has a
    /// persisted session). WI-118 closed that gap by adding
    /// `Runtime::resume_root(ResumeSpec)`: it reads the existing
    /// `SessionMeta` via `store.meta` (no `store.create`), re-registers
    /// `meta.agent_id` into `Runtime`'s `agents` map and `AgentTree` through
    /// the same `launch_agent` path `start_root` uses, and gates the
    /// resumed `AgentLoop`'s first iteration behind a `ResumeGate` so it
    /// idles until this handle's own first `SessionHandle::prompt` call --
    /// never racing the (already-completed) persisted transcript. This
    /// method now calls it directly, which resolves both criteria the
    /// pre-WI-118 doc could not satisfy:
    /// - `prompt()` after resume: `Runtime::prompt` now finds `agent` in
    ///   `Runtime.agents` (registered by `resume_root` below), so it appends
    ///   and wakes the gated loop instead of returning `AgentNotFound`.
    /// - `tree()`: `resume_root` attaches the resumed root to `AgentTree`,
    ///   so `SessionHandle::tree()` (`self.rt.tree()`, WI-101, unchanged)
    ///   now reflects it. **Still disclosed, not silently dropped:**
    ///   `resume_root`'s own doc is explicit that it re-attaches only the
    ///   resumed *root* -- past fork/spawn children are not re-attached as
    ///   live `AgentTree` nodes (their tasks are gone; a live-looking node
    ///   with nothing to ever finish it would misrepresent their status
    ///   worse than omitting them). Their history remains fully readable via
    ///   `transcript`/`context_report_at`, just not via `tree()`.
    ///
    /// Every property the old, store-only implementation already delivered
    /// is preserved: `id()`/`root()` still read from the persisted
    /// `SessionMeta` (`resume_root` returns exactly `meta.agent_id`, never a
    /// freshly minted id); `transcript(root)` still reads purely through
    /// `SessionStore`, unaffected by live registration; a truncated trailing
    /// line is still repaired transparently by `JsonlSessionStore` on first
    /// file access (the same `store.meta` call `resume_root` makes
    /// internally), so `resume` still succeeds on such a session without any
    /// special-casing here. The warning-forwarding gap this method's
    /// previous doc disclosed (`SessionStore`'s ports carry no "a repair
    /// just happened" signal to surface as `Event::Error{fatal: false}`)
    /// still stands, for the same reason -- `conway-session`'s repair path
    /// only `tracing::warn!`s, with nothing threaded back through any
    /// `Result`.
    ///
    /// `agent_def`/`role`/`cwd` are all left `None` in the `ResumeSpec`
    /// below, so `resume_root` falls back to the persisted `SessionMeta`'s
    /// own values -- this method has no override surface for them (matching
    /// `resume`'s existing binding signature, which takes only `sid`); a
    /// caller that needs an override can add one to `ResumeSpec` through a
    /// future item without breaking this one's contract.
    ///
    /// **Error-shape preservation (disclosed):** `resume_root`'s own error
    /// for an unknown/missing session is `RuntimeError::Store` (its internal
    /// `store.meta` lookup, converted via that type's own `#[from]
    /// StoreError`) -- a plain `?` here would surface it as
    /// `ConwayError::Runtime(RuntimeError::Store(_))`, one layer deeper than
    /// this method returned pre-WI-119 (`ConwayError::Store(_)` directly, from
    /// this method's own former `store.meta` call). `resume`'s existing test
    /// suite asserts the flat shape, and nothing about resuming a session
    /// makes "the store doesn't have it" a *runtime* concern rather than a
    /// *store* one -- so this unwraps `RuntimeError::Store` back to
    /// `ConwayError::Store` explicitly, keeping every other `RuntimeError`
    /// variant (e.g. a future `resume_root` failure mode) under
    /// `ConwayError::Runtime` unchanged.
    pub async fn resume(&self, sid: SessionId) -> Result<SessionHandle> {
        let agent = self
            .rt
            .resume_root(ResumeSpec {
                session: sid,
                agent_def: None,
                role: None,
                tools: None,
                budget: self.default_budget(),
                cwd: None,
            })
            .await
            .map_err(|err| match err {
                RuntimeError::Store(inner) => ConwayError::Store(inner),
                other => ConwayError::Runtime(other),
            })?;
        Ok(SessionHandle::new(
            self.rt.clone(),
            sid,
            agent,
            self.store.clone(),
        ))
    }

    /// Enumerates persisted sessions via `SessionStore::list`, returned
    /// unmodified -- no facade-side re-filtering, re-ordering, or paging
    /// beyond what `filter` itself already expresses.
    pub async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>> {
        Ok(self.store.list(filter).await?)
    }

    /// A session's own local record count -- `SessionStore::head`, the same
    /// value [`Conway::fork_from`]'s own bounds check compares `at` against.
    ///
    /// Distinct from [`SessionHandle::transcript`](crate::SessionHandle::transcript)'s
    /// length: `transcript` returns the *effective, ancestry-resolved* view
    /// (inherited prefix + this session's own records), which overcounts the
    /// local head for any session that is itself a fork child. Callers that
    /// need "this session's current head, as `fork_from` itself sees it" --
    /// e.g. `conway-cli`'s `--fork-from <ref>` with no `@seq`, which must
    /// compute "fork this branch at its current head" -- need this method,
    /// not `transcript().len()`.
    pub async fn session_head(&self, sid: SessionId) -> Result<LogSeq> {
        Ok(self.store.head(&sid).await?)
    }

    /// Forks a *stored* session at an arbitrary point, offline -- no live
    /// parent agent is involved, and `SessionStore::fork`'s O(1)-by-
    /// reference contract (architecture §5.1/§8, D-11's local-unit `at_seq`)
    /// means zero parent records are copied.
    ///
    /// Distinct from [`SessionHandle::fork`](crate::SessionHandle::fork),
    /// which forks a *live* agent at its current head through
    /// `SubagentHost` -- this item's binding notes name that contrast as
    /// "the most likely point of confusion in the public API": both
    /// ultimately call `SessionStore::fork`, but only `SessionHandle::fork`
    /// goes through the runtime's subagent machinery (and so also spawns a
    /// live agent task); this method only creates the child's session file.
    ///
    /// Reuses [`ForkSpec`] (WI-102) rather than a parallel type, per the
    /// binding notes. `directive`/`cache_hint`/`result_contract` still have
    /// no session-level counterpart -- `conway_core::log::SessionMeta`
    /// carries none of them, and there is no live child turn here to attach
    /// a `LogRecord::ForkDirective` to (the child session is *created* with
    /// zero records, store-side, exactly as before) -- so only `agent_def`
    /// and `role` are consulted for the persisted `SessionMeta`, as
    /// overrides onto the parent's own values.
    ///
    /// **Live registration (WI-119):** after the store-side fork below, this
    /// method now also calls `Runtime::resume_root` over the freshly created
    /// child session -- the same mechanism [`Conway::resume`] uses -- so the
    /// returned handle is DRIVABLE: `prompt` on it succeeds (verified by
    /// `fork_from_returns_a_drivable_child_whose_prompt_succeeds`).
    /// `resume_root`'s `ResumeGate` (WI-118) means the child idles until
    /// *this* handle's own first `prompt` call, exactly like a resumed root
    /// -- it does not run a turn as a side effect of `fork_from` itself.
    /// `spec.tools`/`spec.budget` -- otherwise unused by the store-side fork
    /// -- ARE consulted here: they configure the live agent's `AgentSpec`
    /// (`ResumeSpec.tools`/`.budget`), the same role `ForkSpec` plays for
    /// [`SessionHandle::fork`](crate::SessionHandle::fork)'s live path.
    /// `agent_def`/`role`/`cwd` are left `None` in the `ResumeSpec` --
    /// `resume_root` falls back to `child_meta`'s own already-resolved
    /// values (set from `spec`/`parent_meta` just above), so there is no
    /// need to re-derive them a second time.
    ///
    /// **Inherited prefix, resolved (WI-119 gap closed):** this criterion
    /// also asks for "the child's context contains the inherited prefix" --
    /// previously disclosed here as NOT satisfied, since `Runtime::
    /// resume_root` (WI-118) always constructed its `AgentLoop` with
    /// `inherited: None`, correct only for a genuine root (whose own session
    /// records ARE its complete history), not for a fork child (whose own
    /// records are, by the zero-copy contract this method preserves, empty
    /// or a small tail). `resume_root` (`conway-runtime`) now detects a
    /// fork-child session via its persisted `SessionMeta::origin` and
    /// resolves the parent's prefix at `origin.at_seq` through
    /// `conway_session::TranscriptResolver::resolve_prefix` (made `pub` for
    /// this) -- the exact primitive `subagent.rs`'s live-fork path already
    /// bottoms out on, so there is one shared implementation of the D-11
    /// ancestry walk, not two. This works for `fork_from`'s arbitrary,
    /// possibly-earlier `at` (unlike substituting `subagent.rs`'s own
    /// current-head-only fork path, which was ruled out for exactly that
    /// reason) because it resolves directly against `(parent, at_seq)`
    /// rather than reusing `subagent.rs`'s "resolve the freshly-forked
    /// child" shortcut. No change was needed in this method itself: it
    /// already called `resume_root`, which now does the right thing.
    ///
    /// **Sibling-tool note (disclosed):** `resume_root`'s own doc covers the
    /// case a resumed fork child has since accumulated its own turns (the
    /// resolved prefix excludes them, so `AgentLoop`'s separate own-records
    /// read is never double-counted) -- see that method's doc for the full
    /// mechanism.
    ///
    /// **Defense-in-depth bounds check (disclosed):** `SessionStore::fork`'s
    /// own committed implementation (`conway-session`'s `fork_impl`) already
    /// rejects `at > head` with `StoreError::SeqOutOfRange{ requested, head
    /// }` -- but `conway_core::fakes::FakeStore` (a `SessionStore` impl this
    /// crate depends on but does not own; out of this item's file scope to
    /// change) does not enforce that bound. Rather than let this method's
    /// behavior depend on which `SessionStore` backs a given `Conway`, the
    /// bound is checked here too, against the same error shape, so the
    /// criterion holds under every `SessionStore` implementation.
    ///
    /// **Shared with `SessionHandle::ask` (disclosed refactor):** the
    /// `store.fork` -> `rt.resume_root` sequence below used to live inline
    /// here. It now delegates to `crate::fork_child::fork_child`, the same
    /// helper the `/ask` fork-ask flow's `SessionHandle::ask` calls with
    /// `ephemeral: true` -- this method always passes `ephemeral: false`, so
    /// a session created through this method is never catalog-hidden. See
    /// that module's doc for why the sequence is factored rather than
    /// duplicated.
    pub async fn fork_from(
        &self,
        sid: SessionId,
        at: LogSeq,
        spec: ForkSpec,
    ) -> Result<SessionHandle> {
        let parent_meta = self.store.meta(&sid).await?;
        let head = self.store.head(&sid).await?;
        if at.0 > head.0 {
            return Err(ConwayError::Store(StoreError::SeqOutOfRange {
                requested: at,
                head,
            }));
        }

        crate::fork_child::fork_child(
            &self.rt,
            &self.store,
            sid,
            parent_meta,
            at,
            crate::fork_child::ForkChildRequest {
                agent_def: spec.agent_def,
                role: spec.role,
                tools: spec.tools,
                budget: spec.budget,
                ephemeral: false,
            },
        )
        .await
    }
}

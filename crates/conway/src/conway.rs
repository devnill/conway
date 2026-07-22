//! `Conway`: the live, assembled facade over one `conway-runtime::Runtime`
//! (WI-100). Constructed exclusively via `crate::builder::ConwayBuilder::build`.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use conway_core::agent::{AgentDefRef, Budget};
use conway_core::capabilities::RequiredCaps;
use conway_core::error::StoreError;
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SessionId};
use conway_core::log::{SessionFilter, SessionMeta, SessionStatus};
use conway_core::ports::SessionStore;
use conway_core::routing::RouteRequest;
use conway_routing::{DeclarativeRouter, ExplainReport, RoutingExplain};
use conway_runtime::runtime::{RootSpec, Runtime};

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

        let session = SessionId::new();
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

    /// Reattaches to a persisted session (WI-103).
    ///
    /// **Disclosed gap, flagged rather than worked around:** the binding
    /// implementation notes describe this method's sequence as ending in
    /// `Runtime::resume_root(ResumeSpec{ session, meta })`, with an explicit
    /// fallback instruction for exactly this situation: "if `conway-runtime`
    /// exposes only `start_root`, flag the gap to the architect rather than
    /// reconstructing agent state in the facade." Grepping `conway-runtime`
    /// confirms `Runtime`'s only session-starting method is `start_root` --
    /// there is no `resume_root`, no `attach`, and no other public method
    /// that registers an agent in `Runtime`'s in-memory `agents` map or
    /// `AgentTree`. `start_root` cannot be repurposed for resume either: it
    /// unconditionally calls `SessionStore::create`, which every committed
    /// `SessionStore` impl (`JsonlSessionStore` via `create_new(true)`;
    /// `conway_core::fakes::FakeStore`) rejects with
    /// `StoreError::AlreadyExists` for a session id that already has a
    /// file/entry -- exactly this method's input. This item's own file
    /// scope (`## scope` in the work item) is `crates/conway/src/conway.rs`
    /// only, so adding a `Runtime`-level resume/registration path to
    /// `conway-runtime` is out of scope for this change.
    ///
    /// What this method delivers with the ports actually available today is
    /// everything reachable through `SessionStore` alone -- which covers
    /// every criterion whose verification does not require the *runtime* to
    /// know the agent is live:
    /// - `id()`/`root()`: read directly from the persisted `SessionMeta`.
    /// - `transcript(root)`: `SessionHandle::transcript` already reads only
    ///   through `SessionStore`, never through `Runtime`'s in-memory state,
    ///   so it is unaffected by the agent not being re-registered.
    /// - Tolerating a truncated trailing line: `JsonlSessionStore` repairs
    ///   this internally on first file access (`get_or_open_handle`), before
    ///   `meta`/`head`/`read` ever return to this method, so `resume`
    ///   succeeds on such a session without any special-casing here.
    ///
    /// **Two criteria this method cannot satisfy without that missing
    /// runtime capability** (disclosed here and in this item's Self-Check,
    /// not silently dropped):
    /// - `tree()` reconstruction: `SessionHandle::tree()` is
    ///   `self.rt.tree()` unconditionally (WI-101, also out of this item's
    ///   file scope to change), which reads `Runtime`'s own in-memory
    ///   `AgentTree` -- populated only by `launch_agent`/`start_root`. A
    ///   resumed session's agents were never attached in this process, so
    ///   `tree()` on a resumed handle reflects whatever this `Runtime`
    ///   happens to have live, not the persisted session's history.
    ///   Reconstructing an `AgentTreeSnapshot` locally in this file to paper
    ///   over that is exactly what the binding notes forbid ("the facade
    ///   must not build `AgentTreeSnapshot` itself -- it passes the child
    ///   `SessionMeta` list and the runtime owns the tree type").
    /// - `prompt()` after resume: `Runtime::prompt` looks `agent` up in its
    ///   own `agents: RwLock<HashMap<AgentId, AgentHandle>>`, which resume
    ///   never populates (no task is spawned) -- calling `prompt` on a
    ///   resumed handle returns `RuntimeError::AgentNotFound`, not a
    ///   successful append at `head + 1`.
    ///
    /// **Warning-forwarding gap (disclosed):** the binding notes also ask
    /// for the truncated-trailing-line repair to reach "the event stream as
    /// `Event::Error{fatal: false}`". `conway_core::ports::SessionStore`'s
    /// methods (`meta`/`head`/`read`) return no signal that a repair
    /// happened -- confirmed by reading `conway-session/src/store.rs`'s
    /// `get_or_open_handle`, whose repair path only calls `tracing::warn!`,
    /// with nothing threaded back through any `Result`. There is no data
    /// this method could forward to the event bus even if it tried; the
    /// truncation is still tolerated (the file is repaired and `resume`
    /// still returns `Ok`), just not observable at this layer today.
    pub async fn resume(&self, sid: SessionId) -> Result<SessionHandle> {
        let meta = self.store.meta(&sid).await?;
        Ok(SessionHandle::new(
            self.rt.clone(),
            sid,
            meta.agent_id,
            self.store.clone(),
        ))
    }

    /// Enumerates persisted sessions via `SessionStore::list`, returned
    /// unmodified -- no facade-side re-filtering, re-ordering, or paging
    /// beyond what `filter` itself already expresses.
    pub async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>> {
        Ok(self.store.list(filter).await?)
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
    /// binding notes. `directive`/`budget`/`cache_hint`/`tools`/
    /// `result_contract` have no session-level counterpart --
    /// `conway_core::log::SessionMeta` carries none of them, and there is no
    /// live child turn here to attach a `LogRecord::ForkDirective` to (the
    /// child session is created with zero records) -- so only `agent_def`
    /// and `role` are consulted, as overrides onto the parent's own values.
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

        let child_agent = AgentId::new();
        let child_meta = SessionMeta {
            id: SessionId::new(),
            agent_id: child_agent,
            // `SessionStore::fork` fills this in from `sid`/`at`/`mode`.
            origin: None,
            agent_def: spec.agent_def.or(parent_meta.agent_def),
            role: spec.role.or(parent_meta.role),
            created: Utc::now(),
            cwd: parent_meta.cwd,
            labels: Vec::new(),
            status: SessionStatus::Active,
        };
        let child = self.store.fork(&sid, at, child_meta).await?;

        Ok(SessionHandle::new(
            self.rt.clone(),
            child,
            child_agent,
            self.store.clone(),
        ))
    }
}

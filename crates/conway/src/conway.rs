//! `Conway`: the live, assembled facade over one `conway-runtime::Runtime`
//! (WI-100). Constructed exclusively via `crate::builder::ConwayBuilder::build`.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use conway_core::agent::{AgentDefRef, Budget};
use conway_core::capabilities::RequiredCaps;
use conway_core::ids::{AgentId, RoleAlias, SessionId};
use conway_core::ports::SessionStore;
use conway_core::routing::RouteRequest;
use conway_routing::{DeclarativeRouter, ExplainReport, RoutingExplain};
use conway_runtime::runtime::{RootSpec, Runtime};

use crate::config::{ConfigWarning, ConwayConfig};
use crate::error::Result;
use crate::session_handle::{SessionHandle, SessionSpec};

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
}

//! The "fork a session, then re-register it as a live, drivable agent"
//! sequence shared by [`crate::Conway::fork_from`] and
//! [`crate::SessionHandle::ask`] (the `/ask` fork-ask slice).
//!
//! Both callers need exactly the same two calls -- `SessionStore::fork`
//! (zero-copy, one header write) followed by `Runtime::resume_root`
//! (re-registers the child as a live agent, resolving its inherited prefix)
//! -- differing only in *which* fields of the child's `SessionMeta`/
//! `AgentSpec` they override. This module exists so that sequence, and in
//! particular the `ephemeral` bit each caller sets differently, lives in
//! exactly one place rather than two copies that could drift apart. It does
//! not re-implement any ancestry/inherited-prefix resolution itself -- that
//! logic lives entirely inside `Runtime::resume_root` (`conway-runtime`),
//! unchanged by this module.

use std::sync::Arc;

use chrono::Utc;
use conway_core::agent::{Budget, ToolSelector};
use conway_core::error::RuntimeError;
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SessionId};
use conway_core::log::{SessionMeta, SessionStatus};
use conway_core::ports::SessionStore;
use conway_runtime::runtime::{ResumeSpec, Runtime};

use crate::error::{ConwayError, Result};
use crate::session_handle::SessionHandle;

/// Overrides onto the parent's own `agent_def`/`role` (`None` inherits the
/// parent's value, the same fallback [`crate::Conway::fork_from`] used
/// before this module existed), plus the live child agent's `tools`/
/// `budget`, plus whether the child is born ephemeral.
pub(crate) struct ForkChildRequest {
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    pub ephemeral: bool,
}

/// Forks `parent` at `at` into a fresh, drivable child session.
///
/// Procedure: build the child's `SessionMeta` (overrides from `req`,
/// everything else inherited from `parent_meta`), `store.fork` it (a single
/// header write, zero parent records copied), then `rt.resume_root` it --
/// re-registering the freshly created session as a live root agent, exactly
/// as [`crate::Conway::resume`]/[`crate::Conway::fork_from`] already do.
/// `resume_root`'s own `ResumeGate` means the child idles until the
/// returned handle's first `prompt`/`ask`-driven turn -- this function never
/// runs a turn as a side effect of forking by itself.
pub(crate) async fn fork_child(
    rt: &Arc<Runtime>,
    store: &Arc<dyn SessionStore>,
    parent: SessionId,
    parent_meta: SessionMeta,
    at: LogSeq,
    req: ForkChildRequest,
) -> Result<SessionHandle> {
    let child_agent = AgentId::new();
    let child_meta = SessionMeta {
        id: SessionId::new(),
        agent_id: child_agent,
        // `SessionStore::fork` fills this in from `parent`/`at`/`mode`.
        origin: None,
        agent_def: req.agent_def.or(parent_meta.agent_def),
        role: req.role.or(parent_meta.role),
        created: Utc::now(),
        cwd: parent_meta.cwd,
        labels: Vec::new(),
        status: SessionStatus::Active,
        ephemeral: req.ephemeral,
    };
    let child = store.fork(&parent, at, child_meta).await?;

    let agent = rt
        .resume_root(ResumeSpec {
            session: child,
            agent_def: None,
            role: None,
            tools: req.tools,
            budget: req.budget,
            cwd: None,
        })
        .await
        .map_err(|err| match err {
            RuntimeError::Store(inner) => ConwayError::Store(inner),
            other => ConwayError::Runtime(other),
        })?;
    debug_assert_eq!(
        agent, child_agent,
        "resume_root must return SessionMeta::agent_id unchanged"
    );

    Ok(SessionHandle::new(rt.clone(), child, agent, store.clone()))
}

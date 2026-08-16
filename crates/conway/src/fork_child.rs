//! The "fork a session, then re-register it as a live, drivable agent"
//! sequence backing [`crate::Conway::fork_from`].
//!
//! `fork_from` needs exactly two calls -- `SessionStore::fork` (zero-copy,
//! one header write) followed by `Runtime::resume_root` (re-registers the
//! child as a live agent, resolving its inherited prefix at the caller's
//! arbitrary, possibly-earlier `at`) -- plus the overrides onto the child's
//! `SessionMeta`/`AgentSpec` `ForkSpec` carries. This module exists so that
//! sequence lives in exactly one place. It does not re-implement any
//! ancestry/inherited-prefix resolution itself -- that logic lives entirely
//! inside `Runtime::resume_root` (`conway-runtime`), unchanged by this
//! module.
//!
//! **Why not `SubagentHost::start`:** `subagent.rs`'s live-fork path forks
//! only at the parent's CURRENT head; `fork_from`'s whole point is forking a
//! persisted session at an arbitrary earlier seq (see `fork_from`'s own doc,
//! which ruled that substitution out for exactly that reason).
//!
//! **History (B2):** the `/ask` fork-ask flow
//! (`SessionHandle::ask`) used to share this module, passing
//! `ephemeral: true`. It no longer does: B2 moved `/ask` onto the runtime's
//! own attach path (`SubagentHost::start`) so the ephemeral child attaches
//! as a proper fork child of the asker (`kind: Fork`, `parent: Some(asker)`,
//! `AgentSpawned` emitted) instead of a `kind: None` root with no event.
//! `fork_from`'s child remains a first-class, NON-ephemeral catalog citizen
//! resumed as a root -- this module's only remaining caller -- so the
//! `ephemeral` knob the shared helper once carried is gone with the `/ask`
//! caller: `SessionMeta::ephemeral` is fixed `false` here.

use std::sync::Arc;

use chrono::Utc;
use conway_core::agent::{Budget, ToolSelector};
use conway_core::error::RuntimeError;
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SessionId};
use conway_core::log::SessionMeta;
use conway_core::ports::SessionStore;
use conway_runtime::runtime::{ResumeSpec, Runtime};

use crate::error::{ConwayError, Result};
use crate::session_handle::SessionHandle;

/// Overrides onto the parent's own `agent_def`/`role` (`None` inherits the
/// parent's value, the same fallback [`crate::Conway::fork_from`] used
/// before this module existed), plus the live child agent's `tools`/
/// `budget`/`result_contract`/`keep_alive`/`plugin_config`.
pub(crate) struct ForkChildRequest {
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    /// The live child agent's result contract -- threaded into
    /// `ResumeSpec::result_contract` below (board item
    /// `01M03FQDF33AZ8G258516EDWQD`, see that field's own doc for the gap
    /// this closes). `None` when [`crate::Conway::fork_from`]'s caller left
    /// [`crate::ForkSpec::result_contract`] unset -- the same "no contract"
    /// behavior every caller had before this field existed.
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// The live child agent's keep-alive flag -- threaded into
    /// `ResumeSpec::keep_alive` below (board item
    /// `01M03KZXR1KF77YRAW4W4GE6KK`, see that field's own doc for the gap
    /// this closes and the re-arming semantics that were settled before
    /// wiring). `false` when [`crate::Conway::fork_from`]'s caller left
    /// [`crate::ForkSpec::keep_alive`] at its default -- the same one-shot
    /// behavior every caller had before this field existed.
    pub keep_alive: bool,
    /// Per-agent plugin configuration this fork requests for the child --
    /// narrowing-only overrides for keys the owning plugin declares
    /// narrowable (`conway.fs`'s `conway.fs.root` is the proving consumer).
    /// `None` means "inherit the parent's own effective per-agent config
    /// unchanged" (the pre-this-field behavior). `Some` is re-validated --
    /// never carried verbatim -- via [`Runtime::narrow_plugin_config_for_fork`]
    /// below, which narrows the parent's PERSISTED config by the requested
    /// override against the CURRENTLY installed plugins' rules, refusing any
    /// widening (board item `01M03KZXR1KF77YRAW4W4GE6KK`, the third sibling
    /// of the `result_contract` bug; follows `01M0321414SVRD60HEP074AFHG`'s
    /// re-validate-not-carry discipline).
    pub plugin_config: Option<conway_core::ports::PluginConfig>,
}

/// Forks `parent` at `at` into a fresh, drivable child session.
///
/// Procedure: build the child's `SessionMeta` (overrides from `req`,
/// everything else inherited from `parent_meta`), `store.fork` it (a single
/// header write, zero parent records copied), then `rt.resume_root` it --
/// re-registering the freshly created session as a live root agent, exactly
/// as [`crate::Conway::resume`] already does. `resume_root`'s own
/// `ResumeGate` means the child idles until the returned handle's first
/// `prompt`-driven turn -- this function never runs a turn as a side effect
/// of forking by itself.
pub(crate) async fn fork_child(
    rt: &Arc<Runtime>,
    store: &Arc<dyn SessionStore>,
    parent: SessionId,
    parent_meta: SessionMeta,
    at: LogSeq,
    req: ForkChildRequest,
) -> Result<SessionHandle> {
    let child_agent = AgentId::new();
    // (board item `01M03KZXR1KF77YRAW4W4GE6KK`) The child's EFFECTIVE
    // per-agent plugin config: the parent's PERSISTED config narrowed by
    // `req.plugin_config` (the caller's `ForkSpec::plugin_config` request),
    // re-validated against the CURRENTLY installed plugins' narrowing rules
    // -- the facade-fork-path mirror of `SubagentHost::start`'s live-fork
    // narrowing in `conway-runtime`'s `subagent.rs`. `None` (the
    // `ForkSpec::new` default) means "inherit the parent's unchanged" via
    // `PluginConfig::narrow`'s own `requested: None` -> `self.clone()`
    // contract, so the pre-this-field behaviour is preserved byte-for-byte
    // when a caller leaves `ForkSpec::plugin_config` unset. A requested
    // value that would WIDEN the parent's config, or names a key no
    // installed plugin declares narrowable, FAILS THE FORK HERE with a
    // typed error -- never silently clamped and never silently honored, the
    // same "fail the whole operation, don't guess" discipline
    // `subagent.rs`'s own widening check already established. The child's
    // root is the parent's (a fork inherits it, `ForkSpec` has no `root`
    // field), so `derive_fs_root_config`'s derived `conway.fs.root` entry --
    // when `conway.fs` is installed -- matches what `child_meta.root` below
    // already says. `resume_root` (called just below) re-validates this
    // persisted value against the global config a SECOND time, the same
    // defence-in-depth it applies to every resumed session.
    let child_plugin_config = rt
        .narrow_plugin_config_for_fork(
            &parent_meta.plugin_config,
            req.plugin_config.as_ref(),
            parent_meta.root.as_deref(),
        )
        .map_err(|err| match err {
            RuntimeError::Store(inner) => ConwayError::Store(inner),
            other => ConwayError::Runtime(other),
        })?;
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
        // Never ephemeral: a `fork_from` child is a first-class catalog
        // citizen (see the module doc's B2 history note).
        ephemeral: false,
        // B5: not an `/ask` child either -- the tag exists only on ephemeral
        // ask children (stamped from `SubagentSpec::ask_origin` in
        // `conway-runtime`'s `SubagentHost::start`).
        ask_origin: None,
        // (S3) A fork always inherits the parent's root unchanged, never
        // overrides it (mirrors `cwd` immediately above) -- this is a fork,
        // not a spawn, so there is no narrowing/widening decision to make
        // here at all.
        root: parent_meta.root,
        // (board item `01M03KZXR1KF77YRAW4W4GE6KK`) The re-validated,
        // narrowed config computed above -- never `parent_meta.plugin_config`
        // verbatim when the caller supplied `ForkSpec::plugin_config`, and
        // never wider than what the parent imposed. Persisting it here (not
        // just handing it to `resume_root` in-memory) is what lets a
        // subsequent `Conway::resume` of this child re-derive the SAME
        // narrowed value, rather than silently reverting to the parent's --
        // the same round-trip-survives-resume contract
        // `01M0321414SVRD60HEP074AFHG` established for the spawn path.
        plugin_config: child_plugin_config,
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
            result_contract: req.result_contract,
            keep_alive: req.keep_alive,
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

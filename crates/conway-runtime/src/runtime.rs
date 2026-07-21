//! `Runtime`: the facade over one agent tree (WI-082, architecture §4, §7).
//!
//! Owns dependency injection (`RuntimeDeps`), root-agent task lifecycle, and
//! the public surface (`start_root`, `prompt`, `cancel`, `subscribe`,
//! `context_report`, `tree`). No subagent code exists in this item — forking
//! and spawning are WI-084; the agent tree/supervisor guarantees are WI-083.
//! `tree()` here is deliberately a single-level stub over whatever root
//! agents have been started, superseded by `AgentTree::snapshot()` in
//! WI-083.
//!
//! ## Reconciliations against the WI-082 spec's illustrative types
//!
//! - **`ToolRunner`/`PermissionBroker` construction (carried from WI-079's
//!   cycle-1 review, F-079-1):** `ToolRunner::new` takes `Arc<PluginRegistry>`
//!   and `Arc<PermissionBroker>`, not the unwrapped values the WI-080/081
//!   prose's illustrative structs might suggest. This item wraps both in
//!   `Arc` at construction, as that review already flagged for this item's
//!   brief.
//! - **`RuntimeDeps` has no `subagents` field:** `LoopDeps::subagents`
//!   (WI-081, committed) requires an `Arc<dyn SubagentHost>` for every agent
//!   task, and `impl SubagentHost for Runtime` is WI-084's job — it does not
//!   exist yet. Rather than accept this as an injected dependency (WI-082
//!   cycle-1 review, F-082 S1: an embedder-supplied fake is not a real
//!   dependency, and `conway_core::fakes::FakeSubagentHost` is gated behind
//!   `feature = "fakes"`, reserved for test-shaped consumers, so wiring it
//!   into a non-test `Runtime::new` would be a layering violation either
//!   way), `Runtime::new` builds a private, crate-internal [`NoSubagentHost`]
//!   and wires it into `LoopDeps::subagents` itself. Every method returns a
//!   `RuntimeError` naming the gap. WI-084 replaces this stub with a
//!   self-referential `Arc<dyn SubagentHost>` built from the same
//!   `Arc<Runtime>` (`impl SubagentHost for Runtime`) — a detail for that
//!   item, not this one.
//! - **Skill body resolution:** `conway_core::config::AgentDef::skills` is a
//!   `Vec<String>` of *names*; there is no `SkillDef` registry among this
//!   item's eight injected fields (only `agent_defs: HashMap<String,
//!   AgentDef>`), so this item cannot resolve a name to its fragment body.
//!   `start_root` therefore builds every `AgentSpec` with `skills: vec![]`
//!   regardless of what an `AgentDef` names — a known gap, not a decision;
//!   it should be raised against the facade (`MODULE:conway`) as a request
//!   for a `skills: Arc<HashMap<String, SkillDef>>` `RuntimeDeps` field once
//!   skill discovery exists there.
//! - **Live `context_report` via a loop-pushed slot, not a bus fold:** an
//!   earlier revision of this item reconstructed `ContextReport` by
//!   subscribing to the bus and folding `TurnStarted`/`ContextSegmentAdded`
//!   envelopes. WI-082 cycle-1 review (F-082 C1, Critical) rejected that
//!   design: `EventBus::subscribe` synthesizes `Event::Lagged` envelopes
//!   carrying a *freshly generated* `AgentId` on broadcast overflow
//!   (`events.rs`), which the fold could never match back to the lagging
//!   agent, so a slow subscriber's dropped envelopes silently and
//!   permanently truncated that agent's live report with no error and no
//!   signal. This item now follows the spec's own sketch instead: this
//!   crate's authorized, strictly-additive extension to `agent_loop.rs`
//!   (`AgentSpec::report_slot`, an `Option<Arc<Mutex<Option<ContextReport>>>>`)
//!   gives `AgentLoop` a slot to push its just-built `ContextReport` into
//!   every turn, after `ContextBuilder::build` succeeds and before that
//!   turn's backend call. `Runtime::start_root` allocates the slot per agent
//!   and shares it between the `AgentSpec` handed to the loop and this
//!   agent's `AgentHandle::last_report`; `context_report` reads the same
//!   `Arc` directly. No event bus involvement, no reconstruction, and no
//!   window in which an overflowing broadcast buffer can corrupt the
//!   result.
//! - **`Runtime::cancel`'s `reason`:** the committed `AgentLoop::finish_cancelled`
//!   (WI-081) hardcodes `ResultStatus::Cancelled { reason: "cancelled" }` and
//!   has no field or callback through which an external `reason` string
//!   could reach it. `Runtime::cancel` still accepts `reason` (matching the
//!   spec's signature) and records it via `tracing`, but callers must not
//!   expect it to appear in the resulting `AgentResult` until `AgentLoop`
//!   grows a way to accept one.
//! - **`Runtime::cancel`'s return type:** the spec's illustrative signature
//!   shows no return value; this item returns `Result<(), RuntimeError>`
//!   (erroring `AgentNotFound` for an unknown id) for consistency with
//!   `prompt`'s and `context_report`'s error handling. A disclosed,
//!   intentional deviation, not an oversight.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use async_trait::async_trait;
use chrono::Utc;
use conway_core::agent::{
    AgentDefRef, AgentNode, AgentResult, AgentStatus, AgentTreeSnapshot, Budget, ResultStatus,
    SubagentSpec, ToolSelector,
};
use conway_core::capabilities::CacheMode;
use conway_core::config::{AgentDef, DEFAULT_MAX_PARALLEL_TOOLS};
use conway_core::error::{RuntimeError, ToolError};
use conway_core::ids::{AgentId, BackendId, LogSeq, RoleAlias, SessionId};
use conway_core::log::{LogRecord, SessionMeta, SessionStatus};
use conway_core::ports::{
    Backend, HealthRegistry, PermissionGate, Plugin, PluginConfig, Router, SessionStore,
    SubagentHost,
};
use conway_core::provenance::{ContextReport, Provenance};
use conway_core::segment::CacheTtl;
use conway_routing::config::HeadroomPolicy;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use crate::attempt::AttemptEngine;
use crate::context::{ContextBuilder, TOKEN_ESTIMATOR};
use crate::events::{EventBus, EventStream};
use crate::permission::PermissionBroker;
use crate::tools::{PluginRegistry, ToolRunner};

/// Every port-shaped dependency the runtime needs, injected by the facade
/// (or, in tests, built entirely from `conway-core`'s fakes). Nothing here
/// is constructed by this crate: backends, plugins, the store, the router,
/// the permission gate, and the subagent host are all supplied by the
/// caller.
pub struct RuntimeDeps {
    pub store: Arc<dyn SessionStore>,
    pub router: Arc<dyn Router>,
    pub health: Arc<dyn HealthRegistry>,
    pub backends: HashMap<BackendId, Arc<dyn Backend>>,
    pub plugins: Vec<Arc<dyn Plugin>>,
    pub gate: Arc<dyn PermissionGate>,
    pub agent_defs: HashMap<String, AgentDef>,
    pub event_bus: Arc<EventBus>,
    pub headroom: Arc<HeadroomPolicy>,
}

/// The complete specification for starting a new root agent (i.e. one with
/// no parent — the entry point of a fresh agent tree).
pub struct RootSpec {
    /// Overrides the store-assigned session id (useful for reproducible
    /// tests); `None` generates a fresh one.
    pub session: Option<SessionId>,
    pub agent_def: Option<AgentDefRef>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    pub cwd: PathBuf,
    pub prompt: Option<String>,
}

/// Everything the runtime keeps about one live (or finished-but-not-yet-
/// evicted) agent task. Looked up by reference, never cloned wholesale, so
/// `agents`'s `RwLock` is never held across an `.await`.
struct AgentHandle {
    session: SessionId,
    parent: Option<AgentId>,
    cancel: CancellationToken,
    /// Wired in WI-085; created here, unused (the receiver is dropped
    /// immediately — nothing drains it yet, matching `AgentLoop::drain_inbox`
    /// being a no-op until then).
    #[allow(dead_code)]
    inbox: mpsc::Sender<conway_core::agent::AgentMessage>,
    result: watch::Receiver<Option<AgentResult>>,
    last_report: Arc<Mutex<Option<ContextReport>>>,
    #[allow(dead_code)]
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
    agent_def: Option<String>,
    role: Option<RoleAlias>,
    budget: Budget,
}

/// The runtime facade: owns dependency injection and root-agent task
/// lifecycle, and exposes the public surface over the agent loop.
pub struct Runtime {
    store: Arc<dyn SessionStore>,
    bus: Arc<EventBus>,
    agent_defs: HashMap<String, AgentDef>,
    /// Held so `Runtime` reflects the spec's illustrative fields even though
    /// no method here reaches back into them directly (both are already
    /// shared, via clones, with `loop_deps`'s `ToolRunner`).
    #[allow(dead_code)]
    registry: Arc<PluginRegistry>,
    #[allow(dead_code)]
    broker: Arc<PermissionBroker>,
    loop_deps: Arc<LoopDeps>,
    agents: RwLock<HashMap<AgentId, AgentHandle>>,
}

impl Runtime {
    /// Builds a runtime from injected ports. Panics if `deps.plugins`
    /// contains a duplicate tool name across plugins — a malformed plugin
    /// set is a registration bug, not a runtime condition (matches
    /// `PluginRegistry::from_plugins`'s own construction-time-error
    /// contract; `Runtime::new`'s binding signature is infallible, so this
    /// is the only place the check can surface).
    pub fn new(deps: RuntimeDeps) -> Arc<Runtime> {
        let RuntimeDeps {
            store,
            router,
            health,
            backends,
            plugins,
            gate,
            agent_defs,
            event_bus,
            headroom,
        } = deps;

        let registry = Arc::new(
            PluginRegistry::from_plugins(plugins)
                .expect("RuntimeDeps.plugins must register without duplicate tool names"),
        );
        let broker = Arc::new(PermissionBroker::new(gate, event_bus.clone()));
        let tool_runner = Arc::new(ToolRunner::new(
            registry.clone(),
            broker.clone(),
            event_bus.clone(),
        ));
        let attempt = Arc::new(AttemptEngine::new(backends, health, event_bus.clone()));
        let builder = Arc::new(ContextBuilder::new());
        let plugin_config = Arc::new(PluginConfig::default());

        let loop_deps = Arc::new(LoopDeps {
            store: store.clone(),
            router,
            attempt,
            registry: registry.clone(),
            tool_runner,
            subagents: Arc::new(NoSubagentHost) as Arc<dyn SubagentHost>,
            plugin_config,
            bus: event_bus.clone(),
            builder,
            headroom,
        });

        Arc::new(Runtime {
            store,
            bus: event_bus,
            agent_defs,
            registry,
            broker,
            loop_deps,
            agents: RwLock::new(HashMap::new()),
        })
    }

    /// Creates a session, appends its head record, spawns one tokio task
    /// running the new root agent's `AgentLoop`, and returns once that task
    /// has been handed to the executor — before its first turn completes.
    pub async fn start_root(&self, spec: RootSpec) -> Result<AgentId, RuntimeError> {
        let agent_id = AgentId::new();
        let session_id = spec.session.unwrap_or_default();

        let agent_def = spec
            .agent_def
            .as_ref()
            .and_then(|r| self.agent_defs.get(r.0.as_str()));

        let role = spec
            .role
            .clone()
            .or_else(|| agent_def.and_then(|d| d.role.clone()))
            .unwrap_or_else(|| RoleAlias::new("default"));

        let system_prompt = agent_def.map(|d| crate::context::SystemPromptSpec {
            agent_def: d.name.clone(),
            text: d.system_prompt.clone(),
        });
        // Skills are deliberately empty here -- see the module doc's
        // reconciliation note (no SkillDef registry is injected).
        let skills = Vec::new();
        let tools = spec
            .tools
            .clone()
            .or_else(|| agent_def.map(|d| d.tools.clone()));
        let pin = agent_def.and_then(|d| d.model.clone());

        let meta = SessionMeta {
            id: session_id,
            agent_id,
            origin: None,
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role.clone()),
            created: Utc::now(),
            cwd: spec.cwd.clone(),
            labels: Vec::new(),
            status: SessionStatus::Active,
        };
        self.store.create(meta).await?;

        // `append`'s `assign_seq` always overwrites this with the store's
        // own next value (the store, not the caller, is the seq authority --
        // see `conway-session`'s `provenance.rs`), so there is no need to
        // round-trip through `store.head` first for what is always the
        // session's first record.
        self.store
            .append(
                &session_id,
                LogRecord::UserTurn {
                    seq: LogSeq::ZERO,
                    ts: Utc::now(),
                    text: spec.prompt.clone().unwrap_or_default(),
                    prov: Provenance::UserPrompt,
                },
            )
            .await?;

        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills,
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

        let cancel = CancellationToken::new();
        let agent_loop = AgentLoop {
            agent_id,
            session: session_id,
            parent: None,
            agent_path: vec![agent_id],
            cwd: spec.cwd.clone(),
            deps: self.loop_deps.clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
        };

        let (result_tx, result_rx) = watch::channel(None);
        let (inbox_tx, _inbox_rx) = mpsc::channel(64);

        let join = tokio::spawn(async move {
            let result = agent_loop.run().await;
            let _ = result_tx.send(Some(result));
        });

        let handle = AgentHandle {
            session: session_id,
            parent: None,
            cancel,
            inbox: inbox_tx,
            result: result_rx,
            last_report,
            join: Arc::new(Mutex::new(Some(join))),
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role),
            budget: spec.budget,
        };

        self.agents
            .write()
            .expect("agents lock poisoned")
            .insert(agent_id, handle);

        Ok(agent_id)
    }

    /// Appends a `LogRecord::UserTurn` to `agent`'s session before returning
    /// (persist-before-act). Delivering this to a live agent task (so an
    /// already-running conversation picks it up) is WI-085's mailbox wiring;
    /// this item only guarantees the durable append.
    pub async fn prompt(&self, agent: AgentId, text: String) -> Result<(), RuntimeError> {
        let session = {
            let agents = self.agents.read().expect("agents lock poisoned");
            agents
                .get(&agent)
                .ok_or(RuntimeError::AgentNotFound { agent })?
                .session
        };
        // See `start_root`'s note: `append`'s `assign_seq` always overwrites
        // this placeholder with the store's own next value, so no
        // `store.head` round trip is needed first.
        self.store
            .append(
                &session,
                LogRecord::UserTurn {
                    seq: LogSeq::ZERO,
                    ts: Utc::now(),
                    text,
                    prov: Provenance::UserPrompt,
                },
            )
            .await?;
        Ok(())
    }

    /// Trips `agent`'s `CancellationToken`. See the module doc's
    /// reconciliation note on why `reason` cannot yet reach the resulting
    /// `AgentResult`.
    pub fn cancel(&self, agent: AgentId, reason: String) -> Result<(), RuntimeError> {
        let agents = self.agents.read().expect("agents lock poisoned");
        let handle = agents
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?;
        tracing::info!(agent = %agent, reason = %reason, "Runtime::cancel");
        handle.cancel.cancel();
        Ok(())
    }

    /// Every envelope emitted after this call. Two concurrent subscribers
    /// observe identical `seq` sequences per session (guaranteed by
    /// `EventBus::emit`'s atomic assign-then-publish).
    pub fn subscribe(&self) -> EventStream {
        self.bus.subscribe()
    }

    /// The most recent turn's `ContextReport` for `agent`, read directly
    /// from the slot `AgentLoop` pushes into every turn (see the module
    /// doc's reconciliation note). Returns an empty report (rather than
    /// erroring) for an agent that has been started but has not yet
    /// completed a turn.
    pub fn context_report(&self, agent: AgentId) -> Result<ContextReport, RuntimeError> {
        let agents = self.agents.read().expect("agents lock poisoned");
        let handle = agents
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?;
        let report = handle.last_report.lock().expect("report lock poisoned");
        Ok(report.clone().unwrap_or_else(|| empty_report(agent)))
    }

    /// A single-level snapshot of every root agent started so far. Children
    /// (and thus a real, multi-level tree) arrive in WI-083, which replaces
    /// this stub with `AgentTree::snapshot()`. `AgentTreeSnapshot::root` is
    /// arbitrary and nondeterministic when more than one root agent has been
    /// started (this stub has no real notion of "the" root) until WI-083
    /// supplies a real `AgentTree`.
    pub fn tree(&self) -> AgentTreeSnapshot {
        let agents = self.agents.read().expect("agents lock poisoned");
        let nodes: Vec<AgentNode> = agents
            .iter()
            .map(|(id, handle)| {
                let finished = handle.result.borrow().clone();
                let (status, steps_taken) = match &finished {
                    None => (AgentStatus::Running, 0),
                    Some(result) => (status_for(&result.status), result.steps_taken),
                };
                AgentNode {
                    agent_id: *id,
                    session: handle.session,
                    parent: handle.parent,
                    mode: None,
                    agent_def: handle.agent_def.clone(),
                    role: handle.role.clone(),
                    status,
                    steps_taken,
                    budget: handle.budget.clone(),
                }
            })
            .collect();
        let root = nodes.first().map(|n| n.agent_id).unwrap_or_default();
        AgentTreeSnapshot {
            root,
            nodes,
            at: Utc::now(),
        }
    }
}

/// Placeholder `SubagentHost`, wired into every agent task's `LoopDeps`
/// until `impl SubagentHost for Runtime` lands in WI-084 (a self-referential
/// `Arc<dyn SubagentHost>` built from the same `Arc<Runtime>` this stub
/// currently substitutes for — see the module doc's reconciliation note,
/// F-082 S1). Every method reports the closest committed `RuntimeError`
/// naming the gap, rather than a fake success, so a tool or caller that
/// reaches for subagent functionality today gets a clear, typed error.
struct NoSubagentHost;

fn subagents_unavailable() -> RuntimeError {
    RuntimeError::Tool(ToolError::Internal {
        detail: "subagents are unavailable until WI-084 implements `impl SubagentHost for Runtime`"
            .to_string(),
    })
}

#[async_trait]
impl SubagentHost for NoSubagentHost {
    async fn start(&self, _parent: AgentId, _spec: SubagentSpec) -> Result<AgentId, RuntimeError> {
        Err(subagents_unavailable())
    }

    async fn steer(&self, _target: AgentId, _text: String) -> Result<(), RuntimeError> {
        Err(subagents_unavailable())
    }

    async fn await_result(&self, _target: AgentId) -> Result<AgentResult, RuntimeError> {
        Err(subagents_unavailable())
    }

    async fn cancel(&self, _target: AgentId, _reason: String) -> Result<(), RuntimeError> {
        Err(subagents_unavailable())
    }

    fn tree(&self) -> AgentTreeSnapshot {
        AgentTreeSnapshot {
            root: AgentId::default(),
            nodes: Vec::new(),
            at: Utc::now(),
        }
    }
}

fn empty_report(agent_id: AgentId) -> ContextReport {
    ContextReport {
        agent_id,
        turn: 0,
        tokenizer: TOKEN_ESTIMATOR.to_string(),
        segments: Vec::new(),
        total_tokens_est: 0,
    }
}

/// Maps a terminal `ResultStatus` to the tree's coarser `AgentStatus`.
/// `ResultStatus` is `#[non_exhaustive]`; unrecognized future variants map
/// to `Finished` rather than failing to compile or panicking.
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

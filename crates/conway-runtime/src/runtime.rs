//! `Runtime`: the facade over one agent tree (WI-082/WI-083/WI-084,
//! architecture §4, §7).
//!
//! Owns dependency injection (`RuntimeDeps`), root-agent task lifecycle, and
//! the public surface (`start_root`, `prompt`, `cancel`, `subscribe`,
//! `context_report`, `tree`). `tree()` and `cancel()` are backed by the real
//! [`crate::tree::AgentTree`] (WI-083): every agent (root, forked, or
//! spawned) is `attach`ed to it, and its task is wrapped by
//! [`crate::supervisor::supervise`] so a panic or a blown deadline still
//! resolves to a terminal result instead of leaving the tree's bookkeeping
//! stuck on `Running` forever. `impl SubagentHost for Runtime` (WI-084,
//! `subagent.rs`) is this crate's fork/spawn entry point; see that module's
//! doc for the fork/spawn procedure and the self-referential-`Arc`
//! construction this file's `new()` sets up for it. See `tree.rs`'s and
//! `supervisor.rs`'s module docs for the guarantees this buys and the one
//! race it does not close.
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
//!   task. Rather than accept this as an injected dependency (WI-082
//!   cycle-1 review, F-082 S1: an embedder-supplied fake is not a real
//!   dependency, and `conway_core::fakes::FakeSubagentHost` is gated behind
//!   `feature = "fakes"`, reserved for test-shaped consumers, so wiring it
//!   into a non-test `Runtime::new` would be a layering violation either
//!   way), `Runtime::new` now builds the real `subagent::WeakRuntimeHost`
//!   (WI-084) from its own `Weak<Runtime>`, replacing the `NoSubagentHost`
//!   stub this item originally shipped (every method of which returned a
//!   `RuntimeError` naming the gap). See `Runtime::new`'s own doc for why a
//!   `Weak`-backed delegator, not a literal `Arc<Runtime>`, is what
//!   `LoopDeps::subagents` holds.
//! - **WI-084 file-scope note:** the work item's own scope section lists
//!   only `subagent.rs`, `agent_loop.rs`, and its test file — not this file.
//!   In practice `impl SubagentHost for Runtime` cannot be wired up without
//!   touching `Runtime::new` (replacing `NoSubagentHost`, adding the
//!   `TranscriptResolver` instance fork resolution needs) and without a
//!   handful of narrow `pub(crate)` accessors (`loop_deps`, `agent_defs`,
//!   `tree_ref`, `resolver`, `agent_session`, `launch_agent`) letting
//!   `subagent.rs` reach state that was, by design, made private to this
//!   module by WI-082/083. This is disclosed here as a reconciliation
//!   rather than silently expanding scope: every added accessor is
//!   `pub(crate)` (one `#[doc(hidden)] pub` test seam excepted, mirroring
//!   `conway-session`'s own `peek_prefix` precedent), no existing public
//!   method's signature or behavior changes, and `start_root` is left
//!   untouched rather than refactored onto the new `launch_agent` helper,
//!   to keep this file's diff as small as the underlying necessity allows.
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
//! - **WI-083: `AgentHandle` sheds its own result channel and cancel
//!   token.** Before this item, `AgentHandle` held its own
//!   `watch::Receiver<Option<AgentResult>>` (populated by a bare
//!   `tokio::spawn` that sent into a paired `Sender` on completion) and its
//!   own `CancellationToken`, and the WI-082 `tree()`/`cancel()` read and
//!   wrote them directly. Both are now owned by `AgentTree` instead (a
//!   `start_root` agent is `attach`ed to it exactly like a future WI-084
//!   child would be, with `kind: None` since a root is started, not
//!   spawned — see `tree.rs`'s module doc), so `AgentHandle` keeps only
//!   what nothing else already tracks: the session id (for `prompt`) and
//!   the live report slot (for `context_report`). Routing both channels
//!   through one owner is also what makes `tree().nodes[].status` accurate
//!   for a finished root agent, which the old per-`AgentHandle` channel,
//!   never read by the WI-082 `tree()` stub, did not actually provide.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use conway_core::agent::{AgentDefRef, AgentResult, AgentTreeSnapshot, Budget, ToolSelector};
use conway_core::capabilities::CacheMode;
use conway_core::config::{AgentDef, DEFAULT_MAX_PARALLEL_TOOLS};
use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::error::{ConwayError, RuntimeError};
use conway_core::event::Event;
use conway_core::ids::{AgentId, BackendId, LogSeq, ModelRef, RoleAlias, SeqRange, SessionId, ToolName};
use conway_core::log::{
    ForkOrigin, LogRecord, SessionFilter, SessionMeta, SessionStatus, SubagentMode,
};
use conway_core::ports::{
    Backend, ContextHook, HealthRegistry, PermissionGate, Plugin, PluginConfig, Router,
    SessionStore, SubagentHost,
};
use conway_core::provenance::{ContextReport, Provenance};
use conway_core::segment::CacheTtl;
use conway_routing::config::HeadroomPolicy;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use crate::attempt::AttemptEngine;
use crate::context::{ContextBuilder, InheritedPrefix, TOKEN_ESTIMATOR};
use crate::events::{EventBus, EventStream};
use crate::mailbox::{self, Mailbox, MailboxSender};
use crate::permission::PermissionBroker;
use crate::supervisor::{self, SuperviseArgs};
use crate::tools::{PluginRegistry, ToolRunner};
use crate::tree::{AgentNode, AgentTree};

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
    /// Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: this root agent's own
    /// confinement root -- the S3/S5 primitive (`SubagentSpec::root`,
    /// `AgentRoot`, `PermissionBroker::check_root`), finally reachable for
    /// the agent an operator actually talks to. Before this field existed,
    /// `start_root` always passed `SessionMeta.root: None` and
    /// `AgentLoop.root: None`, so `AgentRoot::reconstruct` always produced
    /// `Unconfined` for a root agent and `check_root` returned `Proceed`
    /// without ever inspecting `path_args` -- the root check was real, but
    /// entirely unreachable from the top of the tree.
    ///
    /// `None` (every caller before this field existed, and still the
    /// default for every caller that does not set it) preserves that exact
    /// behavior: unconfined, byte-for-byte. `Some(path)` is resolved exactly
    /// like a spawned child's `SubagentSpec::root` (`subagent.rs`'s
    /// `SubagentHost::start`): relative paths resolve against `cwd` above
    /// (a root agent has no parent cwd to resolve against), the result must
    /// canonicalize, and `cwd` itself must already fall inside it --
    /// `start_root` returns `RuntimeError::Tool(ToolError::Internal { .. })`
    /// (via the same `subagent::invalid_spec` helper `resume_root` already
    /// uses) rather than starting an agent whose own working directory sits
    /// outside its own confinement before a single tool call ever runs.
    /// Cwd is never itself a security boundary (S0's own charter) -- root is
    /// -- so this is a distinct field, not an inference from `cwd`; the two
    /// are configured as a pair (see `conway-cli`'s `--root`) precisely so
    /// an operator cannot confuse them.
    pub root: Option<PathBuf>,
    pub prompt: Option<String>,
    /// Opt-in multi-turn keep-alive (see `agent_loop::AgentSpec::keep_alive`'s
    /// own doc for the bug this fixes and why it must stay opt-in). `false`
    /// preserves this crate's pre-existing behavior exactly: the started
    /// agent's task terminates after its first `Completed` turn, same as
    /// every `RootSpec` caller before this field existed.
    pub keep_alive: bool,
    /// Pins the model for this session, overriding the role's chain (WI-128).
    /// `start_root` prefers this over the `agent_def`-sourced pin when
    /// present -- see that method's own doc for the precedence.
    pub model: Option<ModelRef>,
}

/// The specification for re-registering a persisted session's agent as a
/// live root agent (WI-118 — closes the F-117-1/F-103-1/Q-1 session-
/// continuity gap). Mirrors [`RootSpec`] minus `prompt` (resuming never
/// appends an initial `UserTurn` — the caller's continuation arrives via a
/// later [`Runtime::prompt`] call) and minus the fields recoverable from the
/// persisted [`SessionMeta`] once it is loaded (`agent_def`, `role`, `cwd`
/// all fall back to the header's own values when left `None` here, exactly
/// as `start_root` falls back to an `AgentDef`'s values). `tools` and
/// `budget` are never persisted in `SessionMeta` — like `RootSpec`, both
/// must be supplied fresh on every resume.
pub struct ResumeSpec {
    /// The session to resume. Must already exist in the store — `resume_root`
    /// reads its `SessionMeta` via `store.meta` and does NOT `store.create`.
    pub session: SessionId,
    pub agent_def: Option<AgentDefRef>,
    pub role: Option<RoleAlias>,
    pub tools: Option<ToolSelector>,
    pub budget: Budget,
    /// Overrides the persisted `SessionMeta::cwd`; `None` reuses it.
    pub cwd: Option<PathBuf>,
}

/// Everything the runtime keeps about one live (or finished-but-not-yet-
/// evicted) agent task that isn't already tracked by `Runtime::tree`
/// (WI-083 — see the module doc's reconciliation note). Looked up by
/// reference, never cloned wholesale, so `agents`'s `RwLock` is never held
/// across an `.await`.
struct AgentHandle {
    session: SessionId,
    /// This agent's mailbox sender (WI-085) — cloned out by
    /// [`Runtime::agent_mailbox`] for `subagent.rs`'s `steer` and for a
    /// fork/spawn child's `parent_mailbox`.
    mailbox: MailboxSender,
    last_report: Arc<Mutex<Option<ContextReport>>>,
    /// WI-118 (generalized by keep-alive): the same `Arc` as this agent's
    /// `AgentLoop::resume_gate.notify` (cloned out of the `AgentLoop` before
    /// it moves into its spawned task -- see [`Runtime::launch_agent`]).
    /// [`Runtime::prompt`] calls `notify_one()` on it after every durable
    /// append so a gated `resume_root` agent's idling first iteration
    /// wakes, or a `keep_alive: true` agent's idling END-of-turn wait wakes
    /// to run the newly-appended prompt as a genuine next turn; a no-op for
    /// every other agent, whose loop never awaits it.
    prompt_notify: Arc<tokio::sync::Notify>,
    #[allow(dead_code)]
    join: Arc<Mutex<Option<JoinHandle<()>>>>,
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
    tree: Arc<AgentTree>,
    /// Ancestry resolution for WI-084's fork path (`subagent.rs`) --
    /// `conway_session::TranscriptResolver`, one instance per runtime so
    /// sibling forks share memoized prefixes (see that type's own module
    /// doc). Not part of `RuntimeDeps`: it needs no injected configuration
    /// beyond a cache capacity, and adding a field to `RuntimeDeps` would
    /// be a breaking change to WI-082's already-committed, criterion-pinned
    /// surface for a value this crate can construct unconditionally itself.
    resolver: Arc<conway_session::TranscriptResolver>,
}

/// Entry count for `Runtime`'s `TranscriptResolver` cache. No criterion
/// pins this value; `conway-session`'s own test suite exercises capacities
/// from 2 to 64 without treating the number itself as load-bearing, so this
/// picks a generous-but-bounded default rather than inventing a config
/// surface WI-084 has no mandate to add.
const TRANSCRIPT_CACHE_CAPACITY: usize = 512;

impl Runtime {
    /// Builds a runtime from injected ports. Panics if `deps.plugins`
    /// contains a duplicate tool name across plugins — a malformed plugin
    /// set is a registration bug, not a runtime condition (matches
    /// `PluginRegistry::from_plugins`'s own construction-time-error
    /// contract; `Runtime::new`'s binding signature is infallible, so this
    /// is the only place the check can surface).
    ///
    /// ## WI-084 reconciliation: self-referential `subagents`
    ///
    /// `LoopDeps::subagents` must be a working `Arc<dyn SubagentHost>`
    /// backed by this very `Runtime` (`impl SubagentHost for Runtime`,
    /// `subagent.rs`) -- every agent task's `ToolCtx` needs to be able to
    /// fork/spawn through it. A literal `Arc<Runtime>` stored inside
    /// `Arc<LoopDeps>` (which `Runtime` itself also owns, and which every
    /// agent task also holds a clone of) would be a strong reference cycle:
    /// `Runtime` would never drop, even once every external handle and
    /// every agent task is gone. `Arc::new_cyclic` breaks the cycle: the
    /// constructor closure receives a `Weak<Runtime>` *before* the `Arc`
    /// exists, which `subagent::WeakRuntimeHost` (a small, crate-private
    /// delegator -- see its own doc) wraps and upgrades on every call. This
    /// is the "self-referential `Arc<Runtime>` or equivalent" the work item
    /// anticipates; `WeakRuntimeHost` is the "or equivalent".
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
        let tree = Arc::new(AgentTree::new(event_bus.clone()));
        let resolver = Arc::new(conway_session::TranscriptResolver::new(
            TRANSCRIPT_CACHE_CAPACITY,
        ));

        Arc::new_cyclic(|weak: &std::sync::Weak<Runtime>| {
            let loop_deps = Arc::new(LoopDeps {
                store: store.clone(),
                router,
                attempt,
                registry: registry.clone(),
                tool_runner,
                subagents: Arc::new(crate::subagent::WeakRuntimeHost::new(weak.clone()))
                    as Arc<dyn SubagentHost>,
                plugin_config,
                bus: event_bus.clone(),
                builder,
                headroom,
                tree: tree.clone(),
                // WI-126: no `RuntimeDeps` field sources this (out of that
                // item's file scope to add here) -- `set_context_hook`
                // below fills it in post-construction. `None` here
                // preserves every existing caller's behavior unchanged.
                context_hook: RwLock::new(None),
            });

            Runtime {
                store,
                bus: event_bus,
                agent_defs,
                registry,
                broker,
                loop_deps,
                agents: RwLock::new(HashMap::new()),
                tree,
                resolver,
            }
        })
    }

    /// Everything `subagent.rs`'s `impl SubagentHost for Runtime` (WI-084)
    /// needs that isn't reachable through `loop_deps()`'s already-`pub`
    /// fields. Kept as narrow, crate-private accessors rather than widening
    /// any field's visibility, so `Runtime`'s actual public surface (the
    /// thing the WI-084 criterion "no additional public methods" on the
    /// trait impl is protecting) is unaffected.
    pub(crate) fn loop_deps(&self) -> &Arc<LoopDeps> {
        &self.loop_deps
    }

    pub(crate) fn agent_defs(&self) -> &HashMap<String, AgentDef> {
        &self.agent_defs
    }

    pub(crate) fn tree_ref(&self) -> &Arc<AgentTree> {
        &self.tree
    }

    pub(crate) fn resolver(&self) -> &Arc<conway_session::TranscriptResolver> {
        &self.resolver
    }

    /// WI-126: registers (or clears, via `None`) the `ContextHook` every
    /// agent task under this runtime consults before each LLM request and
    /// on T-1 overflow (`AgentLoop::route_and_attempt`). A new, purely
    /// additive public method rather than a `RuntimeDeps` field: `RuntimeDeps`
    /// is out of this item's file scope, and is also constructed by field
    /// literal in several existing tests that a new required field would
    /// have broken. Intended to be called once, by the facade
    /// (`conway::ConwayBuilder::build`), before any session is started;
    /// safe to call at any time regardless, since every turn reads the
    /// current value fresh (`AgentLoop::context_hook`).
    pub fn set_context_hook(&self, hook: Option<Arc<dyn ContextHook>>) {
        *self
            .loop_deps
            .context_hook
            .write()
            .expect("context_hook lock poisoned") = hook;
    }

    /// Test-only accessor (mirrors `conway_session::TranscriptResolver::
    /// peek_prefix`'s own `#[doc(hidden)] pub` test seam): lets integration
    /// tests assert `Arc::ptr_eq` sibling-fork sharing directly against the
    /// runtime's own resolver instance, the same guarantee `conway-session`'s
    /// test suite already proves at the resolver level in isolation.
    #[doc(hidden)]
    pub fn resolver_for_test(&self) -> &Arc<conway_session::TranscriptResolver> {
        self.resolver()
    }

    /// The session id of a live-or-finished agent tracked by this runtime.
    /// `AgentNotFound` for an unknown id -- the same lookup `prompt` already
    /// inlines, factored out so `subagent.rs`'s `start` (WI-084) can resolve
    /// a fork/spawn `parent`'s session without duplicating it.
    pub(crate) fn agent_session(&self, agent: AgentId) -> Result<SessionId, RuntimeError> {
        let agents = self.agents.read().expect("agents lock poisoned");
        Ok(agents
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?
            .session)
    }

    /// A clone of `agent`'s mailbox sender (WI-085). Used by `subagent.rs`'s
    /// `steer` (the target's sender) and `start` (the parent's sender, so a
    /// fork/spawn child can deliver its terminal `Result` upward through
    /// `AgentLoop::parent_mailbox`).
    pub(crate) fn agent_mailbox(&self, agent: AgentId) -> Result<MailboxSender, RuntimeError> {
        let agents = self.agents.read().expect("agents lock poisoned");
        Ok(agents
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?
            .mailbox
            .clone())
    }

    /// Attaches `node` to the tree, spawns `agent_loop`'s task under the
    /// supervisor, and registers its handle. The shared tail of both
    /// `start_root` (root agents, unchanged, still inlines its own copy of
    /// this sequence) and WI-084's fork/spawn path (`subagent.rs`), which
    /// has no other way to reach `agents`/`tree`/`bus` to do this itself
    /// without those fields losing their private visibility.
    ///
    /// `mailbox` is the sender half of the mailbox `agent_loop.inbox`
    /// already owns the receiver half of (WI-085) -- constructed by the
    /// caller, since only the caller (`subagent.rs`'s `start`) knows
    /// whether this child also needs a `parent_mailbox` wired from an
    /// already-registered parent before this agent's own handle exists.
    pub(crate) fn launch_agent(
        &self,
        node: AgentNode,
        agent_loop: AgentLoop,
        last_report: Arc<Mutex<Option<ContextReport>>>,
        mailbox: MailboxSender,
    ) -> Result<(), RuntimeError> {
        let agent_id = node.id;
        let session_id = node.session;
        let cancel = node.cancel.clone();
        let deadline = agent_loop.spec.budget.deadline;
        // WI-118: `agent_loop` already carries its own `resume_gate.notify`
        // (a real `resume_root` gate, or an unused `Default` one for every
        // other caller of this shared path -- currently only `subagent.rs`'s
        // fork/spawn) -- clone the same `Arc` out before `agent_loop` moves
        // into its spawned task below, so `Runtime::prompt` has something to
        // notify.
        let prompt_notify = agent_loop.resume_gate.notify.clone();

        self.tree.attach(node)?;

        let task: JoinHandle<AgentResult> = tokio::spawn(async move { agent_loop.run().await });
        let join = supervisor::supervise(SuperviseArgs {
            tree: self.tree.clone(),
            bus: self.bus.clone(),
            agent: agent_id,
            session: session_id,
            cancel,
            deadline,
            grace: supervisor::DEFAULT_GRACE,
            task,
        });

        let handle = AgentHandle {
            session: session_id,
            mailbox,
            last_report,
            prompt_notify,
            join: Arc::new(Mutex::new(Some(join))),
        };
        self.agents
            .write()
            .expect("agents lock poisoned")
            .insert(agent_id, handle);
        Ok(())
    }

    /// Creates a session, appends its head record, spawns one tokio task
    /// running the new root agent's `AgentLoop`, and returns once that task
    /// has been handed to the executor — before its first turn completes.
    /// The permission broker this runtime authorizes tool calls through
    /// (V2b).
    ///
    /// Exposed so a consumer can read and change permission MODE and
    /// pattern grants at runtime — the TUI's `/settings` needs this, and
    /// `conway-cli` is mechanically forbidden from depending on
    /// `conway-runtime` (`no_forbidden_deps`), so the facade re-exposes
    /// this rather than the CLI reaching in.
    ///
    /// The broker is the AUTHORITY on what is permitted. Any copy a
    /// consumer keeps for display is a mirror, and must be refreshed from
    /// here rather than written independently.
    pub fn permission_broker(&self) -> Arc<PermissionBroker> {
        Arc::clone(&self.broker)
    }

    /// F12: the `render_kind` a registered tool declares for itself, by
    /// name, or `None` if no plugin registered that tool. Used by the
    /// facade's permission-file loader to surface a typed registration
    /// error when a `command_prefix` rule is paired with a tool whose
    /// rendering is `Structured` (a JSON dump) -- a rule that can never
    /// reliably match is a lie the operator will not notice, so the loader
    /// refuses to install one silently. This is the ONLY new tool-metadata
    /// surface the structured-rule registration check needs; it reads the
    /// already-resolved tool the same way `ToolRunner::execute_one` does.
    pub fn tool_render_kind(&self, name: &ToolName) -> Option<conway_core::ports::RenderKind> {
        self.registry.resolve(name).map(|r| r.tool.render_kind())
    }

    /// B1: the `PathArgs` a registered tool declares for itself, by name, or
    /// `None` if no plugin registered that tool. The structured-rule
    /// registration check (`validate_rule_registration`) uses this -- alongside
    /// [`Self::tool_render_kind`] -- to surface a typed registration error
    /// when a `paths_under` deny/prompt rule is paired with a tool whose
    /// `PathArgs` is not `Named` (i.e. `Unconfinable` such as `bash`, or
    /// `None`). A `paths_under` predicate can never confine such a tool, so a
    /// `then: deny/prompt` rule selecting it is silently inert -- fail-OPEN,
    /// the hazard B1 closes. `then: allow` is fail-CLOSED for the same inert
    /// (the broker simply never matches it and the call falls through to the
    /// gate), so it is NOT a registration error. Reads the already-resolved
    /// tool the same way `ToolRunner::execute_one` does; no new resolution
    /// path, and the same surface `tool_render_kind` already established.
    pub fn tool_path_args(&self, name: &ToolName) -> Option<conway_core::ports::PathArgs> {
        self.registry.resolve(name).map(|r| r.tool.path_args())
    }

    /// A4: every registered tool's `(name, category, render_kind)` metadata,
    /// enumerating the whole registry rather than resolving one name. The
    /// broadened `command_prefix`-on-`Structured` registration check uses this
    /// to resolve a `Select::Tools` trailing-`*` wildcard or a
    /// `Select::Categories` select against the tools actually registered at
    /// load time, so a `command_prefix` rule that would be silently inert
    /// (every Structured-rendering tool it selects can never match a
    /// token-wise prefix over a JSON dump) is surfaced to the operator
    /// rather than installed inert -- the mirror of the single-tool check
    /// `tool_render_kind` already drove. Reads the same compiled `ToolSpec`s
    /// and `Tool::render_kind` declarations, just enumerated; no new
    /// resolution path.
    pub fn registered_tools_metadata(
        &self,
    ) -> Vec<(
        conway_core::ids::ToolName,
        conway_core::content::ToolCategory,
        conway_core::ports::RenderKind,
    )> {
        self.registry.tools_metadata()
    }

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
        // `spec.model` (a caller-supplied pin, e.g. `--model`) takes
        // precedence over the `agent_def`'s own configured model (WI-128).
        let pin = spec
            .model
            .clone()
            .or_else(|| agent_def.and_then(|d| d.model.clone()));

        // Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: resolve and validate this
        // root agent's own confinement root ONCE, mirroring `subagent.rs`'s
        // `SubagentHost::start` spawn-time validation for a spawned child's
        // `Some(requested)` root (see that method's own doc for the full
        // shape this repeats). A root agent has no parent root to narrow
        // against -- it IS the top of the tree -- so only two checks apply
        // here: the requested root itself must canonicalize, and `spec.cwd`
        // must already fall inside it. A relative `spec.root` resolves
        // against `spec.cwd` (there is no parent cwd to resolve against, and
        // `cwd`/`root` are configured as a pair -- see `RootSpec::root`'s
        // own doc).
        let root: Option<PathBuf> = match &spec.root {
            None => None,
            Some(requested) => {
                let resolved = if requested.is_absolute() {
                    requested.clone()
                } else {
                    spec.cwd.join(requested)
                };
                let canonical_root = CanonicalRoot::new(&resolved).map_err(|err| {
                    crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "root agent's root {} does not canonicalize: {err}",
                            resolved.display()
                        ),
                    })
                })?;
                match canonical_root.contains(&spec.cwd) {
                    Containment::Inside => {}
                    Containment::Outside | Containment::Undecidable => {
                        // Same "show both operands on the same footing"
                        // treatment as `resume_root`'s identical check
                        // below, and `subagent.rs`'s own cwd-outside-root
                        // error -- see either's comment for why.
                        let canonical_cwd = spec.cwd.canonicalize().ok();
                        let shown = match &canonical_cwd {
                            Some(c) if c != &spec.cwd => {
                                format!("{} (resolved: {})", spec.cwd.display(), c.display())
                            }
                            _ => spec.cwd.display().to_string(),
                        };
                        return Err(crate::subagent::invalid_spec(ConwayError::Config {
                            detail: format!(
                                "root agent's cwd {} is outside its own root {}",
                                shown,
                                canonical_root.as_path().display(),
                            ),
                        }));
                    }
                }
                Some(canonical_root.as_path().to_path_buf())
            }
        };

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
            // A root is never ephemeral -- only a facade-level fork-ask
            // child is (`conway`'s `SessionHandle::ask`, which sets this
            // itself before calling `store.fork`, never `start_root`).
            ephemeral: false,
            // B5: a root is never an `/ask` child either -- the tag only
            // exists on ephemeral ask children (stamped from the spec in
            // `subagent.rs`'s `SubagentHost::start`).
            ask_origin: None,
            // Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: the resolved, canonical
            // root computed above -- `None` (unconfined) exactly as before
            // this field existed unless `spec.root` was set.
            root: root.clone(),
        };
        self.store.create(meta).await?;

        // Seed an initial user turn ONLY when the caller supplied a prompt.
        // A prompt-less root (the interactive TUI, and any `new_session`
        // whose first prompt arrives later via `Runtime::prompt`) starts
        // IDLE: no empty placeholder turn is written and the loop gates its
        // first iteration (see `resume_gate` below), so the agent never runs
        // a turn against an empty prompt and "explores" before the user has
        // said anything. `append`'s `assign_seq` overwrites `seq` with the
        // store's own next value regardless.
        //
        // The matching live `Event::UserTurn` (this item) is emitted right
        // after the append succeeds -- ordering-safe unconditionally: a root
        // is attached below with `kind: None` (see that call's own comment),
        // so `AgentTree::attach` never emits `Event::AgentSpawned` for it at
        // all, and the "AgentSpawned precedes every event for its agent"
        // guarantee is vacuous for a root either way.
        if let Some(text) = spec.prompt.clone() {
            self.store
                .append(
                    &session_id,
                    LogRecord::UserTurn {
                        seq: LogSeq::ZERO,
                        ts: Utc::now(),
                        text: text.clone(),
                        prov: Provenance::UserPrompt,
                    },
                )
                .await?;
            self.bus.emit(
                session_id,
                agent_id,
                Event::UserTurn {
                    text,
                    prov: Provenance::UserPrompt,
                },
            );
        }

        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills,
            tools,
            role: role.clone(),
            pin,
            budget: spec.budget.clone(),
            // Deliberately `None`, not a gap: `ContextBuilder::build` runs
            // before routing resolves a concrete model, so this can only
            // ever be a pre-routing placeholder. The prompt-caching item's
            // real, capability-keyed cache-hint attachment happens as a
            // POST-routing pass in `attempt.rs`'s `attach_route_cache_hints`
            // -- see `subagent.rs`'s module doc ("`CacheMode` is not wired
            // from `SubagentSpec::cache_hint`") for the full rationale,
            // which applies identically to a root.
            cache_mode: CacheMode::None,
            cache_ttl: CacheTtl::FiveMinutes,
            headroom_override: None,
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            report_slot: Some(last_report.clone()),
            // A root agent has no `SubagentSpec` to source a contract from
            // (WI-086) -- only a fork/spawn child can declare one.
            result_contract: None,
            keep_alive: spec.keep_alive,
        };

        let cancel = CancellationToken::new();
        let (mailbox_tx, mailbox_rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
        let mailbox_tx =
            mailbox_tx.with_events(self.bus.clone(), session_id, agent_id, cancel.clone());
        let agent_loop = AgentLoop {
            agent_id,
            session: session_id,
            parent: None,
            agent_path: vec![agent_id],
            cwd: spec.cwd.clone(),
            // Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: matches `meta.root`
            // above -- the same resolved, canonical root (or `None`,
            // unconfined, unchanged from before this field existed).
            root: root.clone(),
            deps: self.loop_deps.clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
            // A root agent's context never inherits anything (WI-084: only
            // a fork child gets `Some`).
            inherited: None,
            inbox: mailbox_rx,
            // A root has no parent to deliver a terminal `Result` to
            // (WI-085).
            parent_mailbox: None,
            pending_cancel: None,
            // A root started WITHOUT an initial prompt (the interactive TUI;
            // any `new_session` whose first prompt arrives later via
            // `Runtime::prompt`) gates its first iteration and idles until
            // that prompt arrives -- otherwise it would immediately run a
            // turn against the empty placeholder and "explore" before the
            // user has typed anything. A root started WITH a prompt runs its
            // first turn immediately, as before. For a `keep_alive` root,
            // `run_inner` re-arms this same gate at each turn boundary.
            resume_gate: crate::agent_loop::ResumeGate {
                awaiting_prompt: spec.prompt.is_none(),
                notify: Arc::new(tokio::sync::Notify::new()),
            },
        };
        let prompt_notify = agent_loop.resume_gate.notify.clone();

        // A root is started, not spawned (`kind: None`) — see `tree.rs`'s
        // module doc on why that means `attach` will not emit
        // `Event::AgentSpawned` for it.
        self.tree.attach(AgentNode {
            id: agent_id,
            parent: None,
            session: session_id,
            kind: None,
            agent_def: agent_def.map(|d| d.name.clone()),
            role: Some(role),
            budget: spec.budget.clone(),
            cancel: cancel.clone(),
            inherited_upto: None,
            // A root is never ephemeral (decision 01KYD1TWXMZD4BT842CMJT1AED:
            // only `conway`'s facade `SessionHandle::ask` builds an ephemeral
            // child, and that goes through `resume_root`, not `start_root`).
            ephemeral: false,
        })?;

        let task: JoinHandle<AgentResult> = tokio::spawn(async move { agent_loop.run().await });
        let join = supervisor::supervise(SuperviseArgs {
            tree: self.tree.clone(),
            bus: self.bus.clone(),
            agent: agent_id,
            session: session_id,
            cancel,
            deadline: spec.budget.deadline,
            grace: supervisor::DEFAULT_GRACE,
            task,
        });

        let handle = AgentHandle {
            session: session_id,
            mailbox: mailbox_tx,
            last_report,
            prompt_notify,
            join: Arc::new(Mutex::new(Some(join))),
        };

        self.agents
            .write()
            .expect("agents lock poisoned")
            .insert(agent_id, handle);

        Ok(agent_id)
    }

    /// Re-registers an already-persisted session's agent as live (WI-118):
    /// reads its existing `SessionMeta` via `store.meta` (erroring
    /// `RuntimeError::Store(StoreError::NotFound { .. })` — already a typed
    /// `RuntimeError` via `#[from]`, not a panic and not a `create` — for an
    /// unknown or record-less session id) and registers it into
    /// `Runtime.agents`/`AgentTree` through the same [`Runtime::launch_agent`]
    /// path `start_root` uses, so `prompt`/`cancel`/`tree`/`context_report`
    /// all work on the returned `AgentId` exactly as they do for a
    /// `start_root` agent.
    ///
    /// Unlike `start_root`, this method does NOT `store.create` (the session
    /// already exists) and does NOT append an initial `UserTurn` — the
    /// caller's continuation prompt arrives via a subsequent
    /// [`Runtime::prompt`] call. `AgentLoop` re-resolves the full effective
    /// transcript from the store on every turn (`conway_session`'s
    /// `TranscriptResolver`), so "continue from where it left off" falls out
    /// of that existing mechanism once this agent is registered — this
    /// method's job is registration, not transcript replay.
    ///
    /// The returned `AgentId` is `SessionMeta::agent_id` (the id the session
    /// was originally created under), never a freshly minted one: a caller
    /// that persisted the original agent id (e.g. `conway::conway::resume`)
    /// can address this resumed agent with the same id it already has, and
    /// `Runtime::agent_session`/`prompt`/`tree` all resolve against this same
    /// id since it is exactly what gets inserted into `self.agents` and
    /// attached to the tree below.
    ///
    /// ## Child re-registration (criterion 4 disclosure)
    ///
    /// This method attaches only the resumed root to `AgentTree` — it does
    /// NOT walk `store.children` to re-attach past fork/spawn children as
    /// live tree nodes. Those children's own agent tasks are long gone (this
    /// is a process restart, not a live process with tasks to reconnect to);
    /// attaching them as `AgentTree` nodes with no backing task would leave
    /// them permanently `AgentStatus::Running` in `Runtime::tree()` (nothing
    /// would ever call `AgentTree::publish_result` for them), which is a
    /// worse misrepresentation than omitting them. Their history remains
    /// fully readable via `store.children`/`store.read` directly and via
    /// [`Runtime::context_report_at`] (which already resolves an agent id
    /// via a store scan, not the live tree); only *live* re-registration —
    /// i.e., resuming a child as a promptable agent in its own right — is
    /// out of scope for this item. A caller that needs that can call
    /// `resume_root` again with that child's own `SessionId`.
    pub async fn resume_root(&self, spec: ResumeSpec) -> Result<AgentId, RuntimeError> {
        let meta = self.store.meta(&spec.session).await?;
        let agent_id = meta.agent_id;

        // WI-119: a genuine root's own session records ARE its complete
        // history (`inherited` stays `None`, matching WI-118's original,
        // unaffected behavior below) -- but a fork child's own records are,
        // by the zero-copy fork contract (`SessionStore::fork`, D-11), only
        // its OWN post-fork turns; the inherited portion lives in the
        // parent's session by reference and must be resolved here, exactly
        // as `subagent.rs`'s live fork path resolves it for a
        // freshly-forked child (see that module's doc). Detected via
        // `SessionMeta::origin`, the same signal `subagent.rs` itself
        // produces on fork (`mode: SubagentMode::Fork`) -- a spawned
        // child's `origin` is `Some` too, but with `mode: SubagentMode::
        // Spawn`, for which context assembly has never inherited anything
        // (`subagent.rs`'s own spawn branch always builds `inherited:
        // None`), so only the `Fork` arm resolves a prefix here.
        //
        // `resolver().resolve(store, &child)` -- what `subagent.rs` uses --
        // is NOT reusable as-is: it returns the child's FULL effective
        // transcript at its current head, which is only exactly "the
        // parent's prefix" at the one moment `subagent.rs` calls it
        // (immediately after `store.fork`, before the child owns any
        // records of its own). A resumed fork child may already have run
        // turns of its own (non-empty own records), and `AgentLoop` reads
        // those own records separately every turn -- folding them into
        // `inherited` too would double-count them. `TranscriptResolver::
        // resolve_prefix` (made `pub` for this, `conway-session`, WI-119)
        // is the shared primitive both paths already bottom out on; calling
        // it directly against `(origin.parent, origin.at_seq)` resolves
        // exactly the parent-only portion, at any depth, without a second,
        // divergent copy of the D-11 ancestry walk in this crate.
        let inherited = match meta.origin {
            Some(ForkOrigin {
                parent,
                at_seq,
                mode: SubagentMode::Fork,
            }) => {
                let records = self
                    .resolver
                    .resolve_prefix(self.store.as_ref(), &parent, at_seq)
                    .await?;
                Some(InheritedPrefix {
                    from: parent,
                    seq_range: SeqRange::new(LogSeq::ZERO, Some(at_seq)),
                    records,
                })
            }
            _ => None,
        };

        let agent_def_ref = spec
            .agent_def
            .clone()
            .or_else(|| meta.agent_def.clone().map(AgentDefRef));
        let agent_def = agent_def_ref
            .as_ref()
            .and_then(|r| self.agent_defs.get(r.0.as_str()));

        let role = spec
            .role
            .clone()
            .or_else(|| meta.role.clone())
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
        let cwd = spec.cwd.clone().unwrap_or_else(|| meta.cwd.clone());

        // (S3) `ResumeSpec` carries no `root` override field at all -- this
        // session's `root` is therefore always whatever `meta.root` already
        // says (this method never rewrites the header), which by
        // construction can neither widen nor null it: there is no code path
        // here that could turn a persisted `Some(root)` into `None`, or
        // replace it with something wider. What CAN change on resume is
        // `cwd` (`spec.cwd` may override the persisted `meta.cwd` above),
        // and an overridden `cwd` must still satisfy "cwd subset of root,
        // always" (the same invariant `subagent.rs`'s `SubagentHost::start`
        // enforces at spawn time) -- a resumed root confined to `meta.root`
        // whose caller-supplied `cwd` override has drifted outside it (e.g.
        // the root directory itself moved, or the caller passed the wrong
        // path) must fail loudly here rather than resume into an incoherent
        // state. `Containment::Undecidable` is treated identically to
        // `Outside` -- "can't check" is never "allow" (see
        // `conway_core::containment::Containment`'s own doc).
        if let Some(root_path) = meta.root.as_deref() {
            let canonical_root = CanonicalRoot::new(root_path).map_err(|err| {
                crate::subagent::invalid_spec(ConwayError::Config {
                    detail: format!(
                        "resumed session's root {} does not canonicalize: {err}",
                        root_path.display()
                    ),
                })
            })?;
            match canonical_root.contains(&cwd) {
                Containment::Inside => {}
                Containment::Outside | Containment::Undecidable => {
                    // Both operands on the same footing -- see the identical
                    // treatment in `subagent.rs`'s cwd-outside-root error for
                    // why a raw cwd beside a canonical root misleads.
                    let canonical_cwd = cwd.canonicalize().ok();
                    let shown = match &canonical_cwd {
                        Some(c) if c != &cwd => {
                            format!("{} (resolved: {})", cwd.display(), c.display())
                        }
                        _ => cwd.display().to_string(),
                    };
                    return Err(crate::subagent::invalid_spec(ConwayError::Config {
                        detail: format!(
                            "resumed cwd {} is outside the session's own root {}",
                            shown,
                            canonical_root.as_path().display()
                        ),
                    }));
                }
            }
        }

        let last_report = Arc::new(Mutex::new(None));
        let agent_spec = AgentSpec {
            system_prompt,
            skills,
            tools,
            role: role.clone(),
            pin,
            budget: spec.budget.clone(),
            // Pre-routing placeholder, same as `start_root` above -- see
            // that field's comment there for the full rationale.
            cache_mode: CacheMode::None,
            cache_ttl: CacheTtl::FiveMinutes,
            headroom_override: None,
            max_parallel_tools: DEFAULT_MAX_PARALLEL_TOOLS,
            report_slot: Some(last_report.clone()),
            // A resumed root has no `SubagentSpec` to source a contract
            // from either -- same as `start_root`.
            result_contract: None,
            // Out of scope for this item: only a freshly `start_root`ed
            // agent can opt into keep-alive today (`RootSpec::keep_alive`).
            // `ResumeSpec` has no counterpart field, so a resumed root
            // always terminates on its first `Completed` turn, same as
            // before keep-alive existed.
            keep_alive: false,
        };

        let cancel = CancellationToken::new();
        let (mailbox_tx, mailbox_rx) = Mailbox::new(mailbox::RUNTIME_CAPACITY);
        let mailbox_tx =
            mailbox_tx.with_events(self.bus.clone(), spec.session, agent_id, cancel.clone());
        let agent_loop = AgentLoop {
            agent_id,
            session: spec.session,
            parent: None,
            agent_path: vec![agent_id],
            cwd: cwd.clone(),
            // (S3/S5) `meta.root` was already validated against `cwd` just
            // above (the `Containment` check this method's own doc
            // describes) -- passed straight through unchanged, exactly as
            // the module doc for `ResumeSpec` promises ("no code path here
            // could turn a persisted `Some(root)` into `None`, or replace
            // it with something wider").
            root: meta.root.clone(),
            deps: self.loop_deps.clone(),
            spec: agent_spec,
            cancel: cancel.clone(),
            // A genuine resumed root still inherits nothing (`None`, same
            // as `start_root`; the resolver rebuilds its full effective
            // transcript from its own records instead) -- a resumed fork
            // child gets its resolved parent prefix instead, computed just
            // above.
            inherited,
            inbox: mailbox_rx,
            // A root has no parent to deliver a terminal `Result` to.
            parent_mailbox: None,
            pending_cancel: None,
            // WI-118 (the F-118 D-3 fix): this loop's first iteration must
            // wait for the caller's next `Runtime::prompt` rather than
            // racing it against the persisted (already-completed)
            // transcript -- see `ResumeGate`'s and `run_inner`'s own docs.
            // `launch_agent` clones this same `notify` `Arc` into this
            // agent's `AgentHandle` before `agent_loop` moves into its
            // spawned task, which is what `Runtime::prompt` signals below.
            resume_gate: crate::agent_loop::ResumeGate {
                awaiting_prompt: true,
                notify: Arc::new(tokio::sync::Notify::new()),
            },
        };

        // A resumed root is re-started, not spawned (`kind: None`) -- see
        // `tree.rs`'s module doc on why that means `attach` will not emit
        // `Event::AgentSpawned` for it, matching `start_root`'s own root
        // node.
        let node = AgentNode {
            id: agent_id,
            parent: None,
            session: spec.session,
            kind: None,
            agent_def: agent_def
                .map(|d| d.name.clone())
                .or_else(|| meta.agent_def.clone()),
            role: Some(role),
            budget: spec.budget.clone(),
            cancel: cancel.clone(),
            inherited_upto: None,
            // Stamped from the persisted `SessionMeta::ephemeral`: a session
            // forked off ephemeral (e.g. a `/ask` child, born via
            // `SubagentHost::start` with `spec.ephemeral = true` -- board
            // item B2 moved the facade `/ask` onto that path) keeps the bit
            // in its header, so resuming it later re-attaches an ephemeral
            // node; a normal `conway::resume`/`fork_from` header has it
            // `false`. `attach` does not emit `Event::AgentSpawned` for this
            // node (a resumed root has `kind: None`), but `ephemeral_of`
            // reads this field for the `Event::AgentFinished` stamp at
            // finish time.
            ephemeral: meta.ephemeral,
        };

        self.launch_agent(node, agent_loop, last_report, mailbox_tx)?;

        Ok(agent_id)
    }

    /// Appends a `LogRecord::UserTurn` to `agent`'s session before returning
    /// (persist-before-act), then emits the live `Event::UserTurn` twin (this
    /// item) so a subscriber on the event stream sees the SAME occurrence
    /// live that replay would later reconstruct from the log -- closing the
    /// P-8 gap where only the TUI (via its own local `Entry::User` push) ever
    /// showed a prompt. Ordering-safe for every caller of this method: `agent`
    /// must already be a KEY of `self.agents` (looked up just below) to reach
    /// the emit at all, and the only way an agent id becomes a key is
    /// `launch_agent`, which calls `AgentTree::attach` (and thus, for any
    /// agent with `kind: Some(_)`, emits `Event::AgentSpawned`) BEFORE
    /// inserting into `self.agents` -- so `Event::AgentSpawned` has always
    /// already been emitted (if this agent has one at all -- a root's `kind`
    /// is `None`, so it has none, and the ordering guarantee is vacuous for
    /// it) by the time any `prompt` call for that agent can even find it.
    /// Delivering this to a live agent task (so an already-running
    /// conversation picks it up) is WI-085's mailbox wiring; this item only
    /// guarantees the durable append plus this live broadcast.
    pub async fn prompt(&self, agent: AgentId, text: String) -> Result<(), RuntimeError> {
        let (session, prompt_notify) = {
            let agents = self.agents.read().expect("agents lock poisoned");
            let handle = agents
                .get(&agent)
                .ok_or(RuntimeError::AgentNotFound { agent })?;
            (handle.session, handle.prompt_notify.clone())
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
                    text: text.clone(),
                    prov: Provenance::UserPrompt,
                },
            )
            .await?;
        self.bus.emit(
            session,
            agent,
            Event::UserTurn {
                text,
                prov: Provenance::UserPrompt,
            },
        );
        // WI-118, generalized by keep-alive: wakes a `resume_root` agent's
        // gated first iteration, OR a `keep_alive: true` agent's gated
        // end-of-turn idle wait -- both the same `ResumeGate` (see that
        // type's doc). `Notify::notify_one`'s single stored permit means
        // this is safe even if that agent's task has not polled its
        // `notified()` yet -- the permit is buffered and consumed by the
        // very next `.await` on it. A no-op for every other agent (nothing
        // ever awaits this `Notify`).
        prompt_notify.notify_one();
        Ok(())
    }

    /// Trips `agent`'s `CancellationToken` via `AgentTree::cancel` (WI-083).
    /// See that method's doc, and this module's reconciliation note, on why
    /// `reason` is recorded (via `tracing`) but cannot yet reach the
    /// resulting `AgentResult`.
    pub fn cancel(&self, agent: AgentId, reason: String) -> Result<(), RuntimeError> {
        self.tree.cancel(agent, reason)
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

    /// The persisted `ContextReport` for `agent`'s historical `turn`
    /// (WI-087). Unlike [`Runtime::context_report`], this always reads the
    /// durable store (`crate::context::report::persisted_at_turn`) rather
    /// than the live `last_report` slot -- history only exists in the
    /// store, since the slot only ever holds the most recent turn. This is
    /// also what makes the method work across a process restart: it
    /// resolves `agent`'s `SessionId` via [`Runtime::resolve_session`],
    /// which falls back to a store scan when `agent` is unknown to this
    /// `Runtime` instance's in-memory `agents` map (e.g. a fresh `Runtime`
    /// over the same store). An out-of-range `turn` returns a typed error
    /// naming the valid range (see `report::persisted_at_turn`'s doc for
    /// the `RuntimeError::Tool(ToolError::Internal)` "closest fit" mapping
    /// this uses, `RuntimeError` having no dedicated variant and
    /// `conway-core/src/error.rs` being out of this item's scope).
    pub async fn context_report_at(
        &self,
        agent: AgentId,
        turn: u32,
    ) -> Result<ContextReport, RuntimeError> {
        let session = self.resolve_session(agent).await?;
        crate::context::report::persisted_at_turn(self.store.as_ref(), agent, &session, turn).await
    }

    /// T3 follow-up: [`Self::context_report`], but closes its documented
    /// gap for an agent this `Runtime` instance attached to (so it is not
    /// `AgentNotFound`) yet has never itself driven a completed turn for --
    /// most commonly a session RESUMED from a prior process, whose
    /// `last_report` slot starts `None` here even though its durable log
    /// already holds turns with real `ContextReportRecord`s. Where
    /// `context_report` would silently return `empty_report` in that case,
    /// this method instead falls back to the most recently PERSISTED report
    /// (`conway_session::provenance::load_all_context_reports`, ascending
    /// seq order, so the last element is the newest) -- one store read, only
    /// on the cold path, never on the hot "already has a live report" one.
    ///
    /// Still returns an empty report for an agent that is genuinely fresh
    /// (started, never completed a turn anywhere) -- there is nothing to
    /// fall back to in that case, and that is not a bug this method exists
    /// to paper over.
    ///
    /// Deliberately additive, not a change to [`Self::context_report`]
    /// itself: that method is synchronous (an in-memory-only read several
    /// existing callers rely on staying `.await`-free — see its own tests),
    /// while the durable fallback here needs `SessionStore::read`.
    pub async fn context_report_current(
        &self,
        agent: AgentId,
    ) -> Result<ContextReport, RuntimeError> {
        let live = {
            let agents = self.agents.read().expect("agents lock poisoned");
            let handle = agents
                .get(&agent)
                .ok_or(RuntimeError::AgentNotFound { agent })?;
            let report = handle.last_report.lock().expect("report lock poisoned").clone();
            report
        };
        if let Some(report) = live {
            return Ok(report);
        }
        let session = self.resolve_session(agent).await?;
        let reports = conway_session::provenance::load_all_context_reports(
            self.store.as_ref(),
            &session,
        )
        .await
        .map_err(RuntimeError::Store)?;
        Ok(reports.into_iter().last().unwrap_or_else(|| empty_report(agent)))
    }

    /// Resolves `agent`'s `SessionId`: first from this instance's
    /// in-memory `agents` map (cheap, the common case), then -- only if
    /// this `Runtime` has no record of `agent` at all -- via a linear scan
    /// of every session the store knows about (`SessionStore::list`),
    /// matching `SessionMeta::agent_id`. `RuntimeError::AgentNotFound` if
    /// neither finds it.
    ///
    /// No indexed `AgentId -> SessionId` lookup exists anywhere in this
    /// workspace today (`SessionFilter` carries no `agent_id` field, and
    /// `conway-session`'s `SessionIndex`, WI-050, does not index on it
    /// either) -- this scan is O(session count) and is an accepted MVP
    /// cost for an inspection API, not a design decision. A dedicated
    /// `conway-session` index is a refinement candidate for `MODULE:
    /// conway-session` if this path ever becomes hot; it is never on the
    /// agent-loop turn path, only on this restart/historical inspection
    /// one.
    async fn resolve_session(&self, agent: AgentId) -> Result<SessionId, RuntimeError> {
        let live = {
            let agents = self.agents.read().expect("agents lock poisoned");
            agents.get(&agent).map(|handle| handle.session)
        };
        if let Some(session) = live {
            return Ok(session);
        }

        // `include_ephemeral: true` -- this is an identity lookup by agent,
        // not a catalog browse, so an ephemeral agent must resolve here too
        // post-restart (its only other route, `self.agents`, is gone once
        // the live map no longer has it) -- mirrors
        // `SessionHandle::resolve_agent_session`'s identical rationale
        // (conway/src/session_handle.rs).
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
            .ok_or(RuntimeError::AgentNotFound { agent })
    }

    /// A snapshot of the whole agent tree (WI-083: `AgentTree::snapshot()`).
    /// Includes every attached agent — every root started so far; children
    /// arrive once WI-084 attaches them.
    pub fn tree(&self) -> AgentTreeSnapshot {
        self.tree.snapshot()
    }

    /// The runtime half of the facade's ephemeral→persistent promote (B3):
    /// flips `agent`'s `ephemeral` flag to `false` in the live tree, then
    /// emits exactly one `Event::AgentPromoted` under the agent's OWN
    /// session, and returns that `SessionId`.
    ///
    /// ORDERING CONTRACT (binding on callers — `conway`'s `Conway::promote`
    /// owns the full sequence): the durable session-header rewrite
    /// (`SessionStore::set_ephemeral`) must have ALREADY succeeded before
    /// this is called. The flag flip strictly precedes the event emission
    /// inside this method, so the event is always the LAST of the three
    /// promote steps — a UI observing `AgentPromoted` may flip its own
    /// cached copy of the flag unconditionally, with no optimistic
    /// pre-flip. `AgentTree` never detaches nodes, so once the facade's
    /// own presence guard has passed this method cannot fail with
    /// `AgentNotFound` in practice; the error return exists because the
    /// tree setter is total over unknown ids (a direct-call bug), not
    /// because a race can remove the node.
    pub fn promote_agent(&self, agent: AgentId) -> Result<SessionId, RuntimeError> {
        let session = self.tree.set_ephemeral(agent, false)?;
        self.bus.emit(session, agent, Event::AgentPromoted {});
        Ok(session)
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

//! `Runtime`: the facade over one agent tree (architecture §4, §7).
//!
//! Owns dependency injection (`RuntimeDeps`), root-agent task lifecycle, and
//! the public surface (`start_root`, `prompt`, `cancel`, `subscribe`,
//! `context_report`, `tree`). `tree()` and `cancel()` are backed by the real
//! [`crate::tree::AgentTree`]: every agent (root, forked, or
//! spawned) is `attach`ed to it, and its task is wrapped by
//! [`crate::supervisor::supervise`] so a panic or a blown deadline still
//! resolves to a terminal result instead of leaving the tree's bookkeeping
//! stuck on `Running` forever. `impl SubagentHost for Runtime` (//! `subagent.rs`) is this crate's fork/spawn entry point; see that module's
//! doc for the fork/spawn procedure and the self-referential-`Arc`
//! construction this file's `new()` sets up for it. See `tree.rs`'s and
//! `supervisor.rs`'s module docs for the guarantees this buys and the one
//! race it does not close.
//!
//! ## Reconciliations against the spec's illustrative types
//!
//! - **`ToolRunner`/`PermissionBroker` construction:** `ToolRunner::new` takes `Arc<PluginRegistry>`
//!   and `Arc<PermissionBroker>`, not the unwrapped values the
//!   prose's illustrative structs might suggest. This item wraps both in
//!   `Arc` at construction, as that review already flagged for this item's
//!   brief.
//! - **`RuntimeDeps` has no `subagents` field:** `LoopDeps::subagents`
//!   (committed) requires an `Arc<dyn SubagentHost>` for every agent
//!   task. Rather than accept this as an injected dependency (
//!   An earlier review found: , an embedder-supplied fake is not a real
//!   dependency, and `conway_testkit::FakeSubagentHost` lives in a
//!   test-only crate this crate's own `[dependencies]` never names (only
//!   `[dev-dependencies]` does), so wiring it into a non-test `Runtime::new`
//!   would be a layering violation either
//!   way), `Runtime::new` now builds the real `subagent::WeakRuntimeHost`
//!   from its own `Weak<Runtime>`, replacing the `NoSubagentHost`
//!   stub this item originally shipped (every method of which returned a
//!   `RuntimeError` naming the gap). See `Runtime::new`'s own doc for why a
//!   `Weak`-backed delegator, not a literal `Arc<Runtime>`, is what
//!   `LoopDeps::subagents` holds.
//! - **An earlier file-scope note:** the work item's own scope section lists
//!   only `subagent.rs`, `agent_loop.rs`, and its test file — not this file.
//!   In practice `impl SubagentHost for Runtime` cannot be wired up without
//!   touching `Runtime::new` (replacing `NoSubagentHost`, adding the
//!   `TranscriptResolver` instance fork resolution needs) and without a
//!   handful of narrow `pub(crate)` accessors (`loop_deps`, `agent_defs`,
//!   `tree_ref`, `resolver`, `agent_session`, `launch_agent`) letting
//!   `subagent.rs` reach state that was, by design, made private to this
//!   module. This is disclosed here as a reconciliation
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
//!   envelopes. An earlier review found: rejected that
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
//!   hardcodes `ResultStatus::Cancelled { reason: "cancelled" }` and
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
//! - **`AgentHandle` sheds its own result channel and cancel
//!   token.** Before this item, `AgentHandle` held its own
//!   `watch::Receiver<Option<AgentResult>>` (populated by a bare
//!   `tokio::spawn` that sent into a paired `Sender` on completion) and its
//!   own `CancellationToken`, and `tree()`/`cancel()` read and
//!   wrote them directly. Both are now owned by `AgentTree` instead (a
//!   `start_root` agent is `attach`ed to it exactly like a future
//!   child would be, with `kind: None` since a root is started, not
//!   spawned — see `tree.rs`'s module doc), so `AgentHandle` keeps only
//!   what nothing else already tracks: the session id (for `prompt`) and
//!   the live report slot (for `context_report`). Routing both channels
//!   through one owner is also what makes `tree().nodes[].status` accurate
//!   for a finished root agent, which the old per-`AgentHandle` channel,
//!   never read by the `tree()` stub that preceded it, did not actually provide.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use chrono::Utc;
use conway_core::agent::{AgentDefRef, AgentResult, AgentTreeSnapshot, Budget, ToolSelector};
use conway_core::capabilities::{CacheMode, HeadroomPolicy};
use conway_core::config::{AgentDef, DEFAULT_MAX_PARALLEL_TOOLS};
use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::error::{ConwayError, RuntimeError};
use conway_core::event::Event;
use conway_core::ids::{
    AgentId, BackendId, LogSeq, ModelRef, RoleAlias, SeqRange, SessionId, ToolName,
};
use conway_core::log::{ForkOrigin, LogRecord, SessionFilter, SessionMeta, SubagentMode};
use conway_core::ports::{
    Backend, ContextHook, HealthRegistry, HookRunner, PermissionGate, Plugin, PluginConfig,
    PluginEventEmitter, RegisteredObserver, Router, SessionStore, SubagentHost,
};
use conway_core::provenance::{ContextReport, Provenance};
use conway_core::segment::CacheTtl;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{AgentLoop, AgentSpec, LoopDeps};
use crate::attempt::AttemptEngine;
use crate::context::{ContextBuilder, GuardedContextHook, InheritedPrefix, TOKEN_ESTIMATOR};
use crate::events::{EventBus, EventStream};
use crate::mailbox::{self, Mailbox, MailboxSender};
use crate::permission::{PermissionBroker, PreToolUseHookSpec};
use crate::supervisor::{self, SuperviseArgs};
use crate::tools::{PluginRegistry, ToolRunner};
use crate::tree::{AgentNode, AgentTree};

mod root;
pub use root::{ResumeSpec, RootSpec};

/// Every port-shaped dependency the runtime needs, injected by the facade
/// (or, in tests, built entirely from `conway-testkit`'s fakes). Nothing here
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

/// Everything the runtime keeps about one live (or finished-but-not-yet-
/// evicted) agent task that isn't already tracked by `Runtime::tree`
/// (— see the module doc's reconciliation note). Looked up by
/// reference, never cloned wholesale, so `agents`'s `RwLock` is never held
/// across an `.await`.
struct AgentHandle {
    session: SessionId,
    /// This agent's mailbox sender — cloned out by
    /// [`Runtime::agent_mailbox`] for `subagent.rs`'s `steer` and for a
    /// fork/spawn child's `parent_mailbox`.
    mailbox: MailboxSender,
    last_report: Arc<Mutex<Option<ContextReport>>>,
    /// (generalized by keep-alive): the same `Arc` as this agent's
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
    /// The observation-only hook tier (`post_tool_use`, `session_starting`,
    /// `child_spawned`) --. Shared with
    /// `LoopDeps.tool_runner`, which dispatches `post_tool_use` from inside
    /// the tool batch; this handle is what `start_root` and
    /// `impl SubagentHost for Runtime`'s `start` use for the other two, and
    /// what `set_observation_hooks` writes config onto.
    hooks: Arc<crate::hook_dispatch::HookDispatcher>,
    loop_deps: Arc<LoopDeps>,
    agents: RwLock<HashMap<AgentId, AgentHandle>>,
    tree: Arc<AgentTree>,
    /// Ancestry resolution for the fork path (`subagent.rs`) --
    /// `conway_session::TranscriptResolver`, one instance per runtime so
    /// sibling forks share memoized prefixes (see that type's own module
    /// doc). Not part of `RuntimeDeps`: it needs no injected configuration
    /// beyond a cache capacity, and adding a field to `RuntimeDeps` would
    /// be a breaking change to the already-committed, criterion-pinned
    /// surface for a value this crate can construct unconditionally itself.
    resolver: Arc<conway_session::TranscriptResolver>,
}

/// Entry count for `Runtime`'s `TranscriptResolver` cache. No criterion
/// pins this value; `conway-session`'s own test suite exercises capacities
/// from 2 to 64 without treating the number itself as load-bearing, so this
/// picks a generous-but-bounded default rather than inventing a config
/// surface this type has no mandate to add.
const TRANSCRIPT_CACHE_CAPACITY: usize = 512;

impl Runtime {
    /// Builds a runtime from injected ports. Panics if `deps.plugins`
    /// contains a duplicate tool name across plugins — a malformed plugin
    /// set is a registration bug, not a runtime condition (matches
    /// `PluginRegistry::from_plugins`'s own construction-time-error
    /// contract; `Runtime::new`'s binding signature is infallible, so this
    /// is the only place the check can surface).
    ///
    /// ## Reconciliation: self-referential `subagents`
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

        // Collected before `from_plugins` consumes the set. Each observer is
        // paired with its own plugin's manifest id here and nowhere else, so
        // the events it later fires resolve to that plugin's namespace and
        // cannot be made to resolve to another's -- the same rule
        // `ToolCtx::plugin_events` already applies to a plugin's own tools.
        let observers: Vec<RegisteredObserver> = plugins
            .iter()
            .flat_map(|plugin| {
                let plugin_id = plugin.manifest().id;
                plugin
                    .observers()
                    .into_iter()
                    .map(move |observer| RegisteredObserver {
                        plugin_id: plugin_id.clone(),
                        observer,
                    })
            })
            .collect();

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
        // Read back rather than constructed here, so the runtime and the tool
        // runner share ONE dispatcher: `post_tool_use` fires from inside
        // `ToolRunner`, while `session_starting` and `child_spawned` fire from
        // `Runtime`'s own methods, and all three must see the same injected
        // runner and subscription lists.
        let hooks = tool_runner.hooks();
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
                // no `RuntimeDeps` field sources this (out of that
                // item's file scope to add here) -- `set_context_hook`
                // below fills it in post-construction. `None` here
                // preserves every existing caller's behavior unchanged.
                context_hook: RwLock::new(None),
                observers,
                // The SAME dispatcher `post_tool_use` and every other
                // plugin-declared event already fan out through, so an
                // observer's events and a tool's events share one path.
                plugin_events: hooks.clone() as Arc<dyn PluginEventEmitter>,
            });

            Runtime {
                store,
                bus: event_bus,
                agent_defs,
                registry,
                broker,
                hooks,
                loop_deps,
                agents: RwLock::new(HashMap::new()),
                tree,
                resolver,
            }
        })
    }

    /// Everything `subagent.rs`'s `impl SubagentHost for Runtime`
    /// needs that isn't reachable through `loop_deps()`'s already-`pub`
    /// fields. Kept as narrow, crate-private accessors rather than widening
    /// any field's visibility, so `Runtime`'s actual public surface (the
    /// thing the criterion "no additional public methods" on the
    /// trait impl is protecting) is unaffected.
    /// The shared observation-hook dispatcher, for `subagent.rs`'s
    /// `impl SubagentHost for Runtime`.
    /// `pub(crate)` for the same reason [`Self::loop_deps`] is: an internal
    /// seam between two files of one crate, not public surface.
    pub(crate) fn observation_dispatcher(&self) -> &Arc<crate::hook_dispatch::HookDispatcher> {
        &self.hooks
    }

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

    /// Registers (or clears, via `None`) the `ContextHook` every
    /// agent task under this runtime consults before each LLM request and
    /// on T-1 overflow (`AgentLoop::route_and_attempt`). A new, purely
    /// additive public method rather than a `RuntimeDeps` field: `RuntimeDeps`
    /// is out of this item's file scope, and is also constructed by field
    /// literal in several existing tests that a new required field would
    /// have broken. Intended to be called once, by the facade
    /// (`conway::ConwayBuilder::build`), before any session is started;
    /// safe to call at any time regardless, since every turn reads the
    /// current value fresh (`AgentLoop::context_hook`).
    ///
    /// **The one place a hook enters this runtime, which is why the wrap
    /// happens HERE** (board item `01M00RGARPESWXYAVY960KDE7S`, `INTENT.md`
    /// §8.6): `hook` arrives as an ordinary, un-self-checking `Arc<dyn
    /// ContextHook>` -- exactly what every implementation looks like, since
    /// coherence-checking was never part of the trait's contract -- and
    /// leaves as a `GuardedContextHook`, the only thing `LoopDeps::
    /// context_hook`'s field type can hold. Neither of `AgentLoop`'s two
    /// call sites constructs one themselves; there is no unwrapped hook for
    /// them to reach.
    pub fn set_context_hook(&self, hook: Option<Arc<dyn ContextHook>>) {
        *self
            .loop_deps
            .context_hook
            .write()
            .expect("context_hook lock poisoned") =
            hook.map(|inner| Arc::new(GuardedContextHook::new(inner)));
    }

    /// Registers (or clears, via
    /// `None`) the `HookRunner` [`PermissionBroker::decide`]'s `pre_tool_use`
    /// step consults, at the SAME deny tier as `deny_matches` -- see that
    /// method's own doc for why the placement, not merely the existence, is
    /// what makes a denying hook enforceable under every permission mode,
    /// `AutoAllow` included.
    ///
    /// Mirrors [`Self::set_context_hook`]'s own shape exactly, for the
    /// identical reason: `RuntimeDeps` is out of this item's file scope
    /// (also constructed by field literal in several existing tests a new
    /// required field would break), so this is a purely additive
    /// post-construction setter rather than a new `RuntimeDeps` field. Not
    /// called at all (the default) leaves `PermissionBroker::decide`
    /// byte-for-byte unchanged from before this item -- the broker's own
    /// `hook_runner` field defaults to `None`, which the hook-check step
    /// treats as "nothing to consult" before it performs any I/O or even
    /// reads the installed hook list.
    ///
    /// `conway::ConwayBuilder::with_hook_runner` is this method's own
    /// facade-level caller; `conway-runtime` itself never constructs a
    /// concrete `HookRunner` (: this
    /// crate must not depend on `conway-tools` to reach one -- the runner
    /// arrives here as an already-constructed `Arc<dyn HookRunner>`, a
    /// sibling crate's concern, not this one's).
    pub fn set_hook_runner(&self, runner: Option<Arc<dyn HookRunner>>) {
        self.broker.set_hook_runner(runner);
    }

    /// Installs the `pre_tool_use`
    /// hook specs [`Self::set_hook_runner`]'s dispatcher consults, wholesale
    /// -- see [`PermissionBroker::set_pre_tool_use_hooks`]'s own doc.
    /// `conway::ConwayBuilder::build` is this method's own caller, computing
    /// the list once from `[hooks].rules[]` filtered to `event ==
    /// "pre_tool_use" && enabled` before any session starts. Not called at
    /// all (the default, an empty list) is the same no-op
    /// `Self::set_hook_runner(None)` is.
    /// Injects the same `HookRunner` into the observation tier
    /// (`post_tool_use`, `session_starting`, `child_spawned`) that
    /// [`Self::set_hook_runner`] gives the permission broker.
    ///; `conway::ConwayBuilder::build` calls both
    /// with the same runner, so an operator injecting one gets every event.
    /// Not called at all leaves every observation dispatch a no-op.
    pub fn set_observation_hook_runner(
        &self,
        runner: Option<Arc<dyn conway_core::ports::HookRunner>>,
    ) {
        self.hooks.set_runner(runner);
    }

    /// Replaces the observation tier's subscription lists wholesale, keyed by
    /// event name -- the observation counterpart of
    /// [`Self::set_pre_tool_use_hooks`]. See
    /// [`crate::hook_dispatch::HookDispatcher::set_hooks`].
    pub fn set_observation_hooks(
        &self,
        hooks: std::collections::BTreeMap<String, Vec<crate::hook_dispatch::HookSpec>>,
    ) {
        self.hooks.set_hooks(hooks);
    }

    pub fn set_pre_tool_use_hooks(&self, hooks: Vec<PreToolUseHookSpec>) {
        self.broker.set_pre_tool_use_hooks(hooks);
    }

    /// Every currently-installed `pre_tool_use` hook spec -- the review-list
    /// counterpart of [`Self::set_pre_tool_use_hooks`]. See
    /// [`PermissionBroker::active_pre_tool_use_hooks`]'s own doc.
    pub fn pre_tool_use_hooks(&self) -> Vec<PreToolUseHookSpec> {
        self.broker.active_pre_tool_use_hooks()
    }

    /// The observation tier's whole subscription map -- the review-list
    /// counterpart of [`Self::set_observation_hooks`] , including `prompt_submitted`'s own
    /// entry (the deny-capable event this tier also dispatches -- see
    /// `crate::hook_dispatch`'s own module doc). See
    /// [`crate::hook_dispatch::HookDispatcher::hooks_snapshot`]'s own doc
    /// for why this returns the WHOLE map rather than one event's list.
    pub fn observation_hooks(
        &self,
    ) -> std::collections::BTreeMap<String, Vec<crate::hook_dispatch::HookSpec>> {
        self.hooks.hooks_snapshot()
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
    /// inlines, factored out so `subagent.rs`'s `start` can resolve
    /// a fork/spawn `parent`'s session without duplicating it.
    pub(crate) fn agent_session(&self, agent: AgentId) -> Result<SessionId, RuntimeError> {
        let agents = self.agents.read().expect("agents lock poisoned");
        Ok(agents
            .get(&agent)
            .ok_or(RuntimeError::AgentNotFound { agent })?
            .session)
    }

    /// A clone of `agent`'s mailbox sender. Used by `subagent.rs`'s
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
    /// this sequence) and the fork/spawn path (`subagent.rs`), which
    /// has no other way to reach `agents`/`tree`/`bus` to do this itself
    /// without those fields losing their private visibility.
    ///
    /// `mailbox` is the sender half of the mailbox `agent_loop.inbox`
    /// already owns the receiver half of -- constructed by the
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
        // `agent_loop` already carries its own `resume_gate.notify`
        // (a real `resume_root` gate, or an unused `Default` one for every
        // other caller of this shared path -- currently only `subagent.rs`'s
        // fork/spawn) -- clone the same `Arc` out before `agent_loop` moves
        // into its spawned task below, so `Runtime::prompt` has something to
        // notify.
        let prompt_notify = agent_loop.resume_gate.notify.clone();
        // read out BEFORE
        // `agent_loop` moves into the spawned task below -- `AgentId` is
        // `Copy` (`ids.rs`'s `ulid_id!`), so this is an ordinary copy, not
        // a partial move that would make the later whole-struct move
        // illegal.
        let parent = agent_loop.parent;

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
            hooks: self.hooks.clone(),
            parent,
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

    /// Appends a `LogRecord::UserTurn` to `agent`'s session before returning
    /// (persist-before-act), then emits the live `Event::UserTurn` twin (this
    /// item) so a subscriber on the event stream sees the SAME occurrence
    /// live that replay would later reconstruct from the log -- closing the
    /// The gap where only the TUI (via its own local `Entry::User` push) ever
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
    /// conversation picks it up) is the mailbox wiring; this method only
    /// guarantees the durable append plus this live broadcast.
    pub async fn prompt(&self, agent: AgentId, text: String) -> Result<(), RuntimeError> {
        let (session, prompt_notify) = {
            let agents = self.agents.read().expect("agents lock poisoned");
            let handle = agents
                .get(&agent)
                .ok_or(RuntimeError::AgentNotFound { agent })?;
            (handle.session, handle.prompt_notify.clone())
        };

        // `prompt_submitted` for a FOLLOW-UP prompt on a live session (board
        // item). After the session is resolved, so
        // the payload can name it, but BEFORE the `store.append` below -- a
        // denied prompt must leave no record behind, exactly as if it had
        // never been typed.
        //
        // `text` is passed by reference and returned to the caller untouched.
        // Nothing here can rewrite it: `dispatch_deny_only` reads only
        // `HookPermissionVerdict`, which has no field capable of carrying
        // replacement text.
        if let Some(reason) = self
            .hooks
            .dispatch_deny_only(
                crate::hook_dispatch::PROMPT_SUBMITTED,
                serde_json::json!({
                    "text": text,
                    "agent_id": agent,
                    "session": session,
                    "first_prompt": false,
                }),
            )
            .await
        {
            return Err(RuntimeError::PromptDenied { reason });
        }
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
        // Generalized by keep-alive: wakes a `resume_root` agent's
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

    /// Trips `agent`'s `CancellationToken` via `AgentTree::cancel`.
    /// This is the immediate half of `conway_cancel`/`SessionHandle::
    /// cancel_with`'s two modes -- see `CancelMode`'s own doc for the other.
    ///
    /// `reason` is recorded via `tracing` (unchanged) AND now reaches `agent`'s own terminal
    /// `AgentResult` (`ResultStatus::Cancelled { reason }`), the same way
    /// the graceful path's mailbox-delivered reason always has -- see
    /// `AgentTree::cancel`'s own doc for the storage mechanism and both
    /// read-back sites: `AgentLoop::finish_cancelled` for the ordinary
    /// loop-boundary case, and
    /// `AgentLoop::finish_error` for the narrower case where the cancel is
    /// instead observed mid-request, inside a backend call.
    ///
    /// This guarantee is scoped to `agent` ITSELF, not the whole subtree
    /// this call structurally cancels: every child's `CancellationToken` is
    /// a `child_token()` of its parent's (`tree.rs`), so a hard cancel on an
    /// ancestor trips every descendant's token too, but only `agent` was
    /// ever actually named in this call -- a descendant's own result falls
    /// back to a generic "cancelled" reason, since attributing `reason` to
    /// an agent that was never told it would misrepresent where it came
    /// from. Whether the subtree collapse itself should carry a
    /// reason down to every descendant is a separate, open question (board
    /// item), not decided here.
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
    ///. Unlike [`Runtime::context_report`], this always reads the
    /// durable store (`crate::context::report::persisted_at_turn`) rather
    /// than the live `last_report` slot -- history only exists in the
    /// store, since the slot only ever holds the most recent turn. This is
    /// also what makes the method work across a process restart: it
    /// resolves `agent`'s `SessionId` via `Runtime::resolve_session`,
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
            let report = handle
                .last_report
                .lock()
                .expect("report lock poisoned")
                .clone();
            report
        };
        if let Some(report) = live {
            return Ok(report);
        }
        let session = self.resolve_session(agent).await?;
        let reports =
            conway_session::provenance::load_all_context_reports(self.store.as_ref(), &session)
                .await
                .map_err(RuntimeError::Store)?;
        Ok(reports
            .into_iter()
            .last()
            .unwrap_or_else(|| empty_report(agent)))
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
    /// `conway-session`'s `SessionIndex`, does not index on it
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

    /// A snapshot of the whole agent tree (`AgentTree::snapshot()`).
    /// Includes every attached agent — every root started so far; children
    /// arrive once something attaches them.
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
        dropped: Vec::new(),
    }
}

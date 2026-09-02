//! `AgentLoop`: the per-agent turn state machine (architecture §7).
//!
//! Wires `ContextBuilder` -> `Router` -> `AttemptEngine` -> `ToolRunner` ->
//! `SessionStore` into one turn, with budgets and terminal-result
//! construction. `LoopDeps::subagents` is the real `Runtime` (see
//! `subagent.rs`), not a stub -- `ToolBatchCtx` gets a working host.
//!
//! ## mailboxes and steering
//!
//! `drain_inbox` (previously a documented no-op hook) now really drains
//! this agent's inbox at every turn boundary and classifies what it finds
//! (`crate::mailbox::classify`) -- see that function's doc and
//! `drain_inbox`'s own doc for the turn-boundary and "no injection outside
//! drain_inbox" guarantees this buys. `AgentLoop` gained three fields:
//! `inbox` (this agent's own `MailboxReceiver`), `parent_mailbox` (used to
//! deliver this agent's terminal `Result` upward on `finish`), and
//! `pending_cancel` (turn-local bookkeeping a drained soft `Cancel`
//! resolves into). A drained `Result` is classified but drives no
//! drain-time action -- An earlier review found: removed the never-
//! populated `pending_subagent` map this used to resolve; see
//! `drain_inbox`'s own doc and `mailbox.rs`'s module doc for why
//! `AgentTree::await_result` is the real, and only, resolution
//! path. `LoopDeps` gained `tree`, used both to close the carried
//!/ double-`AgentFinished` race in `finish` (this file's
//! half of a two-sided fix -- see `supervisor.rs`'s module doc for the
//! other half) -- see `finish`'s own doc.
//!
//! ## `inherited` context (superseded by `resolve_default_path`, D1-3d-wire)
//!
//! `AgentLoop` carries `inherited: Option<InheritedPrefix>`, resolved once at
//! fork time by `subagent.rs`'s `SubagentHost::start` (via
//! `conway_core::transcript::TranscriptResolver`, before any of the child's
//! own records exist). Every turn's path assembly used to hand it to
//! `path_from_legacy` unchanged; as of this item it calls
//! `context::path::resolve_default_path` instead, which re-derives the same
//! inherited prefix itself, every turn, straight from the session's own
//! `meta.origin` (read via the store) rather than from this cached field --
//! see that function's own doc (`context/path.rs`, step 3) for the
//! ancestry-walk fix that made this safe to wire in. `self.inherited` and its
//! construction sites (`subagent.rs`, `runtime/root.rs`) stay unchanged --
//! only `run_inner` stops reading the field, it is not dead: `subagent.rs`
//! also derives `inherited_upto` (`AgentNode` tree bookkeeping, unrelated to
//! context assembly) from the SAME fork-time resolve call, so removing the
//! construction site would take that with it too. `path_from_legacy` is
//! kept as a cheaper, store-free test fixture builder (see its own doc).
//!
//! `AgentSpec::report_slot` (An earlier review found: ) is this item's
//! one additive hook for a live caller: after each successful
//! `ContextBuilder::build`, and before that turn's backend call, the loop
//! pushes a clone of the just-built `ContextReport` into the slot if the
//! caller supplied one. This is the only channel through which a turn's
//! report reaches outside the loop — no event-bus reconstruction is
//! involved.
//!
//! ## Reconciliations against the amendment's illustrative types
//!
//! The amendment's prose assumes a runtime-local `HeadroomPolicy` (in a
//! `headroom.rs` this item would create) and a `RouteRequest.required.
//! min_context` field carrying `est_tokens + headroom`. Neither exists in
//! the committed workspace:
//! - `HeadroomPolicy` is `conway_core::capabilities::HeadroomPolicy` (already
//!   committed; relocated out of `conway-routing`'s `config` module
//!   into `conway-core` by a later item, so this
//!   engine no longer needs to depend on the whole routing crate for it) —
//!   reused directly rather than duplicated.
//! - `conway_core::routing::RequiredCaps` has no `min_context: u32` total;
//!   it has `min_context: Option<u32>` (an independent absolute floor,
//!   unrelated to headroom) and `headroom_tokens: u32` (the headroom
//!   value itself). This loop sets `required.headroom_tokens` to the
//!   turn's resolved headroom instead. `DeclarativeRouter`
//!   documents that it never actually reads this field back (it resolves
//!   headroom itself from its own compiled config) — this loop sets it
//!   anyway so a `RouteRequest` is a complete, honest description of what
//!   the turn asked for, and so alternate `Router` implementations that do
//!   honor it see the same value the attempt engine's gate uses.
//! - Intra-loop consistency: `est_tokens` and `headroom` are each resolved
//!   exactly once per turn, into locals, and both `RouteRequest` and
//!   `AttemptRequest` are built from those same two locals. This does NOT
//!   extend to the real `DeclarativeRouter`'s own filter when
//!   `AgentSpec.headroom_override` diverges from the policy value: the
//!   router resolves headroom from its own compiled config and ignores the
//!   request field, so an override is honored only by the attempt engine's
//!   backstop gate. The divergence fails safe (a
//!   spurious rejection at one gate, never corrupted output); plumbing
//!   per-agent overrides into `DeclarativeRouter` is queued as a follow-up,
//!   and callers must not rely on `headroom_override` affecting routing
//!   decisions until it lands.
//!
//! Event ordering also reconciles the amendment's step-9 prose ("run tools,
//! emit `TurnFinished`") against this item's own binding criterion
//! (`TurnStarted < ModelDecision < TextDelta* < TurnFinished <
//! ToolCallProposed*`): `TurnFinished` is emitted immediately after the
//! assistant record is appended, before any tool call is dispatched. A
//! "turn" is one model generation; tool execution feeds the *next* turn's
//! context, not the current one's completion event.
//!
//! ## `AgentResult` construction and repeated-step detection
//!
//! `finish` no longer builds its `AgentResult` from a raw `summary` string
//! alone: it resolves a [`crate::result::ResultBuilder`] (report-tool
//! precedence over trailing text, non-empty-summary/status-naming
//! fallback) for `summary`/`facts`/`artifacts`/`structured` on every
//! terminal path. The tool-outcome loop also runs every dispatched call
//! offers every dispatched call to each registered
//! [`conway_core::ports::ToolObserver`] and appends whatever notes they
//! return -- the loop holds no detection policy of its own, and with no
//! observing plugin installed that pass does not execute at all.
//! plus an injected `SystemNote` the instant a `(tool, canonical-args)`
//! digest is seen a 3rd time. Both are locals inside `AgentLoop::run_inner`,
//! not new fields on `AgentLoop`/[`AgentSpec`] -- see `result.rs`'s module
//! doc for why (both structs are constructed via field literals in files
//! outside this item's original scope: `runtime.rs`, `subagent.rs`, and
//! existing tests).
//!
//! ## Non-natural terminations report what the agent actually did
//!
//! Board item `01M1FQ3TGHMRC9EECN4JX0MXM3`'s extension. Before this item,
//! every terminal path OTHER than a natural `Completed`/`Rejected` --
//! budget-exceeded (all four dimensions), cancellation (graceful and
//! immediate), a deadline, and a bubbled-up backend/store failure -- called
//! [`Self::finish`] with a literal `""` trailing text. A run that had
//! already done real, disk-visible work then reported
//! `"(no output; terminal status: <name>)"`, indistinguishable from a run
//! that had done nothing at all -- the incident this item documents cost an
//! operator real time finding 69 lines of uncommitted, unreported work by
//! hand. `LoopState` gained `last_assistant_text` (the most recent backend
//! response's own text, captured every turn -- see that field's own doc),
//! and every one of the ten `""`-passing sites now calls
//! [`Self::terminal_account`] instead, which resolves that text, or an
//! explicit "stopped mid-run" marker when there is no text but other
//! evidence of real work, or `""` (genuinely unchanged) only when NEITHER
//! holds. See `terminal_account`'s own doc for the full precedence.
//!
//! `AgentSpec` gained one field this item, `result_contract:
//! Option<schemars::schema::RootSchema>`, carried through from
//! `SubagentSpec::result_contract` by `subagent.rs`'s `SubagentHost::start`
//! (`None` for a root agent -- `runtime.rs`'s `start_root` has no
//! `SubagentSpec` to source one from). Adding it forced one-line, inert
//! `result_contract: None,` additions to `runtime.rs` and the two existing
//! test harnesses (`tests/agent_loop_e2e.rs`, `tests/steering.rs`) that
//! construct `AgentSpec` by field literal -- a file-scope extension the
//! coordinator explicitly authorized (this item's Self-Check) after the
//! initial implementation flagged the conflict rather than silently
//! expanding scope. The natural-completion branch of `AgentLoop::run_inner`
//! enforces the contract when present: `Ok` proceeds to `Completed`;
//! the first failure appends a `SystemNote { reason:
//! "result_contract_violation" }` and gives the agent one more turn
//! (`contract_retried` flips `true`, a local exactly like `result_builder`/
//! `result_builder`); a second failure is terminal,
//! `ResultStatus::Rejected { missing }`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::Utc;
use conway_core::agent::{AgentMessage, AgentResult, Budget, ResultStatus, ToolSelector};
use conway_core::capabilities::{CacheMode, HeadroomPolicy, RequiredCaps, ToolCallSupport};
use conway_core::content::{ContentBlock, ToolResult, ToolSpec, Usage};
use conway_core::error::{ConwayError, RoutingError, RuntimeError};
use conway_core::event::Event;
use conway_core::ids::{AgentId, ModelId, ModelRef, RoleAlias, SessionId};
use conway_core::log::LogRecord;
use conway_core::path::ResolvedPath;
use conway_core::ports::{
    ArtifactWriteHandle, ContextHookCtx, ContextPayload, CurateCtx, CwdHandle, ObservedCall,
    ObserverCtx, OverflowInfo, PathStore, PluginConfig, PluginEventEmitter, PluginEventHandle,
    RegisteredObserver, Router, SessionStore, SubagentHost,
};
use conway_core::provenance::{ContextReport, Provenance};
use conway_core::routing::RouteRequest;
use conway_core::segment::{CacheTtl, PromptSegment};
use tokio_util::sync::CancellationToken;

use crate::attempt::{AttemptEngine, AttemptOutcome, AttemptRequest};
use crate::context::path::resolve_default_path;
use crate::context::{
    ContextBuilder, ContextInput, GuardedContextHook, InheritedPrefix, PluginInstruction,
    SkillFragment, SystemPromptSpec,
};
use crate::events::EventBus;
use crate::mailbox::{self, MailboxReceiver, MailboxSender};
use crate::result::{validate_result_contract, ContractOutcome, ResultBuilder};
use crate::tools::{PluginRegistry, ToolBatchCtx, ToolRunner};
use crate::tree::AgentTree;

/// The per-agent turn loop's static configuration: everything about *this*
/// agent that does not change turn to turn.
#[derive(Clone, Debug)]
pub struct AgentSpec {
    pub system_prompt: Option<SystemPromptSpec>,
    /// Plugin-declared instruction fragments (board item
    /// `01M0K5MD59YZRSHE31JKZKFRMY`), rendered before `skills` -- see
    /// `ContextInput::instructions`'s own doc for the precedence argument.
    /// Resolved by `runtime::root::resolve_instructions` for EVERY agent --
    /// root, resumed, forked, or spawned (board item
    /// `01M0VSKA76NSEHDSH25XJGJ2J5`'s ruling: an instruction fragment is
    /// harness configuration keyed to tool reachability, not transcript
    /// context, so fork/spawn's inheritance split does not govern it --
    /// full argument at that function's own doc). `subagent.rs`'s
    /// `SubagentHost::start` calls the SAME function a root agent's
    /// construction does, with no per-mode branch.
    pub instructions: Vec<PluginInstruction>,
    pub skills: Vec<SkillFragment>,
    /// `None` behaves as [`ToolSelector::All`] (see
    /// [`PluginRegistry::specs`]).
    pub tools: Option<ToolSelector>,
    pub role: RoleAlias,
    pub pin: Option<ModelRef>,
    pub budget: Budget,
    pub cache_mode: CacheMode,
    pub cache_ttl: CacheTtl,
    /// Overrides the resolved headroom for every turn of this agent's run.
    /// Resolution order: `headroom_override` -> `HeadroomPolicy::resolve`.
    pub headroom_override: Option<u32>,
    pub max_parallel_tools: usize,
    /// The live slot `Runtime::context_report` reads from. Pushed
    /// into by this loop after every successful `ContextBuilder::build`,
    /// before the turn's backend call — so a caller reading the slot always
    /// sees the most recently *assembled* context, independent of whether
    /// that turn's attempt has completed yet. `None` in contexts with no
    /// caller listening (e.g. some tests construct an `AgentLoop` directly).
    pub report_slot: Option<Arc<Mutex<Option<ContextReport>>>>,
    /// The schema a `structured` result must satisfy, carried
    /// through from `SubagentSpec::result_contract` (`subagent.rs`'s
    /// `SubagentHost::start`) for a fork/spawn child; `None` for a root
    /// agent (`runtime.rs`'s `start_root` has no `SubagentSpec` to source
    /// one from) and for any `AgentSpec` a test constructs directly without
    /// opting in. Enforced once per natural-completion attempt in
    /// `Self::run_inner` -- see this file's module doc.
    pub result_contract: Option<schemars::schema::RootSchema>,
    /// Opt-in multi-turn keep-alive (fixes the confirmed bug where a live
    /// session's task terminates after one prompt-to-completion turn, so a
    /// SECOND `Runtime::prompt` on the same session silently never runs --
    /// see this file's `run_inner` doc on the natural-completion branch for
    /// the mechanism). `false` for every caller except `runtime.rs`'s
    /// `start_root` (and only when `RootSpec::keep_alive` opts in): a
    /// resumed root and every fork/spawn child must terminate on
    /// `Completed` exactly as before, since a parent awaiting a spawned or
    /// forked child's terminal `AgentResult` (`AgentTree::await_result`,
    /// depends on that child actually terminating -- making this
    /// universal would hang such a parent forever.
    pub keep_alive: bool,
    /// The opaque consumer tag
    /// threaded straight from `conway_core::agent::SubagentSpec::tag` by
    /// `subagent.rs`'s `SubagentHost::start` (`None` for a root or resumed
    /// root -- `runtime.rs`'s `start_root`/`resume_root` have no
    /// `SubagentSpec` to source one from). This loop never reads it for any
    /// decision; it exists solely to be cloned into every turn's
    /// `ContextHookCtx::tag` (see that field's own doc) -- see
    /// `SubagentSpec::tag`'s own doc for the full "conway never interprets
    /// this" guarantee and why it is a genuinely new kind of field.
    ///
    /// Required, not defaulting: like `ContextHookCtx::agent_path`,
    /// `AgentSpec` derives no `Serialize`/
    /// `Deserialize` and has no wire format to preserve compatibility with,
    /// so there is no serialization justification for a silent default --
    /// and a field whose entire purpose is telling two otherwise-identical
    /// agents apart must not hand every construction site (tests included)
    /// an identical `None` for free.
    pub tag: Option<String>,
}

/// Everything an [`AgentLoop`] needs beyond its own identity and spec:
/// store, router, attempt engine, tool dispatch, and the event bus. Shared
/// across every agent task in a runtime (cheap to clone: every field is an
/// `Arc`).
pub struct LoopDeps {
    pub store: Arc<dyn SessionStore>,
    /// Backing store for named/frozen context selections (DESIGN §2.5) --
    /// see `RuntimeDeps::path_store`'s own doc (`runtime.rs`) for what
    /// sources it. `run_inner`'s per-turn path assembly hands this straight
    /// to `resolve_default_path` alongside `store` and `resolver`.
    pub path_store: Arc<dyn PathStore>,
    pub router: Arc<dyn Router>,
    pub attempt: Arc<AttemptEngine>,
    pub registry: Arc<PluginRegistry>,
    pub tool_runner: Arc<ToolRunner>,
    /// Handed to every dispatched tool's `ToolCtx`. No subagent
    /// implementation exists yet; a fake or a not-yet-wired real
    /// host is injected by the caller.
    pub subagents: Arc<dyn SubagentHost>,
    /// The context-path composition capability (decision
    /// `01M0K4QT6MBXPD6PXMBBBD2P7B`) every turn's `ToolBatchCtx` threads
    /// straight through to `ToolRunner::run_batch` -- see `ToolBatchCtx::
    /// context_path_host`'s own doc. One runtime-wide instance
    /// (`Runtime::new`), narrowed per call to that call's own session.
    pub context_path_host: Arc<dyn conway_core::ports::ContextPathHost>,
    /// The cross-session discovery capability (board item
    /// `01M0PS8J3AK7Z7253Z3E3RD3GY`) every turn's `ToolBatchCtx` threads
    /// straight through to `ToolRunner::run_batch`, mirroring
    /// `context_path_host` immediately above exactly -- one runtime-wide
    /// instance (`Runtime::new`), never narrowed to a session (discovery is
    /// cross-session by construction).
    pub session_discovery_host: Arc<dyn conway_core::ports::SessionDiscoveryHost>,
    /// Edge B's plugin -> plugin capability CALL channel (board item
    /// `01M0XXWV3BVDM6Y646WMEBTYT1`) every turn's `ToolBatchCtx` threads
    /// straight through to `ToolRunner::run_batch`, mirroring
    /// `context_path_host`/`session_discovery_host` immediately above
    /// exactly: one runtime-wide instance (`Runtime::new`, sourced from
    /// `RuntimeDeps::capabilities`), narrowed per call to that call's own
    /// resolved tool's declaring plugin id at the dispatch seam
    /// (`conway_runtime::tools::runner`), never here.
    pub capabilities: Arc<dyn conway_core::ports::CapabilityHost>,
    pub plugin_config: Arc<PluginConfig>,
    pub bus: Arc<EventBus>,
    pub builder: Arc<ContextBuilder>,
    pub headroom: Arc<HeadroomPolicy>,
    /// The agent tree this agent belongs to. Carried follow-up
    /// (/): lets `finish` consult the tree's set-once
    /// publication before emitting `Event::AgentFinished`, closing the
    /// benign double-emit race against the supervisor's own grace-timeout
    /// synthesis -- see `finish`'s own doc and `supervisor.rs`'s module doc
    /// ("the narrow race this module does not close").
    pub tree: Arc<AgentTree>,
    /// The memoised effective-transcript resolver shared with the runtime
    /// (architecture §4.4). Held as `Arc` because `TranscriptResolver` is
    /// not `Clone` (it owns a `Mutex`-backed LRU cache). Threaded into every
    /// turn's [`CurateCtx`] so a curator can resolve any session's effective
    /// transcript (§11.5) without re-walking the ancestry each call.
    pub resolver: Arc<conway_core::transcript::TranscriptResolver>,
    /// Pluggable pre-assembly context curation (DESIGN-context-path §11.4).
    /// `RwLock` rather than a plain `Option` for the SAME reason
    /// [`Self::context_hook`] is: `RuntimeDeps` (runtime.rs, out of this
    /// item's file scope) has no field to source one from at `LoopDeps`
    /// construction time -- `Runtime::set_context_curator` (a new, purely
    /// additive method) sets this post-construction, before any agent
    /// starts running, and every turn reads it fresh via
    /// `AgentLoop::context_curator`. `None` (the default every existing
    /// construction site gets, unchanged) means the curator stage is a
    /// zero-cost pass-through -- `apply_curator` returns the original
    /// `ResolvedPath` without allocating a `CurateCtx` or even reading the
    /// lock's value's internals, so `run_inner`'s assembly stays
    /// byte-identical to behavior before this port existed (the
    /// `context_golden` 11/11 gate is the load-bearing proof).
    ///
    /// **`Arc<dyn Curator>`, no guard wrapper** -- unlike
    /// [`Self::context_hook`]'s `GuardedContextHook`, a curator needs no
    /// re-validation layer: `CurateOutcome::Derived` can only be built from
    /// a `Derivation`, which is already the validated, cost-estimated output
    /// of `ValidatedPath::derive` (§11.4). The "make it unrepresentable"
    /// move lives one layer up, in the type itself, so the seam does not
    /// need a second wrapper here.
    pub context_curator: RwLock<Option<Arc<dyn conway_core::ports::Curator>>>,
    /// Pluggable per-call context/tool curation. `RwLock` rather
    /// than a plain `Option` because `RuntimeDeps` (`runtime.rs`, out of
    /// this item's file scope) has no field to source one from at
    /// `LoopDeps` construction time -- `Runtime::set_context_hook` (a new,
    /// purely additive method) sets this post-construction, before any
    /// agent starts running, and every turn reads it fresh via
    /// `AgentLoop::context_hook`. `None` (the default every existing
    /// construction site gets, unchanged) means this loop never invokes
    /// anything named `ContextHook` at all -- not even a no-op call -- so
    /// `run_inner`'s assembly, routing, and overflow handling stay
    /// byte-identical to behavior before the hook existed. `Some` is invoked once per
    /// turn (`ContextHook::before_request`) and, only on a T-1
    /// `ContextTooLarge`, up to `MAX_OVERFLOW_ATTEMPTS` additional times
    /// (`ContextHook::on_overflow`) -- see `AgentLoop::route_and_attempt`.
    ///
    /// **`Arc<GuardedContextHook>`, never `Arc<dyn ContextHook>`**
    /// (board item `01M00RGARPESWXYAVY960KDE7S`, `INTENT.md` §8.6: "an
    /// invariant belongs to the seam, not to its call sites"). A bare hook
    /// is unrepresentable here by construction -- there is no way to store
    /// one that skipped `GuardedContextHook::new`'s tool-call/result
    /// coherence check, so a new call site (or a third `ContextHook`
    /// method, if one is ever added) inherits the guard automatically
    /// rather than having to remember to invoke it. The wrap happens once,
    /// in `Runtime::set_context_hook` -- the one place a hook enters this
    /// runtime -- not at either place `AgentLoop` uses the stored value.
    pub context_hook: RwLock<Option<Arc<GuardedContextHook>>>,
    /// Every `ToolObserver` the installed plugin set contributed, each paired
    /// with the plugin that supplied it so its fired events land in that
    /// plugin's namespace.
    ///
    /// Empty is the default and the overwhelming common case: with no
    /// observing plugin installed the loop's per-outcome observer pass does
    /// not execute at all, so behavior is byte-identical to a build without
    /// this port. That emptiness is the point rather than an optimization --
    /// `PHILOSOPHY.md` §6 requires that writing no loop-intervention policy
    /// be a real option, which it cannot be while the core ships one.
    pub observers: Vec<RegisteredObserver>,
    /// The emitter an observer's `ObserverCtx` fires through -- the SAME
    /// fan-out layer a plugin's own tools reach via `ToolCtx::plugin_events`,
    /// so there is one dispatch path for plugin-declared events rather than a
    /// second one for observers.
    pub plugin_events: Arc<dyn PluginEventEmitter>,
}

/// One agent's turn state machine (architecture §7). `run` drives turns
/// until a terminal `AgentResult` is produced; it never returns early with
/// an error — every failure path is folded into a non-`Completed`
/// `AgentResult`.
pub struct AgentLoop {
    pub agent_id: AgentId,
    pub session: SessionId,
    pub parent: Option<AgentId>,
    /// The root->this-agent chain, including this agent's own id. A root
    /// agent's path is `vec![agent_id]`.
    pub agent_path: Vec<AgentId>,
    pub cwd: PathBuf,
    /// S5: this agent's confinement root (S3's `SessionMeta.root`), already
    /// canonical when `Some` -- every construction site sources this
    /// straight from the same already-validated `PathBuf` persisted onto
    /// `SessionMeta.root` (`subagent.rs`'s `effective_root`, or `meta.root`
    /// on a resumed session; a freshly `start_root`ed agent always has
    /// `None`, since `RootSpec` has no root-setting field -- S3's own
    /// disclosed scope limit). `run_inner` reconstructs the real
    /// `conway_core::containment::CanonicalRoot` from this exactly once, at
    /// the top of the loop (mirroring the `CwdHandle` cell built alongside
    /// it) -- see `crate::permission::AgentRoot::reconstruct`.
    pub root: Option<PathBuf>,
    /// `[S1.5]` per-agent plugin configuration: this agent's own EFFECTIVE
    /// per-agent config -- the global `LoopDeps::plugin_config` (`deps`
    /// below), narrowed by every ancestor's own `SubagentSpec::
    /// plugin_config` override in turn, already merged (never a delta).
    /// `conway.fs`'s own root key is the proving consumer: `FsPlugin`'s
    /// tools read their confinement root from `ToolCtx::config`'s
    /// `"conway.fs.root"` key, which this field is what ultimately backs.
    ///
    /// A ROOT agent's own value is simply `deps.plugin_config.clone()`
    /// (today's pre-existing, always-global behavior, byte-identical). A
    /// fork/spawn child's value is computed exactly once, by `subagent.rs`'s
    /// `SubagentHost::start`, via `conway_core::ports::PluginConfig::
    /// narrow` against the PARENT's own live effective value (`Runtime::
    /// agent_plugin_config`) and the installed plugin set's declared
    /// narrowing rules (`PluginRegistry::narrowing_rules`) -- mirroring
    /// `Self::root`'s own "resolve once here, read fresh every batch"
    /// shape immediately above.
    ///
    /// **Persisted to `SessionMeta::plugin_config`, mirroring `Self::root`**
    /// (`01M0321414SVRD60HEP074AFHG`, closing the gap this field's doc used
    /// to disclose here: a resumed session's per-agent narrowing used to
    /// revert to whatever `deps.plugin_config` -- the global config --
    /// carried, silently, with no error and no warning). `subagent.rs`'s
    /// `SubagentHost::start` persists this same already-merged value onto
    /// `SessionMeta.plugin_config` when it constructs a fork/spawn child's
    /// header; `Runtime::resume_root` re-derives it on resume by
    /// re-applying `PluginConfig::narrow` against the CURRENT process-wide
    /// global config and the CURRENTLY installed plugin set's narrowing
    /// rules -- never by trusting the persisted record verbatim -- and
    /// refuses to resume outright (rather than silently dropping the
    /// narrowing or silently keeping a value nothing enforces) when a
    /// persisted key can no longer be validated. See `SessionMeta::
    /// plugin_config`'s own doc (`conway-core`) and `Runtime::resume_root`'s
    /// own doc comment at its plugin_config re-derivation for the full
    /// contract.
    pub plugin_config: Arc<PluginConfig>,
    pub deps: Arc<LoopDeps>,
    pub spec: AgentSpec,
    pub cancel: CancellationToken,
    /// `Some` for a fork child, resolved exactly once by `subagent.rs`'s
    /// `SubagentHost::start` at fork time via
    /// `conway_core::transcript::TranscriptResolver` and never recomputed afterward
    /// -- the parent's prefix at the fork point is immutable by
    /// construction (a later parent append only extends records the fork
    /// already excluded), so there is no turn-boundary event that could
    /// ever change this value. `None` for a root agent or a spawned child
    /// (spawn's context never inherits anything -- architecture §5.2).
    /// Cloned into every turn's `ContextInput::inherited` unchanged; see
    /// this crate's `context::InheritedPrefix` for why `records` stays a
    /// single shared `Arc` (sibling-fork memoization lives in
    /// `conway-session`, not here).
    pub inherited: Option<InheritedPrefix>,
    /// This agent's own inbox. Drained exactly once per turn
    /// boundary by `Self::drain_inbox` -- never read anywhere else, which
    /// is what makes the turn-boundary landing guarantee hold by
    /// construction.
    pub inbox: MailboxReceiver,
    /// The parent's mailbox sender, used to deliver this agent's terminal
    /// `AgentMessage::Result` upward on `finish` (architecture §3.2: "child
    /// terminates -> AgentResult -> parent mailbox"). `None` for a root
    /// agent (nothing to deliver to).
    pub parent_mailbox: Option<MailboxSender>,
    /// Set by a drained `AgentMessage::Cancel { hard: false, .. }`;
    /// consumed (and cleared) by the top-of-turn cancel check in
    /// `Self::run_inner`, which is what gives a soft cancel its
    /// turn-boundary semantics. A hard cancel never touches this field --
    /// it trips `cancel` directly at enqueue time instead (see
    /// `mailbox.rs`'s module doc). Every constructor should set this to
    /// `None`; `pub` only because this struct has no constructor function
    /// and is always built via a field literal (matching every other field
    /// here).
    pub pending_cancel: Option<String>,
    /// (generalized by the keep-alive item): gates this loop's next
    /// iteration on the caller's next prompt when `awaiting_prompt` is
    /// `true` (see [`ResumeGate`]'s own doc). `Default::default()` for every
    /// non-resumed, non-keep-alive agent -- `start_root` with `keep_alive:
    /// false` and every fork/spawn child -- which is inert and preserves
    /// this loop's earlier behavior exactly.
    pub resume_gate: ResumeGate,
}

/// Resume gate, generalized by the keep-alive item to also gate the
/// END of every turn for a `keep_alive` agent, not just a resumed root's
/// very first iteration -- both are exactly the same wait ("idle until the
/// caller's next prompt, unless cancelled/deadlined first"), so one
/// mechanism serves both instead of a second, parallel one.
///
/// **Its original purpose:** makes a resumed root agent's very first
/// loop iteration WAIT for the caller's next
/// [`crate::runtime::Runtime::prompt`] instead of reading the (stale,
/// already-completed) transcript it was persisted with and running a
/// spurious turn against it. Without this gate, `resume_root`'s spawned task
/// and the caller's subsequent `prompt` call race: if the loop's first
/// iteration reaches the backend before `prompt`'s `UserTurn` append lands,
/// that turn sees no new input, produces no tool calls, and
/// `finish(Completed)`s -- silently terminating the task before the real
/// prompt is ever read by anyone.
///
/// **Keep-alive's reuse:** the same race, one turn boundary later, is
/// exactly the bug a `keep_alive` session hits without this gate: its task
/// would `finish(Completed)` and end after the first turn, so a SECOND
/// `Runtime::prompt` on the same live session finds no task left to notify
/// (see `Runtime::prompt`'s doc). `AgentLoop::run_inner`'s
/// natural-completion branch sets `awaiting_prompt: true` and `continue`s
/// instead of returning when `AgentSpec::keep_alive` is `true`, landing in
/// the exact same top-of-loop wait `resume_root` already gates its first
/// iteration with.
///
/// `start_root` with `keep_alive: false` (and every fork/spawn child built
/// by `subagent.rs`) leaves this at its `Default` --
/// `awaiting_prompt: false` -- so `AgentLoop::run_inner`'s gate is skipped
/// entirely on the very first check and every turn behaves exactly as it did
/// beforehand. `Runtime::resume_root` sets `awaiting_prompt: true` up
/// front; a `keep_alive` agent starts with it `false` (its first turn runs
/// immediately, exactly like any other root) and the loop itself flips it
/// `true` at the end of each completed turn.
///
/// `notify` is a `tokio::sync::Notify`, chosen for its single-stored-permit
/// semantics: a `notify_one()` that lands before the gated `notified().await`
/// begins is not lost -- it is buffered, and the very next `.await` resolves
/// immediately. This is what makes `Runtime::prompt` safe to call without
/// coordinating with the resumed/idling task's own scheduling (it may not
/// have polled even once yet).
#[derive(Clone)]
pub struct ResumeGate {
    pub awaiting_prompt: bool,
    pub notify: Arc<tokio::sync::Notify>,
}

impl Default for ResumeGate {
    fn default() -> Self {
        Self {
            awaiting_prompt: false,
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
}

/// Per-turn accumulator: turns executed and usage accrued so far. `Clone`
/// so it can be captured into an error tuple without disturbing the loop's
/// own copy (see [`AgentLoop::run_inner`]'s early-return sites) --
/// `last_assistant_text` (below) is a `String`, so this is no longer `Copy`
/// as of this item; every `try_rt!` site only ever moves `$state` on the
/// `Err` arm's immediate `return`, so dropping `Copy` needed no call-site
/// changes there. `check_budget`/`finish_cancelled`/`finish_error` take
/// `&LoopState` rather than an owned one for the same reason: none of them
/// need ownership, and several `run_inner` call sites read `state` again
/// after calling one.
#[derive(Clone, Debug, Default)]
struct LoopState {
    turn: u32,
    usage: Usage,
    /// Steps taken since the last keep-alive user-turn boundary. Kept in
    /// lockstep with `turn` everywhere `turn` advances mid-turn (a
    /// result-contract retry, or a dispatched tool-call step), then reset to
    /// `0` -- not incremented -- at the keep-alive natural-completion branch
    /// in [`AgentLoop::run_inner`], since that reset marks the boundary
    /// itself: the turn that just ended needs no further budget check, and
    /// the next one hasn't taken a step yet. `check_budget` gates a
    /// `keep_alive` agent's `max_steps` dimension on THIS field instead of
    /// `turn`, so the budget bounds each user turn's tool-loop independently
    /// rather than the whole session's lifetime -- see that method's doc.
    /// Meaningless (never read) for a non-`keep_alive` agent: `check_budget`
    /// gates that path on `turn` exactly as before this field existed.
    turn_steps: u32,
    /// Tool calls DISPATCHED since the last keep-alive user-turn boundary,
    /// gating `Budget::max_tool_calls`. Counts the batch handed to
    /// `ToolRunner::run_batch`, not the outcomes it returns: a cancel
    /// arriving mid-batch discards every outcome, and calls that already ran
    /// real side effects must still count against the ceiling.
    ///
    /// Turn-scoped for the same reason `turn_steps` is, and reset at the same
    /// boundary: `max_tool_calls` is a runaway-tool-loop guard, so a
    /// session-lifetime reading would permanently end an interactive
    /// keep-alive session after N total calls rather than bounding each user
    /// turn.
    tool_calls: u32,
    /// The most recent backend response's own text (`agent_loop::full_text`
    /// of `outcome.response.content`), overwritten every turn a response
    /// lands in [`AgentLoop::run_inner`] -- BEFORE that same turn's own
    /// tool-dispatch/cancel/budget checks, so a termination mid-tool-batch
    /// still sees the text that accompanied those calls. Empty in
    /// [`LoopState::default`]: a termination before this agent's very first
    /// backend response genuinely has none, which
    /// [`AgentLoop::terminal_account`] (this field's one reader) takes as
    /// real signal, not an oversight -- see that method's own doc for the
    /// full precedence it resolves. Never reset at the keep-alive user-turn
    /// boundary the way `turn_steps`/`tool_calls`/`result_builder` are: a
    /// keep-alive agent idling between turns (`ResumeGate::awaiting_prompt`)
    /// that then hits a deadline should still report its last turn's own
    /// words, not an empty string just because a new user turn had not yet
    /// produced any of its own.
    last_assistant_text: String,
}

/// Bounds how many times [`AgentLoop::route_and_attempt`] will call
/// a registered `ContextHook::on_overflow` for a single turn before giving
/// up and surfacing the last `ContextTooLarge` regardless of what the hook
/// returns. This is a re-assembly-loop bound, not a policy choice a hook can
/// override -- a hook that keeps returning a still-too-large payload cannot
/// hang the turn. Picked small (a hook has two chances to shrink the
/// request enough to fit) since no criterion pins an exact value; `None`
/// registered, or a hook whose `on_overflow` returns `None` on its very
/// first call, both short-circuit long before this bound is ever reached.
const MAX_OVERFLOW_ATTEMPTS: u8 = 2;

/// Early-returns `Err((err.into(), $state))` from the enclosing
/// `Result<AgentResult, (RuntimeError, LoopState)>`-returning fn on a
/// fallible expression's `Err` arm, so every store/router/attempt failure
/// carries the turn state needed to construct a `Failed`/`Cancelled`
/// `AgentResult` without threading it through every call site by hand.
macro_rules! try_rt {
    ($state:expr, $result:expr) => {
        match $result {
            Ok(value) => value,
            Err(err) => return Err((err.into(), $state)),
        }
    };
}

impl AgentLoop {
    /// Drains every message queued on this agent's inbox and classifies
    /// each one (`mailbox::classify`, architecture §6.2). A `Steer` is
    /// persisted as `LogRecord::ParentSteer` *before* this call returns
    /// (persist-before-act) and before the next `SessionStore::read` this
    /// turn -- that ordering, plus this being the only site that ever
    /// calls `self.inbox.drain()`, is what makes "no code path injects into
    /// a context outside `drain_inbox`" hold structurally: a steer becomes
    /// visible by first becoming a stored record, read back exactly like
    /// any other own record (`resolve_default_path`'s own fresh store read,
    /// below), never by this function handing a segment to anyone directly.
    ///
    /// A soft cancel only sets `self.pending_cancel`, consumed by the
    /// caller immediately after this returns. A hard cancel was already
    /// handled at enqueue time (`MailboxSender::send`) and is a no-op here.
    /// `Result` is persisted as
    /// `LogRecord::ChildResultRecord`, the exact same `DrainEffect::Persist`
    /// arm as `Steer` -- this is a NON-blocking notification path, entirely
    /// separate from a `conway_fork`/`conway_spawn` waiter that blocked on
    /// this specific child by id, which still resolves exclusively through
    /// `AgentTree::await_result`; see `mailbox.rs`'s module doc for the
    /// full mechanism.
    ///
    /// ## A mid-batch persist failure does not lose the rest of the batch
    ///
    /// `self.inbox.drain()` atomically empties the queue into one `Vec`
    /// before this loop starts, so every message it processes has already
    /// left the mailbox and cannot be recovered from there. Before cycle-2
    /// review finding M2, a `SessionStore::append` failure on message *k*
    /// early-returned via `?`, silently dropping every already-dequeued
    /// message after it (soft cancels, everything) with no record and no
    /// signal. This function now keeps classifying and applying every
    /// remaining message's *non-persist* effect (a soft cancel still lands)
    /// even after a persist failure; it stops attempting further `append`
    /// calls against a store that has already failed once this drain (to
    /// avoid hammering it), and surfaces the first error at the end via a
    /// `tracing::error`
    /// naming exactly how many queued records could not be persisted,
    /// before returning it -- the agent is terminating either way (this
    /// error propagates through `run_inner`'s `try_rt!` into
    /// `finish_error`), so the caller's own error path is unaffected.
    async fn drain_inbox(&mut self) -> Result<(), RuntimeError> {
        let mut persist_err: Option<RuntimeError> = None;
        let mut lost_records = 0usize;

        for msg in self.inbox.drain() {
            match mailbox::classify(msg) {
                mailbox::DrainEffect::Persist(record) => {
                    if persist_err.is_some() {
                        lost_records += 1;
                        continue;
                    }
                    if let Err(err) = self.deps.store.append(&self.session, record).await {
                        persist_err = Some(err.into());
                        lost_records += 1;
                    }
                }
                mailbox::DrainEffect::SoftCancel { reason } => {
                    self.pending_cancel = Some(reason);
                }
                mailbox::DrainEffect::HardCancelAcknowledged => {}
                mailbox::DrainEffect::Unknown => {}
            }
        }

        if let Some(err) = persist_err {
            tracing::error!(
                agent = %self.agent_id,
                error = %err,
                lost_records,
                "drain_inbox: SessionStore::append failed; {lost_records} already-dequeued \
                 record(s) could not be persisted -- the agent is terminating"
            );
            return Err(err);
        }
        Ok(())
    }

    /// Routes and attempts the given (already `ContextHook::before_request`-
    /// hooked) request materials, retrying through
    /// `ContextHook::on_overflow` when the T-1 gate rejects the assembled
    /// request as too large for the routed model's window
    /// (`RoutingError::ContextTooLarge`) -- from either `Router::resolve`
    /// (the committed `conway_plugin_routing::DeclarativeRouter` -- an
    /// installable engine as of a later item, not a
    /// dependency of this crate -- now does construct
    /// this variant, exactly when every candidate's rejection is
    /// attributable solely to the headroom gate, closing an earlier gap
    /// -- see that crate's `router.rs` module
    /// doc) or `AttemptEngine::execute` (the T-1 backstop gate for the
    /// remaining case: a route the router admitted but whose real backend
    /// still rejects on context size, e.g. a stale/incorrect capability
    /// entry). Before that decision, `DeclarativeRouter` folded every
    /// all-rejected outcome into `RoutingError::NoCandidate`, which meant
    /// this destructure's `else` branch below always fired for a
    /// router-side rejection and `ContextHook::on_overflow` was reachable
    /// only via the `AttemptEngine` backstop path -- that gap is closed now
    /// that the router path can reach this method's `Ok` arm too.
    ///
    /// **No hook, or a hook whose `on_overflow` returns `None`, or
    /// [`MAX_OVERFLOW_ATTEMPTS`] exhausted:** the last `ContextTooLarge`
    /// propagates as `Err`, identical to what `run_inner` would have seen
    /// with no `ContextHook` machinery in this method at all -- this is what
    /// makes "no hook registered -> today's behavior exactly" hold for the
    /// overflow path specifically, not just for `before_request`.
    /// The currently-registered `ContextHook`, if any -- ALWAYS guarded
    /// (see `LoopDeps::context_hook`'s own doc). Reads `LoopDeps::
    /// context_hook` fresh on every call (see that field's own doc for why
    /// it is a `RwLock` rather than a plain `Option`).
    fn context_hook(&self) -> Option<Arc<GuardedContextHook>> {
        self.deps
            .context_hook
            .read()
            .expect("context_hook lock poisoned")
            .clone()
    }

    /// The currently-registered `Curator`, if any. Reads `LoopDeps::
    /// context_curator` fresh on every call (see that field's own doc for
    /// why it is a `RwLock` rather than a plain `Option`). `None` (the
    /// default) makes [`Self::apply_curator`] a zero-cost pass-through.
    fn context_curator(&self) -> Option<Arc<dyn conway_core::ports::Curator>> {
        self.deps
            .context_curator
            .read()
            .expect("context_curator lock poisoned")
            .clone()
    }

    /// The pre-assembly curator stage (DESIGN §11.4). Runs the registered
    /// [`Curator`](conway_core::ports::Curator) -- if any -- against the
    /// harness-resolved `path`, BEFORE `ContextInput` is assembled. This is
    /// the SEAM a cross-tree memory curator plugs into.
    ///
    /// **Zero-cost when no curator is installed** (the
    /// `context_golden` 11/11 gate's load-bearing guarantee): `None` returns
    /// the original `path` without allocating a `CurateCtx` or cloning the
    /// node list, so assembly is byte-identical to a build without this
    /// port. The clone of `path.nodes` into a `ValidatedPath` base happens
    /// ONLY after the `Some` branch is taken.
    ///
    /// **Failure is fail-open and recorded** (§11.6): a curator that returns
    /// `Failed` (or whose `derive` refused) is logged via `tracing::warn!`
    /// -- the SAME non-fatal recording posture a panicking `ToolObserver`
    /// uses (mirroring `ToolObserver`'s `catch_unwind` + `tracing::warn!`
    /// below) -- and the turn proceeds on the uncurated `path`. A curator is
    /// an optimization, not a correctness requirement; the consequence of
    /// not curating is caught downstream by admission (§2.7).
    ///
    /// `derive`-only construction is the guard (§11.4): a `Derived` outcome
    /// carries a `Derivation` whose `path` is already validated, so no
    /// separate `GuardedCurator` re-validation layer is needed -- the
    /// unrepresentability lives in `CurateOutcome`'s shape.
    ///
    /// Delegates to [`crate::context::curator_stage::apply_curator`] so the
    /// stage logic is unit-testable without constructing a full `AgentLoop`.
    async fn apply_curator(
        &self,
        path: ResolvedPath,
        turn: u32,
        model_hint: &ModelId,
    ) -> Result<(ResolvedPath, Option<String>), RuntimeError> {
        let curator = self.context_curator();
        // Capture owned values so the closure is self-contained; the
        // `build_ctx` closure is ONLY called on the `Some` branch -- the
        // zero-cost `None` pass-through never allocates a `CurateCtx`.
        let agent_id = self.agent_id;
        let session = self.session;
        let store = self.deps.store.clone();
        let resolver = self.deps.resolver.clone();
        // The curator stage runs BEFORE routing, so only the pinned
        // `ModelId` hint is available -- a routed `ModelRef` does not
        // exist yet (§11.5: a curator may READ model-dependent facts to
        // decide whether to act; what it PRODUCES stays model-free).
        let model = Some(model_hint.clone());
        crate::context::curator_stage::apply_curator(curator, path, move || CurateCtx {
            agent_id,
            session_id: session,
            turn,
            model,
            store,
            resolver,
        })
        .await
    }

    async fn route_and_attempt(
        &self,
        turn: u32,
        mut segments: Vec<PromptSegment>,
        mut tools: Vec<ToolSpec>,
        mut report: ContextReport,
        headroom: u32,
        artifacts: &ArtifactWriteHandle,
    ) -> Result<(AttemptOutcome, ContextReport), RuntimeError> {
        let mut overflow_attempts: u8 = 0;

        loop {
            let est_tokens = report.total_tokens_est;
            let has_tools = !tools.is_empty();
            let mut required = RequiredCaps {
                headroom_tokens: headroom,
                ..RequiredCaps::default()
            };
            if has_tools {
                required.tool_calling = Some(ToolCallSupport::NonStreamingOnly);
            }
            let route_req = RouteRequest {
                role: self.spec.role.clone(),
                pin: self.spec.pin.clone(),
                required,
                est_tokens,
                agent_id: self.agent_id,
            };

            let routing_err: RoutingError = match self.deps.router.resolve(&route_req) {
                Ok(routes) => {
                    let prefix_key = routes
                        .first()
                        .map(|route| crate::context::prefix_key(&route.model, &segments));
                    let attempt_req = AttemptRequest {
                        agent_id: self.agent_id,
                        session: self.session,
                        role: self.spec.role.clone(),
                        routes,
                        segments: &segments,
                        tools: &tools,
                        prefix_key,
                        est_tokens,
                        headroom,
                        max_tokens_override: None,
                        cache_ttl: self.spec.cache_ttl,
                        cancel: self.cancel.clone(),
                    };
                    match self.deps.attempt.execute(attempt_req).await {
                        Ok(outcome) => return Ok((outcome, report)),
                        Err(RuntimeError::Routing(e)) => e,
                        Err(other) => return Err(other),
                    }
                }
                Err(e) => e,
            };

            let RoutingError::ContextTooLarge {
                role,
                model,
                est_tokens: rejected_est_tokens,
                headroom_tokens,
                required_tokens,
                max_context_tokens,
                shortfall_tokens,
            } = routing_err
            else {
                return Err(routing_err.into());
            };

            // Reconstructs the SAME rejection the router/attempt engine
            // actually returned (`rejected_est_tokens`, not this loop's own
            // `est_tokens` local -- the two can differ, e.g. a fake/custom
            // `Router` in a test that returns a fixed `ContextTooLarge`
            // regardless of the request's real estimate) when no hook can
            // act on it.
            let too_large = |role: RoleAlias, model: ModelRef| RoutingError::ContextTooLarge {
                role,
                model,
                est_tokens: rejected_est_tokens,
                headroom_tokens,
                required_tokens,
                max_context_tokens,
                shortfall_tokens,
            };

            if overflow_attempts >= MAX_OVERFLOW_ATTEMPTS {
                return Err(too_large(role, model).into());
            }
            let rust_hook = self.context_hook();
            let hooks = self.deps.tool_runner.hooks();
            // `context_overflow`: the script-hook counterpart of
            // `ContextHook::on_overflow` (board item
            // `01KZRZZP6A4A27R3EN0HQAENBS`). Fires at the IDENTICAL trigger
            // boundary the Rust hook already observes -- this call site is
            // only reached once `routing_err` has already been destructured
            // as `RoutingError::ContextTooLarge` above (never `NoCandidate`,
            // never any mixed rejection); `hooks.will_dispatch` cannot widen
            // that, it only asks whether anything is subscribed.
            let scripts_subscribed = hooks.will_dispatch(crate::hook_dispatch::CONTEXT_OVERFLOW);
            if rust_hook.is_none() && !scripts_subscribed {
                return Err(too_large(role, model).into());
            }
            overflow_attempts += 1;

            let hook_ctx = ContextHookCtx {
                agent_id: self.agent_id,
                agent_path: self.agent_path.clone(),
                session_id: self.session,
                turn,
                model: Some(model.clone()),
                estimated_tokens: est_tokens,
                artifacts: artifacts.clone(),
                tag: self.spec.tag.clone(),
            };
            let overflow = OverflowInfo {
                max_context_tokens,
                headroom_tokens,
                required_tokens,
                shortfall_tokens,
            };
            let payload = ContextPayload {
                segments: segments.clone(),
                tools: tools.clone(),
            };

            // `rust_hook` (if any) and every subscribed script hook are
            // evaluated INDEPENDENTLY against the SAME pre-edit `payload`
            // above -- decision `01KYTQVYPJW0PAAXRBEMAKZY0V`'s no-chaining
            // rule, restated for this seam. `rust_hook` is a
            // `GuardedContextHook` (see `LoopDeps::context_hook`'s own doc):
            // its `on_overflow` is already the checked, `Result`-returning
            // inherent method, not the raw trait one -- a hook that shrinks
            // a payload by orphaning a tool call/result pair is refused
            // here, never repaired.
            let mut edited = false;
            let mut working = payload.clone();
            if let Some(hook) = &rust_hook {
                match hook.on_overflow(&hook_ctx, payload.clone(), overflow).await {
                    Ok(Some(transformed)) => {
                        working = transformed;
                        edited = true;
                    }
                    Ok(None) => {}
                    Err(err) => return Err(err.into_runtime_error()),
                }
            }
            if scripts_subscribed {
                let script_payload = serde_json::json!({
                    "agent_id": self.agent_id,
                    "agent_path": self.agent_path,
                    "session": self.session,
                    "turn": turn,
                    "model": model,
                    "max_context_tokens": max_context_tokens,
                    "headroom_tokens": headroom_tokens,
                    "required_tokens": required_tokens,
                    "shortfall_tokens": shortfall_tokens,
                    "segment_count": payload.segments.len(),
                    "estimated_tokens": est_tokens,
                    "segments": segment_metadata_json(&payload.segments),
                });
                let outcome = hooks
                    .dispatch_context(crate::hook_dispatch::CONTEXT_OVERFLOW, script_payload)
                    .await;
                if !outcome.answers.is_empty() {
                    let edit = crate::context::apply_script_deltas(working, &outcome.answers);
                    working = edit.payload;
                    edited = true;
                }
            }

            if !edited {
                return Err(too_large(role, model).into());
            }

            let checked = crate::context::hook_guard::ensure_hook_payload_coherent(
                crate::context::hook_guard::HookMethod::OnOverflow,
                &hook_ctx,
                working,
            )
            .map_err(|err| err.into_runtime_error())?;
            segments = checked.segments;
            tools = checked.tools;
            report = crate::context::builder::retotal(
                self.agent_id,
                turn,
                &mut segments,
                &tools,
                report.dropped,
                report.curator_failed,
                report.instruction_fragments,
            );
        }
    }

    /// Runs turns until a terminal result is produced. Infallible in return
    /// type: every internal failure (store I/O, routing, backend, budget,
    /// cancellation) is folded into a non-`Completed` [`AgentResult`] by
    /// `Self::finish`/`Self::finish_error` rather than propagated.
    pub async fn run(mut self) -> AgentResult {
        match self.run_inner().await {
            Ok(result) => result,
            Err((err, state)) => self.finish_error(&state, err).await,
        }
    }

    async fn run_inner(&mut self) -> Result<AgentResult, (RuntimeError, LoopState)> {
        let mut state = LoopState::default();
        let mut seen_segments = HashSet::new();
        // both are turn-loop-local, not `AgentLoop` fields -- see
        // `result.rs`'s module doc for why (both structs are constructed
        // via field literals in files outside this item's scope).
        let mut result_builder = ResultBuilder::new();
        // result-contract retry: `true` once this run has already
        // spent its one corrective turn (`self.spec.result_contract`'s
        // "retried exactly once" rule) -- a second failure after this is
        // `true` is terminal (`Rejected`), never another retry.
        let mut contract_retried = false;
        // S1: the "cd" cell for this agent, constructed exactly once here
        // (not a struct field -- `run_inner` already runs exactly once per
        // agent, so a loop-local is the cell's whole lifetime) and cloned
        // into every turn's `ToolBatchCtx`/`ToolCtx` below. Seeded from
        // `self.cwd`, the agent's spawn-time cwd (unchanged in meaning: see
        // `SessionMeta.cwd`'s own doc for why a `cd` does not rewrite that
        // header -- this cell is the ephemeral, in-memory analogue, not a
        // second persisted copy of it). Cloning `CwdHandle` is an `Arc`
        // refcount bump: every clone shares the same cell, so a tool's
        // `ctx.chdir.set(..)` (a future slice) is visible to this loop, and
        // to every subsequent turn's snapshot, without any write-back step.
        let chdir = CwdHandle::new(self.cwd.clone());
        // S5: this agent's confinement root, reconstructed exactly ONCE
        // here (not a struct field -- same reasoning as `chdir` immediately
        // above) from the persisted, already-canonical `PathBuf` this loop
        // was constructed with. Cloned into every turn's `ToolBatchCtx`
        // below; cloning is cheap (see `AgentRoot`'s own doc) so the one
        // filesystem `canonicalize` call this performs happens once per
        // agent's whole run, never once per batch or per tool call.
        let root = crate::permission::AgentRoot::reconstruct(&self.root);
        // the write-location
        // capability a registered `ContextHook` sees on `ContextHookCtx::
        // artifacts`, built from the SAME `chdir`/`root` pair immediately
        // above -- never a second, independent reconstruction. See
        // `crate::artifact_store`'s own doc.
        let artifacts = ArtifactWriteHandle::new(
            Arc::new(crate::artifact_store::AgentArtifactWriter::new(
                chdir.clone(),
                root.clone(),
            )),
            self.agent_id,
        );

        loop {
            try_rt!(state, self.drain_inbox().await);

            if let Some(reason) = self.pending_cancel.take() {
                return Ok(self
                    .finish(
                        ResultStatus::Cancelled { reason },
                        self.terminal_account(&state, &result_builder),
                        state.usage,
                        state.turn,
                        &result_builder,
                    )
                    .await);
            }
            if let Some(result) = self.check_budget(&state, &result_builder).await {
                return Ok(result);
            }
            if self.cancel.is_cancelled() {
                return Ok(self.finish_cancelled(&state, &result_builder).await);
            }

            // Generalized for keep-alive: a resumed root's very
            // first iteration, or a `keep_alive` agent's idle wait between
            // turns, waits here for the caller's next prompt instead of
            // proceeding into a spurious turn -- see `ResumeGate`'s own doc
            // for why. Once cleared, `continue` re-runs the loop from the
            // top (fresh `drain_inbox`/budget/cancel checks) with the gate
            // now open, so this branch is not re-entered until (for
            // `keep_alive`) the NEXT turn also completes with no pending
            // work.
            if self.resume_gate.awaiting_prompt {
                match self.spec.budget.deadline {
                    Some(deadline) => {
                        let remaining = (deadline - Utc::now()).to_std().unwrap_or(Duration::ZERO);
                        tokio::select! {
                            biased;
                            () = self.cancel.cancelled() => {
                                return Ok(self.finish_cancelled(&state, &result_builder).await);
                            }
                            () = tokio::time::sleep(remaining) => {
                                return Ok(self.finish(
                                    ResultStatus::BudgetExceeded { limit: format!("deadline={deadline}") },
                                    self.terminal_account(&state, &result_builder),
                                    state.usage,
                                    state.turn,
                                    &result_builder,
                                ).await);
                            }
                            () = self.resume_gate.notify.notified() => {
                                self.resume_gate.awaiting_prompt = false;
                            }
                        }
                    }
                    None => {
                        tokio::select! {
                            biased;
                            () = self.cancel.cancelled() => {
                                return Ok(self.finish_cancelled(&state, &result_builder).await);
                            }
                            () = self.resume_gate.notify.notified() => {
                                self.resume_gate.awaiting_prompt = false;
                            }
                        }
                    }
                }
                continue;
            }

            // Board `01M0VWMMEG4CER8Y8VH77KZ0CV`: marked BEFORE the bus
            // emit, mirroring `Event::TurnStarted`'s own ordering guarantee
            // (this module's doc: no later `seq` observed before an earlier
            // one) -- so any subscriber that ever observes the live event
            // is guaranteed to find `AgentTree::turn_in_flight` already
            // `true`, never a stale `false`.
            self.deps.tree.mark_turn_started(self.agent_id);
            self.deps.bus.emit(
                self.session,
                self.agent_id,
                Event::TurnStarted { turn: state.turn },
            );

            let tool_specs = self.deps.registry.specs(self.spec.tools.as_ref());
            let model_hint = self
                .spec
                .pin
                .as_ref()
                .map(|pin| pin.model.clone())
                .unwrap_or_else(|| ModelId::new("unrouted"));

            // `resolve_default_path` runs its own fresh `SessionStore::read`
            // internally (`context/path.rs`'s step 1) -- the same "no
            // injection outside `drain_inbox`" guarantee `path_from_legacy`
            // (the constructor this replaces) relied on `all_records` for,
            // preserved here without a second, now-redundant read of the
            // same range. `self.deps.resolver`/`self.deps.path_store` are
            // the SAME `Arc`s the curator stage's `CurateCtx` reads below --
            // one resolver, one path store, both shared across every turn.
            let validated_path = try_rt!(
                state,
                resolve_default_path(
                    &self.deps.resolver,
                    self.deps.store.as_ref(),
                    self.deps.path_store.as_ref(),
                    &self.session,
                )
                .await
            );
            let path = ResolvedPath {
                nodes: validated_path.into_nodes(),
            };
            // Pre-assembly curator stage (DESIGN §11.4): a registered
            // `Curator` may derive a new path from `path` before assembly
            // renders it. `None` (the default) is a zero-cost pass-through --
            // `apply_curator` returns the original `path` unchanged, never
            // allocating a `CurateCtx`, so `context_golden` stays 11/11
            // unregenerated. This is the SEAM a cross-tree memory curator
            // (Unit 3) plugs into; Unit 2 proves it with test curators.
            let (path, curator_failed) = try_rt!(
                state,
                self.apply_curator(path, state.turn, &model_hint).await
            );
            let input = ContextInput {
                agent_id: self.agent_id,
                turn: state.turn,
                model: model_hint,
                cache_mode: self.spec.cache_mode.clone(),
                system_prompt: self.spec.system_prompt.clone(),
                instructions: self.spec.instructions.clone(),
                skills: self.spec.skills.clone(),
                tools: tool_specs.clone(),
                path,
                cache_ttl: self.spec.cache_ttl,
                curator_failed,
            };
            let (mut segments, mut report) = try_rt!(state, self.deps.builder.build(&input));

            // give a registered `ContextHook` first look at the
            // assembled request -- segment edits/drops (mask-like
            // exclusion, system-prompt augmentation via the
            // `AgentDef`-provenance segment) and tool-announcement
            // narrowing (`announced_tools`, distinct from `PermissionGate`
            // -- see `ContextPayload`'s own doc) all go through this one
            // call. `segments`/`report`/`announced_tools` are re-derived
            // from whatever the hook returns so every downstream consumer
            // below (the live report slot, `Event::ContextSegmentAdded`,
            // routing, the attempt request, the persisted
            // `ContextReportRecord`) sees the SAME payload that is actually
            // sent -- never the pre-hook one. No hook registered -> this
            // block never runs -> the rest of this turn is byte-identical to
            // the behavior before the hook existed.
            let mut announced_tools = tool_specs.clone();
            if let Some(hook) = self.context_hook() {
                let hook_ctx = ContextHookCtx {
                    agent_id: self.agent_id,
                    agent_path: self.agent_path.clone(),
                    session_id: self.session,
                    turn: state.turn,
                    model: self.spec.pin.clone(),
                    estimated_tokens: report.total_tokens_est,
                    artifacts: artifacts.clone(),
                    tag: self.spec.tag.clone(),
                };
                let payload = ContextPayload {
                    segments,
                    tools: announced_tools,
                };
                // `hook` is a `GuardedContextHook` (see `LoopDeps::
                // context_hook`'s own doc): its `before_request` is already
                // the checked, `Result`-returning inherent method, not the
                // raw trait one -- `ContextBuilder::build` guaranteed no
                // rendered context carries a tool call without its result,
                // but only about ITS OWN output, and a hook edits an
                // already-coherent list freely. There is nothing left for
                // this call site to remember; the SAME guard covers
                // `route_and_attempt`'s `on_overflow` call below.
                let transformed = try_rt!(
                    state,
                    hook.before_request(&hook_ctx, payload)
                        .await
                        .map_err(|err| err.into_runtime_error())
                );
                segments = transformed.segments;
                announced_tools = transformed.tools;
                report = crate::context::builder::retotal(
                    self.agent_id,
                    state.turn,
                    &mut segments,
                    &announced_tools,
                    report.dropped,
                    report.curator_failed,
                    report.instruction_fragments,
                );
            }

            // `request_assembled`: fires once per turn, here -- after
            // `ContextBuilder::build` AND (if one is registered)
            // `ContextHook::before_request`'s own edit immediately above,
            // so a subscriber sees the FINAL pre-script assembled request,
            // never the pre-Rust-hook one -- and before `route_and_attempt`
            // below, exactly as `schema::HooksConfig`'s own doc states.
            //
            // CONTEXT-EDITING as of board item
            // `01KZRZZP6A4A27R3EN0HQAENBS`, not observation-only: a
            // configured script hook's `HookAnswer.context`
            // (`conway_core::hook::ContextDelta`) is read via
            // `HookDispatcher::dispatch_context` and applied append-only
            // (`crate::context::apply_script_deltas`), then run through the
            // SAME `crate::context::hook_guard::ensure_hook_payload_coherent`
            // guard the Rust `ContextHook` path above already goes through
            // -- there is no second, unguarded way for a script's edit to
            // reach a request. Still fails OPEN, per hook: a failing/timing-
            // out/malformed script contributes nothing (visibly --
            // `HookDispatcher::dispatch_context`'s own `tracing::warn!` plus
            // its returned `ContextHookFailure`), never fails the turn. An
            // existing rule written purely for observation (its answer never
            // sets `context`) is unaffected: applying an empty `ContextDelta`
            // is a no-op.
            //
            // A SUMMARY payload plus per-segment METADATA, never segment
            // CONTENT: `report.segments` already carries the ordered
            // content this turn is made of, but shipping the full assembled
            // transcript to a subprocess every turn is a real, unbounded
            // cost this item's own design question flags -- `segment_metadata_json`
            // ships each segment's id (needed to EXCLUDE it), role, and
            // provenance (enough for a role/provenance-driven policy, e.g.
            // "exclude every `ToolResult` older than N calls"), never
            // `content`.
            //
            // Reached through `LoopDeps::tool_runner` rather than a new
            // `LoopDeps` field: `ToolRunner::hooks()` is the SAME shared
            // `HookDispatcher` `Runtime::new` already wires `post_tool_use`/
            // `session_starting`/`child_spawned`/`prompt_submitted` through
            // (`tools/runner.rs`'s own doc on `ToolRunner::hooks`), so
            // wiring one runner reaches every dispatched event -- no new
            // machinery, only this call site.
            let hooks = self.deps.tool_runner.hooks();
            if hooks.will_dispatch(crate::hook_dispatch::REQUEST_ASSEMBLED) {
                let script_payload = serde_json::json!({
                    "agent_id": self.agent_id,
                    "agent_path": self.agent_path,
                    "session": self.session,
                    "turn": state.turn,
                    "model_pin": self.spec.pin.as_ref().map(|pin| pin.model.clone()),
                    "segment_count": report.segments.len(),
                    "total_tokens_est": report.total_tokens_est,
                    "tokenizer": report.tokenizer.clone(),
                    "segments": segment_metadata_json(&segments),
                });
                let outcome = hooks
                    .dispatch_context(crate::hook_dispatch::REQUEST_ASSEMBLED, script_payload)
                    .await;
                if !outcome.answers.is_empty() {
                    let hook_ctx = ContextHookCtx {
                        agent_id: self.agent_id,
                        agent_path: self.agent_path.clone(),
                        session_id: self.session,
                        turn: state.turn,
                        model: self.spec.pin.clone(),
                        estimated_tokens: report.total_tokens_est,
                        artifacts: artifacts.clone(),
                        tag: self.spec.tag.clone(),
                    };
                    let edit = crate::context::apply_script_deltas(
                        ContextPayload {
                            segments,
                            tools: announced_tools,
                        },
                        &outcome.answers,
                    );
                    let checked = try_rt!(
                        state,
                        crate::context::hook_guard::ensure_hook_payload_coherent(
                            crate::context::hook_guard::HookMethod::BeforeRequest,
                            &hook_ctx,
                            edit.payload,
                        )
                        .map_err(|err| err.into_runtime_error())
                    );
                    segments = checked.segments;
                    announced_tools = checked.tools;
                    report = crate::context::builder::retotal(
                        self.agent_id,
                        state.turn,
                        &mut segments,
                        &announced_tools,
                        report.dropped,
                        report.curator_failed,
                        report.instruction_fragments,
                    );
                }
            }

            if let Some(slot) = &self.spec.report_slot {
                *slot.lock().expect("report slot poisoned") = Some(report.clone());
            }

            for entry in &report.segments {
                if seen_segments.insert(entry.segment) {
                    self.deps.bus.emit(
                        self.session,
                        self.agent_id,
                        Event::ContextSegmentAdded {
                            segment: entry.segment,
                            provenance: entry.provenance.clone(),
                            tokens_est: entry.tokens_est,
                        },
                    );
                }
            }

            let headroom = resolve_headroom(&self.spec, &self.deps.headroom);

            // `route_and_attempt` owns routing, the attempt call,
            // AND the bounded `ContextHook::on_overflow` re-assembly retry
            // (see that method's own doc) -- the only thing this call site
            // still owns is racing it against the turn's deadline, exactly
            // as the earlier `attempt_fut` race did.
            let route_attempt_fut = self.route_and_attempt(
                state.turn,
                segments,
                announced_tools,
                report,
                headroom,
                &artifacts,
            );
            let route_attempt_result = match self.spec.budget.deadline {
                Some(deadline) => {
                    let remaining = (deadline - Utc::now()).to_std().unwrap_or(Duration::ZERO);
                    tokio::select! {
                        biased;
                        () = tokio::time::sleep(remaining) => {
                            return Ok(self.finish(
                                ResultStatus::BudgetExceeded { limit: format!("deadline={deadline}") },
                                self.terminal_account(&state, &result_builder),
                                state.usage,
                                state.turn,
                                &result_builder,
                            ).await);
                        }
                        res = route_attempt_fut => res,
                    }
                }
                None => route_attempt_fut.await,
            };
            let (outcome, report) = try_rt!(state, route_attempt_result);
            // Captured BEFORE any of this turn's own tool-dispatch/cancel/
            // budget checks below, so a termination mid-tool-batch still
            // sees the text that accompanied those calls, not stale text
            // from an earlier turn -- see `LoopState::last_assistant_text`'s
            // own doc, and `AgentLoop::terminal_account` (this field's one
            // reader).
            state.last_assistant_text = full_text(&outcome.response.content);
            // The report_slot/persisted report must reflect the FINAL
            // assembly actually sent -- overflow retries (if any) rebuilt
            // `report` after the initial slot update above.
            if let Some(slot) = &self.spec.report_slot {
                *slot.lock().expect("report slot poisoned") = Some(report.clone());
            }

            let usage = outcome.response.usage;
            let seq = try_rt!(state, self.deps.store.head(&self.session).await);
            // the assistant record must carry the whole turn -- its
            // text AND the tool calls it made. `GenerateResponse` keeps
            // `content` (text/thinking) and `tool_calls` in separate fields;
            // fold the calls in as trailing `ToolUse` blocks so the persisted
            // record is the complete assistant message. Without this the next
            // turn's context rebuilds an assistant message with no
            // `tool_calls`/`tool_use`: the model never sees that it called a
            // tool, re-calls indefinitely, and the following `ToolResult` is
            // an orphan. Both wire adapters already read `ToolUse` blocks out
            // of the stored content (`assistant_message` / `assistant_content_blocks`).
            let mut assistant_content = outcome.response.content.clone();
            assistant_content.extend(outcome.response.tool_calls.iter().map(|call| {
                ContentBlock::ToolUse {
                    call_id: call.call_id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                }
            }));
            let assistant_record = LogRecord::Assistant {
                seq,
                ts: Utc::now(),
                content: assistant_content,
                model: ModelRef {
                    backend: outcome.route.backend.clone(),
                    model: outcome.route.model.clone(),
                },
                route_reason: serde_json::to_value(&outcome.route.reason)
                    .expect("RoutingReason always serializes"),
                usage,
                stop: outcome.response.stop,
            };
            try_rt!(
                state,
                self.deps
                    .store
                    .append(&self.session, assistant_record)
                    .await
            );
            // persist the SAME report already pushed to
            // `report_slot` above -- one build, two surfaces (live slot,
            // durable store) -- and only after the assistant record it
            // describes is itself durable, so a report is never persisted
            // for a turn that did not happen.
            try_rt!(
                state,
                crate::context::report::persist(self.deps.store.as_ref(), &self.session, &report)
                    .await
            );
            state.usage += usage;

            self.deps.bus.emit(
                self.session,
                self.agent_id,
                Event::TurnFinished {
                    usage,
                    stop: outcome.response.stop,
                },
            );
            // Board `01M0VWMMEG4CER8Y8VH77KZ0CV`: the success-path twin of
            // `mark_turn_started` above. Every OTHER exit from this loop
            // (error/cancelled/budget-exceeded) never reaches this line, but
            // all of those are terminal for the whole agent and so always
            // call `AgentTree::publish_result`, which clears this
            // defensively too -- see `turn_in_flight`'s own doc.
            self.deps.tree.mark_turn_finished(self.agent_id);

            if outcome.response.tool_calls.is_empty() {
                let summary = full_text(&outcome.response.content);

                if let Some(contract) = &self.spec.result_contract {
                    let parts = result_builder.resolve(&summary, &ResultStatus::Completed);
                    match validate_result_contract(
                        parts.structured.as_ref(),
                        contract,
                        contract_retried,
                    ) {
                        ContractOutcome::Ok => {}
                        ContractOutcome::Retry { errors } => {
                            let note_seq =
                                try_rt!(state, self.deps.store.head(&self.session).await);
                            let note_text = format!(
                                "the structured result failed its result_contract: {}",
                                errors.join("; ")
                            );
                            try_rt!(
                                state,
                                self.deps
                                    .store
                                    .append(
                                        &self.session,
                                        LogRecord::SystemNote {
                                            seq: note_seq,
                                            ts: Utc::now(),
                                            text: note_text,
                                            reason: "result_contract_violation".to_string(),
                                            prov: Provenance::SystemNote {
                                                reason: "result_contract_violation".to_string(),
                                            },
                                        },
                                    )
                                    .await
                            );
                            contract_retried = true;
                            state.turn += 1;
                            state.turn_steps += 1;
                            continue;
                        }
                        ContractOutcome::Rejected { missing } => {
                            return Ok(self
                                .finish(
                                    ResultStatus::Rejected { missing },
                                    summary,
                                    state.usage,
                                    state.turn + 1,
                                    &result_builder,
                                )
                                .await);
                        }
                    }
                }

                // Keep-alive (opt-in, `AgentSpec::keep_alive`): a turn that
                // completes with no pending work does NOT end this agent's
                // task -- it idle-awaits the caller's next prompt instead,
                // via the very same `ResumeGate` `resume_root` gates its
                // first iteration with (see that type's own doc). No
                // `finish` call here and no `Event::AgentFinished` -- a
                // keep-alive session is consumed turn-by-turn over the
                // event stream (`TurnStarted`/`TurnFinished`, already
                // emitted above, unconditionally), not via one terminal
                // `AgentResult`; `state.usage`/`state.turn` (bumped here,
                // exactly like the non-keep-alive path's `state.turn + 1`)
                // keep accruing across turns so budgets still span the
                // whole session (`check_budget`, next loop iteration, at
                // the top). The task ends ONLY via the top-of-loop
                // cancel/budget checks or the gate's own
                // cancel/deadline arms below -- never by falling out of
                // this branch on its own after a normal turn.
                if self.spec.keep_alive {
                    state.turn += 1;
                    // Keep-alive user-turn boundary: reset the per-turn step
                    // budget counter and this turn's result accumulators.
                    // `state.turn_steps = 0` (not `+= 1`) because the turn
                    // that just naturally completed needs no further budget
                    // check and the next one hasn't taken a step yet (see
                    // that field's own doc). `result_builder`/
                    // `contract_retried` are similarly turn-scoped for a
                    // keep-alive agent: without this reset, a `report` call
                    // (or a spent contract retry) from THIS turn would keep
                    // shadowing every later turn's own outcome all the way
                    // to whatever turn the session eventually really ends
                    // on, producing a terminal `AgentResult` built from
                    // stale, unrelated history. `seen_segments` and
                    // `state.usage` are deliberately NOT reset here -- they
                    // must persist across the whole keep-alive session (see
                    // their own declarations above this loop).
                    state.turn_steps = 0;
                    state.tool_calls = 0;
                    result_builder = ResultBuilder::new();
                    contract_retried = false;
                    self.resume_gate.awaiting_prompt = true;
                    continue;
                }

                return Ok(self
                    .finish(
                        ResultStatus::Completed,
                        summary,
                        state.usage,
                        state.turn + 1,
                        &result_builder,
                    )
                    .await);
            }

            let batch_ctx = ToolBatchCtx {
                agent_id: self.agent_id,
                agent_path: self.agent_path.clone(),
                session_id: self.session,
                chdir: chdir.clone(),
                cancel: self.cancel.clone(),
                subagents: self.deps.subagents.clone(),
                context_path_host: self.deps.context_path_host.clone(),
                session_discovery_host: self.deps.session_discovery_host.clone(),
                capability_host: self.deps.capabilities.clone(),
                // [S1.5]: this agent's own EFFECTIVE per-agent config
                // (`self.plugin_config`, resolved once at construction --
                // see that field's own doc), not the shared, process-wide
                // `self.deps.plugin_config` every agent used to read
                // identically.
                plugin_config: self.plugin_config.clone(),
                max_parallel_tools: self.spec.max_parallel_tools.max(1),
                root: root.clone(),
            };
            // Counted from the DISPATCHED batch rather than `outcomes` below:
            // the cancel check immediately after discards every outcome,
            // including calls that already completed real side effects, so
            // counting outcomes would let a cancelled batch's work escape the
            // ceiling entirely.
            state.tool_calls = state
                .tool_calls
                .saturating_add(outcome.response.tool_calls.len() as u32);

            let outcomes = self
                .deps
                .tool_runner
                .run_batch(&batch_ctx, outcome.response.tool_calls.clone())
                .await;

            if self.cancel.is_cancelled() {
                // The batch's outcomes are dropped here, including any calls
                // that completed real side effects before the cancel fired —
                // their results never reach the session log.
                return Ok(self.finish_cancelled(&state, &result_builder).await);
            }

            let calls = outcome.response.tool_calls.clone();
            for (index, tool_outcome) in outcomes.into_iter().enumerate() {
                result_builder.observe_tool_outcome(&tool_outcome.tool, &tool_outcome);

                let seq = try_rt!(state, self.deps.store.head(&self.session).await);
                let result = ToolResult {
                    call_id: tool_outcome.call_id,
                    tool: tool_outcome.tool.clone(),
                    blocks: tool_outcome.blocks,
                    is_error: tool_outcome.is_error,
                    truncated: tool_outcome.truncation,
                };
                try_rt!(
                    state,
                    self.deps
                        .store
                        .append(
                            &self.session,
                            LogRecord::ToolResultRecord {
                                seq,
                                ts: Utc::now(),
                                result,
                            },
                        )
                        .await
                );

                // Hand the finished call to every registered `ToolObserver`,
                // and append whatever notes they ask for. The harness holds
                // no policy of its own here -- with no observing plugin
                // installed this loop body does not execute at all, which is
                // what `PHILOSOPHY.md` §6 ("the policy is yours to write,
                // including writing none") requires.
                //
                // `calls` preserves input order (`ToolRunner::run_batch`'s own
                // contract), so `calls[index]` is this outcome's original call
                // and carries the arguments a policy keys on.
                for registered in &self.deps.observers {
                    let observed = ObservedCall {
                        agent_id: self.agent_id,
                        session: self.session,
                        call_id: calls[index].call_id.clone(),
                        tool: tool_outcome.tool.clone(),
                        arguments: calls[index].arguments.clone(),
                        is_error: tool_outcome.is_error,
                        result_seq: seq,
                    };
                    let ctx = ObserverCtx {
                        events: PluginEventHandle::new(
                            self.deps.plugin_events.clone(),
                            registered.plugin_id.clone(),
                        ),
                    };
                    // Observation must never fail the call it observed: the
                    // side effects already happened, so a panicking observer
                    // is contained and the batch proceeds. Same fail-open
                    // posture `post_tool_use` already takes, for the same
                    // reason.
                    let answer =
                        match futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                            registered.observer.after_tool_call(&ctx, &observed),
                        ))
                        .await
                        {
                            Ok(answer) => answer,
                            Err(_) => {
                                tracing::warn!(
                                    plugin = %registered.plugin_id,
                                    tool = %tool_outcome.tool,
                                    "tool observer panicked; ignoring its answer"
                                );
                                continue;
                            }
                        };
                    for note in answer.notes {
                        let note_seq = try_rt!(state, self.deps.store.head(&self.session).await);
                        try_rt!(
                            state,
                            self.deps
                                .store
                                .append(
                                    &self.session,
                                    LogRecord::SystemNote {
                                        seq: note_seq,
                                        ts: Utc::now(),
                                        text: note.text,
                                        reason: note.reason.clone(),
                                        prov: Provenance::SystemNote {
                                            reason: note.reason,
                                        },
                                    },
                                )
                                .await
                        );
                    }
                }
            }

            state.turn += 1;
            state.turn_steps += 1;
        }
    }

    /// Synthesizes the `trailing_text` argument every non-natural terminal
    /// path (budget, cancellation, deadline, backend failure) passes to
    /// [`Self::finish`] -- every one of those sites used to pass a literal
    /// `""`, which `ResultBuilder::resolve`'s status-naming fallback then
    /// turned into `"(no output; terminal status: <name>)"` regardless of
    /// how much real work the run had already done. After this item, no
    /// call to `finish(` in this file passes a bare `""` (`grep -n
    /// 'finish(' agent_loop.rs` finds none) -- this method is what every
    /// one of those ten sites calls instead.
    ///
    /// Precedence:
    /// 1. `state.last_assistant_text` -- the most recent backend response's
    ///    own text, captured every turn in [`Self::run_inner`] BEFORE that
    ///    same turn's own tool-dispatch/cancel/budget checks (see that
    ///    field's own doc). This is the concrete fix for the incident this
    ///    item exists to close: a turn that dispatched tool calls and was
    ///    then cut off -- by a budget check, a cancel, or a deadline --
    ///    before reaching a LATER turn's natural completion still reports
    ///    whatever the model said alongside those calls, not `""`.
    /// 2. An explicit "stopped mid-run" marker, used only when there is no
    ///    captured text but other evidence shows real work happened this
    ///    run anyway (`state.turn > 0`, `state.tool_calls > 0`, or
    ///    `builder.has_activity()` -- a tool artifact or a `report` call
    ///    already observed). This is acceptance criterion 2, "distinguishes
    ///    'no work' from 'stopped before reporting'": a run that dispatched
    ///    tool calls with no accompanying assistant text (a model that only
    ///    ever emits tool calls, never prose) is not a run that did
    ///    nothing, and must not read like one.
    /// 3. `""`, unchanged, when NEITHER of the above holds -- a termination
    ///    before this agent's very first turn ever produced anything at
    ///    all. That really is "no work", and `ResultBuilder::resolve`'s
    ///    status-naming fallback remains the correct, honest summary for
    ///    it -- this method does not touch that fallback, only makes sure
    ///    it is reached by a run that actually earns it.
    fn terminal_account(&self, state: &LoopState, builder: &ResultBuilder) -> String {
        if !state.last_assistant_text.trim().is_empty() {
            return state.last_assistant_text.clone();
        }
        if state.turn > 0 || state.tool_calls > 0 || builder.has_activity() {
            return format!(
                "(stopped after {} turn(s), {} tool call(s) dispatched this run, \
                 before producing a final report -- no assistant text was \
                 captured for the in-progress turn)",
                state.turn, state.tool_calls
            );
        }
        String::new()
    }

    /// Checks every configured budget dimension at the top of a turn,
    /// returning `Some(result)` the first exceeded dimension produces. All
    /// four of `Budget`'s dimensions are enforced here; a dimension a caller
    /// sets must bind, since the whole reason to set one is to bound cost or
    /// blast radius, and a ceiling that silently does nothing is worse than
    /// no field at all.
    ///
    /// **`max_steps` and `max_tool_calls` for a `keep_alive` agent** gate on
    /// turn-scoped counters (`state.turn_steps`, `state.tool_calls`)
    /// (steps since the last user-turn boundary) instead of `state.turn`
    /// (steps since the agent's whole life began): a keep-alive session must
    /// survive an unbounded number of user turns, each independently bounded
    /// by `max_steps` as a runaway-tool-loop guard, not have the WHOLE
    /// session's lifetime capped at `max_steps` total steps -- see
    /// `LoopState::turn_steps`'s own doc. Every other budget dimension
    /// (`deadline`, `max_tokens`) is intentionally left session-lifetime for
    /// both keep-alive and non-keep-alive agents: `state.usage` already
    /// accrues across every turn of a keep-alive session (never reset), and
    /// `deadline` is a wall-clock cutoff independent of turn boundaries by
    /// nature. A session-lifetime `max_tokens`/`deadline` can still end an
    /// interactive keep-alive session outright (the TUI's default `Budget`
    /// has no `max_tokens`/`deadline` set, so this does not fire in
    /// practice) -- the companion TUI-notice fix
    /// (`conway_cli::tui::state::AppState::apply_agent_finished`) is what
    /// makes any such termination visible rather than silent; making those
    /// two dimensions turn-scoped too is out of this item's scope.
    ///
    /// Non-`keep_alive` behavior is byte-for-byte unchanged: `state.turn`
    /// gates `max_steps` exactly as before this field existed.
    async fn check_budget(&self, state: &LoopState, builder: &ResultBuilder) -> Option<AgentResult> {
        let budget = &self.spec.budget;
        let steps_this_turn = if self.spec.keep_alive {
            state.turn_steps
        } else {
            state.turn
        };
        // `0` means NO CEILING, matching every other dimension in
        // `[limits]` (`max_tokens`, `deadline_secs`, `max_tool_calls` all
        // document "0 = unlimited"). `max_steps` was the one limit an
        // operator could not turn off: `Conway::default_budget` maps the
        // other three through `if x == 0 { None }` and passes this one
        // straight through as a `u32`, so any value at all was a hard
        // ceiling. An interactive coding session routinely needs more
        // steps in one turn than any fixed number a default can guess.
        if budget.max_steps > 0 && steps_this_turn >= budget.max_steps {
            return Some(
                self.finish(
                    ResultStatus::BudgetExceeded {
                        limit: format!("max_steps={}", budget.max_steps),
                    },
                    self.terminal_account(state, builder),
                    state.usage,
                    state.turn,
                    builder,
                )
                .await,
            );
        }
        if let Some(deadline) = budget.deadline {
            if Utc::now() >= deadline {
                return Some(
                    self.finish(
                        ResultStatus::BudgetExceeded {
                            limit: format!("deadline={deadline}"),
                        },
                        self.terminal_account(state, builder),
                        state.usage,
                        state.turn,
                        builder,
                    )
                    .await,
                );
            }
        }
        if let Some(max_tokens) = budget.max_tokens {
            let spent = state.usage.input_tokens as u64 + state.usage.output_tokens as u64;
            if spent >= max_tokens as u64 {
                return Some(
                    self.finish(
                        ResultStatus::BudgetExceeded {
                            limit: format!("max_tokens={max_tokens}"),
                        },
                        self.terminal_account(state, builder),
                        state.usage,
                        state.turn,
                        builder,
                    )
                    .await,
                );
            }
        }
        if let Some(max_tool_calls) = budget.max_tool_calls {
            if state.tool_calls >= max_tool_calls {
                return Some(
                    self.finish(
                        ResultStatus::BudgetExceeded {
                            limit: format!("max_tool_calls={max_tool_calls}"),
                        },
                        self.terminal_account(state, builder),
                        state.usage,
                        state.turn,
                        builder,
                    )
                    .await,
                );
            }
        }
        None
    }

    /// The immediate-path terminus for every `self.cancel.is_cancelled()`
    /// check in [`Self::run_inner`].
    /// looks up `self.deps.tree.cancel_reason(self.agent_id)` -- the reason
    /// [`crate::tree::AgentTree::cancel`] stashed if THIS agent was itself
    /// the direct target of a `Runtime::cancel`/`conway_cancel` call -- and
    /// carries it into the terminal `ResultStatus::Cancelled`, agreeing with
    /// the graceful path's `pending_cancel` mailbox mechanism (above, at the
    /// top of `run_inner`'s loop), which has always carried its caller-
    /// supplied reason this way.
    ///
    /// Falls back to the pre-existing literal `"cancelled"` when there is no
    /// stashed reason -- true for a descendant whose token was tripped only
    /// by an ancestor's cancellation propagating structurally (that
    /// descendant was never itself named in a `cancel` call, so there is no
    /// truthful reason to attach; see `AgentTree::cancel`'s own doc), and
    /// also true for the pre-existing deadline-triggered token trip
    /// (`supervisor.rs`'s deadline arm calls `cancel.cancel()` directly, with
    /// no reason at all -- `check_budget` normally catches a deadline first,
    /// but this fallback keeps that race harmless either way).
    ///
    /// Reports [`Self::terminal_account`] as its trailing text, not `""` --
    /// a cancellation is exactly the non-natural termination this item's
    /// fix targets: a cancelled agent that had already written real work
    /// (files, a partial reply) must not read as though it did nothing.
    async fn finish_cancelled(&self, state: &LoopState, builder: &ResultBuilder) -> AgentResult {
        let reason = self
            .deps
            .tree
            .cancel_reason(self.agent_id)
            .unwrap_or_else(|| "cancelled".to_string());
        self.finish(
            ResultStatus::Cancelled { reason },
            self.terminal_account(state, builder),
            state.usage,
            state.turn,
            builder,
        )
        .await
    }

    /// Converts a bubbled-up `RuntimeError` into a terminal `AgentResult`.
    /// `RuntimeError::Cancelled` maps to `ResultStatus::Cancelled` (no fatal
    /// error event: this is a graceful stop, not a failure); everything
    /// else maps to `ResultStatus::Failed` with exactly one
    /// `Event::Error { fatal: true }`.
    ///
    /// Only called from [`Self::run`]'s catch of [`Self::run_inner`]'s `Err`
    /// path, after the turn loop's own `ResultBuilder` has already gone out
    /// of scope with it -- this constructs a fresh, empty one instead. That
    /// still loses any artifacts/report accumulated in earlier turns of a
    /// run that then hit a late I/O error (no criterion here requires
    /// facts/artifacts fidelity on a `Failed` result) -- but summary
    /// fidelity is a DIFFERENT trade-off, and this item withdraws the
    /// acceptance an earlier revision of this doc made of losing it too:
    /// `state` -- the `(RuntimeError, LoopState)` error tuple every
    /// `try_rt!` site threads all the way out of `run_inner` -- still
    /// carries `last_assistant_text` from whatever turn last completed
    /// before the failure, and [`Self::terminal_account`] (not `""`) is
    /// what this fn now passes as trailing text, so a late I/O error no
    /// longer discards the agent's own account of what it said just because
    /// the fresh `ResultBuilder` it discards along with it holds no report
    /// or tool artifacts of its own.
    ///
    /// ## The third `RuntimeError::Cancelled` site
    ///
    /// `RuntimeError::Cancelled` reaches this fn from exactly one production
    /// site: `attempt.rs`'s `run_generate`/`run_stream`, when a cancel lands
    /// while a backend call is actually in flight (`tokio::select!` races
    /// `cancel.cancelled()` against `backend.generate`/`stream`) rather than
    /// at one of this loop's own turn-boundary checks. That site has no
    /// `AgentTree` handle -- `AttemptEngine` is deliberately backend/routing
    /// machinery only -- so it cannot look up the caller's own reason and
    /// hardcodes a generic `"attempt cancelled"` on the `RuntimeError` it
    /// returns (see that call site's own comment).
    ///
    /// Rather than plumb a tree handle into `AttemptEngine` for this one
    /// case, this loop -- which already performs the identical lookup for
    /// the turn-boundary path ([`Self::finish_cancelled`]
    ///) -- performs the SAME
    /// `tree.cancel_reason(self.agent_id)` lookup here and prefers it over
    /// `err`'s generic reason. Because [`crate::tree::AgentTree::cancel`]
    /// stashes the reason BEFORE it trips the token, the stashed reason is
    /// always present by the time an in-flight attempt observes the trip and
    /// unwinds into this fn -- so `Some` is returned deterministically
    /// whenever THIS agent was itself the direct target of the cancel.
    /// `None` (an unknown agent, or a descendant whose token was tripped
    /// only by an ancestor's cancellation propagating structurally --
    /// `AgentTree::cancel`'s own doc) falls back to `err`'s own reason,
    /// unchanged, exactly as before this item.
    async fn finish_error(&self, state: &LoopState, err: RuntimeError) -> AgentResult {
        let builder = ResultBuilder::new();
        if let RuntimeError::Cancelled { reason, .. } = err {
            let reason = self
                .deps
                .tree
                .cancel_reason(self.agent_id)
                .unwrap_or(reason);
            return self
                .finish(
                    ResultStatus::Cancelled { reason },
                    self.terminal_account(state, &builder),
                    state.usage,
                    state.turn,
                    &builder,
                )
                .await;
        }
        self.deps.bus.emit(
            self.session,
            self.agent_id,
            Event::Error {
                error: ConwayError::from(err.clone()),
                fatal: true,
            },
        );
        self.finish(
            ResultStatus::Failed {
                error: err.to_string(),
            },
            self.terminal_account(state, &builder),
            state.usage,
            state.turn,
            &builder,
        )
        .await
    }

    /// Builds the terminal `AgentResult`, persists it (best-effort — a
    /// store failure here is logged, never propagated, since `finish` must
    /// always produce a value), publishes it to the tree, and -- only if
    /// that publication was the first one for this agent -- emits
    /// `AgentFinished` and delivers it to the parent's mailbox.
    ///
    /// ## Carried follow-up (/): the tree-publish gate
    ///
    /// `AgentTree::publish_result` is set-once (`tree.rs`): its
    /// first caller for a given agent gets `Ok(true)`, every later caller
    /// gets `Ok(false)`. Calling it *here*, before emitting, means this is
    /// the one place a normal completion and the supervisor's own
    /// grace-timeout synthesis (`supervisor.rs`) race for real: whichever
    /// publishes first is the one that emits `Event::AgentFinished` and
    /// delivers the result upward; the loser's local `result` value is
    /// still returned (so `run()`'s caller — ultimately the supervisor's
    /// own `Outcome::from_join` — still sees a real, non-synthesized
    /// result), but produces no second event and no second parent
    /// delivery.
    ///
    /// This is only ONE side of the race's closure — not, as an earlier
    /// revision of this doc claimed, the whole of it. `supervisor.rs`'s
    /// `Outcome::Synthesized` branch (a caught panic, or a task still
    /// unresponsive after `grace` and `abort()`'d) must gate ITS emission
    /// on winning the very same `publish_result` CAS too: `task.abort()` is
    /// cooperative, so an aborted task can keep running past the abort
    /// request and reach this very `finish` method after the supervisor has
    /// already given up on joining it, legitimately winning the CAS in that
    /// gap. Before An earlier review found: finding S1, `supervisor.rs` emitted
    /// unconditionally on that path regardless of whether it had actually
    /// won, so the race was only half-closed even with this gate in place.
    /// See `supervisor.rs`'s own module doc for that side's fix; together
    /// the two gates make at most one `Event::AgentFinished` observable per
    /// agent, from whichever side wins.
    ///
    /// `publish_result`'s only error is `AgentNotFound` (this agent was
    /// never `attach`ed to the tree at all — true of some unit tests that
    /// construct an `AgentLoop` directly without a `Runtime`); that case
    /// defaults to "first" so those tests keep observing `AgentFinished`
    /// exactly as before this item.
    async fn finish(
        &self,
        status: ResultStatus,
        trailing_text: impl Into<String>,
        usage: Usage,
        steps_taken: u32,
        builder: &ResultBuilder,
    ) -> AgentResult {
        // precedence between an explicit `report` tool call and
        // trailing assistant text -- and the non-empty-summary /
        // status-naming-fallback guarantee -- are both resolved here, in
        // one place, for every terminal path.
        let parts = builder.resolve(&trailing_text.into(), &status);
        let mut result = AgentResult::new(self.agent_id, self.session, status, parts.summary);
        result.facts = parts.facts;
        result.artifacts = parts.artifacts;
        result.structured = parts.structured;
        result.usage = usage;
        result.steps_taken = steps_taken;

        match self.deps.store.head(&self.session).await {
            Ok(seq) => {
                let record = LogRecord::AgentResultRecord {
                    seq,
                    ts: Utc::now(),
                    result: result.clone(),
                };
                if let Err(err) = self.deps.store.append(&self.session, record).await {
                    tracing::error!(
                        agent = %self.agent_id,
                        error = %err,
                        "failed to persist terminal AgentResult"
                    );
                }
            }
            Err(err) => {
                tracing::error!(
                    agent = %self.agent_id,
                    error = %err,
                    "failed to read session head while persisting terminal AgentResult"
                );
            }
        }

        let is_first = self
            .deps
            .tree
            .publish_result(self.agent_id, result.clone())
            .unwrap_or(true);

        if is_first {
            let ephemeral = self.deps.tree.ephemeral_of(self.agent_id);
            // `EventBus.seqs` still leaks for spawned and
            // forked agents. `is_prunable_on_finish` is read here, before
            // `emit_pruning`'s own lock is ever taken -- see that method's
            // doc for why this adds no new contention to the bus's
            // critical section.
            let prune = self.deps.tree.is_prunable_on_finish(self.agent_id);
            self.deps.bus.emit_pruning(
                self.session,
                self.agent_id,
                Event::AgentFinished {
                    result: result.clone(),
                    ephemeral,
                },
                prune,
            );
            if let Some(parent_mailbox) = &self.parent_mailbox {
                parent_mailbox.send(AgentMessage::Result {
                    from: self.agent_id,
                    result: result.clone(),
                });
            }
            // `child_reported`:
            // observation-only, gated on the SAME `is_first` publish-race
            // winner as the `Event::AgentFinished` emit and the mailbox
            // delivery immediately above -- so this fires exactly once per
            // agent, never once per race participant. Only when this agent
            // HAS a parent: a root's own finish is not "a child reporting"
            // (`self.parent_mailbox` is `None` for every root -- see that
            // field's own construction in `runtime.rs`/`subagent.rs` --
            // deliberately checked on `self.parent`, the SAME condition,
            // rather than re-deriving it).
            //
            // `supervisor.rs`'s `Outcome::Synthesized` branch dispatches the
            // IDENTICAL event, under the identical `won`/`is_first`-shaped
            // gate, for the one case that can race THIS call out from under
            // itself: a panic, or a task still unresponsive past
            // `supervisor::DEFAULT_GRACE` -- see that module's own doc.
            if let Some(parent) = self.parent {
                let hooks = self.deps.tool_runner.hooks();
                if hooks.will_dispatch(crate::hook_dispatch::CHILD_REPORTED) {
                    hooks
                        .dispatch(
                            crate::hook_dispatch::CHILD_REPORTED,
                            serde_json::json!({
                                "agent_id": self.agent_id,
                                "parent": parent,
                                "session": self.session,
                                "result": result,
                            }),
                        )
                        .await;
                }
            }
        }
        result
    }
}

/// `spec.headroom_override` if set, else the policy's resolution for the
/// agent's role. Resolved exactly once per turn by the caller, into a
/// local reused for both the `RouteRequest` and the `AttemptRequest` — see
/// the module doc's reconciliation note.
fn resolve_headroom(spec: &AgentSpec, policy: &HeadroomPolicy) -> u32 {
    spec.headroom_override
        .unwrap_or_else(|| policy.resolve(&spec.role))
}

/// Per-segment METADATA for a context-editing script hook's own payload
/// (`request_assembled`/`context_overflow`, board item
/// `01KZRZZP6A4A27R3EN0HQAENBS`) -- id (needed to EXCLUDE a segment by it),
/// role, provenance, and estimated tokens, deliberately NEVER `content`.
///
/// **The design question this item's own spec asked to be settled first:**
/// "whether the script sees the whole payload or a summary." Shipping full
/// segment bodies to a subprocess on every turn is an unbounded,
/// content-proportional cost paid whether or not a hook ever looks at most
/// of it; metadata is proportional to segment COUNT, not transcript size,
/// and is enough to (a) name a target for `ContextDelta::excludes` and (b)
/// drive a role/provenance-based policy ("exclude every `ToolResult` older
/// than N calls") without reading any of the actual conversation. A hook
/// that genuinely needs a segment's text has no reach for it today -- that
/// is a disclosed limit of this shape, not silently worked around, and
/// matches `schema::HooksConfig`'s own established precedent of shipping a
/// summary rather than a verbatim dump for this exact event.
fn segment_metadata_json(segments: &[PromptSegment]) -> Vec<serde_json::Value> {
    segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "id": segment.id.to_string(),
                "role": segment.role,
                "provenance": segment.provenance,
                "tokens_est": segment.tokens_est,
            })
        })
        .collect()
}

/// Concatenates every `ContentBlock::Text` in `blocks`, in order --
/// the `Completed`/`Rejected` terminal summary source (trailing assistant
/// text), and, as of `LoopState::last_assistant_text`'s own doc, ALSO run
/// over every turn's response (not only the final one) to seed the account
/// a non-natural termination reports via `AgentLoop::terminal_account`.
fn full_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

//! The `Plugin`/`Tool` ports (architecture §4.2) and the `CancellationToken`
//! used to interrupt an in-flight tool call.
//!
//! **There is exactly one extension mechanism (GP-03).** Built-in
//! read/write/edit/bash and the subagent tool are `Plugin` implementations
//! registered by default in `ConwayBuilder`; nothing about them is
//! privileged. MVP plugins are in-process `Arc<dyn Plugin>` (Tension T-8).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::content::{Artifact, ContentBlock, ToolCall, ToolSpec, TruncationPolicy};
use crate::error::{CwdError, ToolError};
use crate::ids::{AgentId, ModelRef, SessionId, ToolName};
use crate::ports::{EventSinkHandle, SubagentHost};
use crate::segment::PromptSegment;

/// A source of tools: a plugin declares its identity and the tools it
/// provides.
///
/// **There is no initialization hook, deliberately.** This trait carried an
/// `on_init(&PluginInitCtx)` method documented as "called once at startup"
/// that **nothing ever called** — no call site existed anywhere in the
/// workspace, and no implementor, built-in or otherwise, overrode it. A hook
/// that silently never runs is worse than an absent one: an absent hook is a
/// known limitation, an unwired one is a trap that costs an implementer a
/// debugging session to discover.
///
/// It is not merely unwired but hard to wire, which is why it was removed
/// rather than connected. `PluginRegistry::from_plugins` is synchronous and
/// eager — it calls `tools()` and compiles every schema at construction — and
/// `Runtime::new` builds it, so there is no natural point at which a fallible,
/// I/O-performing hook could run and have its failure mean anything useful.
/// A plugin needing setup does it in its own constructor, before
/// `ConwayBuilder::with_plugin`, where errors surface to the embedder
/// directly.
///
/// If a genuine lifecycle hook is wanted later, the out-of-process plugin
/// design (`.design/d1-transport.md`) specifies a real handshake with a
/// defined failure mode — that, not a resurrected no-op, is the shape to
/// build.
pub trait Plugin: Send + Sync + 'static {
    /// This plugin's static identity: id, semver, provided tools, required
    /// host capabilities.
    fn manifest(&self) -> PluginManifest;

    fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

/// One invocable tool: aligned with ACP's tool-call categories (`ToolCategory`
/// in `content.rs`) for free future compatibility, zero present cost.
#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// This tool's name, description, JSON Schema, category, and permission
    /// class.
    fn spec(&self) -> ToolSpec;

    /// Invoke the tool.
    ///
    /// PRE: `call.arguments` has already been validated against
    /// `self.spec().schema`. PRE: permission has already been granted for
    /// `(agent, tool, arguments)`. POST: honors `ctx.cancel`; returns within
    /// the runtime's deadline or `Err(ToolError::Cancelled)`. POST: declares
    /// a `TruncationPolicy` on the returned `ToolOutput`; the runtime applies
    /// it and records the truncation in the log (architecture §8).
    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;

    /// Renders this proposed call as a single human-readable line: the text
    /// behind `PermissionRequest::rendered` (permission prompt display,
    /// `Event::PermissionRequested`, and any future audit log), and --
    /// for a tool whose rendering is a shell-command-shaped string -- the
    /// text `conway_core::permission_pattern::PatternRule` prefix-matches
    /// against.
    ///
    /// PRE: `args` has already been validated against `self.spec().schema`
    /// by the caller. It is nonetheless UNTRUSTED, model-supplied content
    /// (P-10): an implementation MUST NOT panic on any `serde_json::Value`
    /// shape (no `unwrap`/`expect`/indexing into `args`), since a caller
    /// that skips validation, or a future validator bug, must not turn a
    /// bad render into a crash. Callers additionally sanitize the returned
    /// string for control bytes before display -- see
    /// `conway_runtime::tools::runner`'s render seam -- so an implementation
    /// need not do that itself, only avoid panicking.
    ///
    /// The default reproduces this trait's original, pre-per-tool-render
    /// behavior: a generic `name(args)` one-liner. It is correct for any
    /// tool whose call has no natural single command-string representation
    /// (`read`, `edit`, the subagent tools, ...). A tool whose call IS
    /// meaningfully a shell command -- `bash` -- overrides this to return
    /// that bare command string instead, so `PatternRule`'s prefix matching
    /// (designed against a shell command, not a JSON debug dump) has
    /// something legible to operate on.
    fn render(&self, args: &serde_json::Value) -> String {
        format!("{}({})", self.spec().name, args)
    }

    /// Declares which top-level fields of a call's `arguments` (the same
    /// shape `self.spec().schema` validates) carry filesystem paths, for a
    /// later permission-broker root-containment check. **This slice ships
    /// only the declaration** -- no `PermissionCtx`/broker reads this yet.
    ///
    /// PRE (mirrors `render`'s own PRE, P-10): a caller MUST NOT rely on
    /// this to be internally consistent with untrusted `args` at runtime --
    /// it is static, call-independent metadata about the tool's *schema*,
    /// not a computation over one call's actual JSON. (A method that
    /// inspected `args` to compute paths would need an extra RPC round trip
    /// for an out-of-process plugin; a field-name list survives the wire
    /// intact — GP-03/GP-11's "core ships mechanism + declarative config".)
    ///
    /// The default is [`PathArgs::Unconfinable`], **not** "no declared
    /// paths": "no paths" defaulting to "nothing to check, therefore allow"
    /// would silently unconfine every tool that doesn't override this
    /// method, including every third-party [`Tool`] impl (a known hazard in
    /// this feature's inventory). `Unconfinable` does not mean "deny" --
    /// see the type's own doc for what it does mean, and why that keeps the
    /// default from being a brick wall.
    fn path_args(&self) -> PathArgs {
        PathArgs::default()
    }

    /// Declares whether [`Self::render`]'s OUTPUT can be interpreted by a
    /// shell, for `conway_core::permission_pattern::PatternRule`'s
    /// metacharacter gate (board item 01KYT3NSWRHMPEAXVXRJ73KDYR).
    ///
    /// **Orthogonal to [`Self::path_args`].** `path_args` asks "which
    /// arguments are filesystem paths a root-containment check can
    /// confine"; this asks "is `render`'s OUTPUT itself something a shell
    /// would interpret". They diverge for real, shipped tools: `report`
    /// declares `PathArgs::Unconfinable` (its `artifacts[].path` is nested
    /// inside an array, which `PathArgs::Named`'s top-level-only vocabulary
    /// cannot express), but its `render` output is `report`'s own JSON
    /// dump, never handed to a shell -- reusing `path_args` as this gate's
    /// signal would leave `report:*` permanently inert for a reason that
    /// has nothing to do with why the gate exists. `bash` needs both
    /// answered independently too, just the other way: its `command` is
    /// unconfinable (a shell command reaches any path) AND
    /// shell-interpretable (it genuinely IS the string handed to a shell).
    ///
    /// **The default is [`RenderKind::ShellCommand`] -- the conservative
    /// choice, matching the metacharacter gate's behavior before this
    /// method existed** (every `rendered` string was gated, unconditionally,
    /// for every tool). A tool that does not override this method is
    /// exactly as gated as it was before this method existed: its pattern
    /// grants may stay inert if its `render` output happens to contain a
    /// shell metacharacter (as the trait's default JSON-dump `render` does,
    /// via `(`, `)`, `{`, `}`), but that is a missed convenience, never a
    /// missed prompt. Deliberately asymmetric with `path_args`'s
    /// `Unconfinable` default: there, "no declared paths" defaulting to
    /// "allow" was the hazard; here, the hazard runs the other way -- a
    /// tool that overrides [`Self::render`] to emit something
    /// shell-interpretable (as `bash` does) and does NOT also flip this to
    /// `ShellCommand` would silently defeat the chaining gate the moment it
    /// is pattern-matched. Declaring [`RenderKind::Structured`] is
    /// therefore an explicit, deliberate claim a tool author makes about
    /// their own `render` output -- see `conway_tools`' generic test
    /// (`render_kind_is_consistent_with_whether_render_is_overridden` in
    /// `conway-tools/tests/builtins.rs`) for the guard that makes this claim
    /// checkable, not merely aspirational: it can only ever be made
    /// truthfully by a tool that keeps this trait's own default `render`
    /// untouched.
    fn render_kind(&self) -> RenderKind {
        RenderKind::default()
    }
}

/// Declarative metadata: whether a [`Tool`]'s [`Tool::render`] output can be
/// interpreted by a shell, consumed by
/// `conway_core::permission_pattern::PatternRule`'s metacharacter gate to
/// decide whether that gate applies to this tool's pattern grants at all.
/// See [`Tool::render_kind`]'s own doc for the full rationale, including why
/// this is a SEPARATE declaration from [`PathArgs`] rather than a reuse of
/// it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderKind {
    /// `render`'s output is NOT interpreted by a shell -- a debug-shaped
    /// dump (the trait's own default `name(args)`), or any other rendering
    /// that is never handed to a shell for execution. Shell
    /// metacharacters appearing in it (JSON's `{`/`}`, a path's `(`, ...)
    /// are incidental syntax, not command-injection risk, so
    /// `PatternRule`'s metacharacter gate does not apply to this tool's
    /// pattern grants.
    Structured,
    /// `render`'s output IS (or could be) the string a shell interprets --
    /// shell metacharacters within it can genuinely extend the effective
    /// command past whatever prefix a pattern matched. `PatternRule`'s
    /// metacharacter gate MUST apply.
    ShellCommand,
}

impl Default for RenderKind {
    /// Fails closed: see [`Tool::render_kind`]'s own doc for why this is
    /// the conservative choice, not [`RenderKind::Structured`].
    fn default() -> Self {
        RenderKind::ShellCommand
    }
}

/// Declarative metadata: which of a [`Tool`]'s call argument names carry
/// filesystem paths. Consumed (in a later slice) by the permission broker to
/// decide whether a call can be auto-allowed under an operator-configured
/// root, or must always fall through to the gate.
///
/// **`Unconfinable` is the safe default, and it does NOT mean "deny."** It
/// means "this tool's arguments cannot be statically confined to a root, so
/// a root-containment check must always fall through to the operator's
/// gate" -- the same asymmetry `conway_core::permission_pattern`'s
/// metacharacter gate is built on (an unnecessary prompt costs a keystroke;
/// a missed one costs arbitrary execution). A tool that never overrides
/// [`Tool::path_args`] is exactly as gated as it is today -- nothing is
/// silently auto-allowed by adding this trait method.
///
/// `bash` needs BOTH of these facts about itself at once: its `command`
/// string is unconfinable (a shell command can reach any path via
/// redirection, substitution, `cd`, subprocess invocation, ...) AND its
/// optional `cwd` argument, when present, IS a path a root check could
/// usefully confine. `Unconfinable`'s `checkable` field carries that second
/// fact *alongside* the first, in the same variant, rather than needing a
/// second top-level enum variant or a struct-of-two-independent-flags
/// shape — so an enforcing call site never special-cases `bash`: it always
/// asks "is this call unconfinable, and if so, is there anything checkable
/// anyway", one match, no `if name == "bash"`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathArgs {
    /// This tool takes no path arguments at all.
    None,
    /// These top-level argument names (fields of `ToolCall::arguments`)
    /// carry filesystem paths a root-containment check can evaluate.
    Named(&'static [&'static str]),
    /// This tool's call cannot be statically confined to a root (e.g. a
    /// free-form shell command) -- a root-containment check must always
    /// fall through to the operator's gate for this call. `checkable`
    /// names any argument that nonetheless IS a checkable path (e.g.
    /// `bash`'s `cwd`); empty when nothing about the call is checkable.
    Unconfinable { checkable: &'static [&'static str] },
}

impl Default for PathArgs {
    /// Fails closed: see this type's own doc and [`Tool::path_args`]'s.
    fn default() -> Self {
        PathArgs::Unconfinable { checkable: &[] }
    }
}

/// A plugin's static identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    pub tools: Vec<ToolName>,
    pub required_host_caps: Vec<String>,
}

/// A plugin's untyped configuration values, as loaded and handed down by the
/// facade. This crate does no config loading itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginConfig {
    pub values: serde_json::Map<String, serde_json::Value>,
}

/// A shared, mutable "current working directory" cell -- the capability
/// underlying a future `cd` tool (S1 ships only the capability itself; no
/// built-in tool calls [`Self::set`] yet).
///
/// **Modeled directly on Unix's `getcwd()`/`chdir()` split, not a single
/// mutable variable.** [`Self::current`] is `getcwd()`: a snapshot, never
/// fails, always returns *some* path. [`Self::set`] is `chdir()`: a
/// distinct, separately-fallible operation. [`ToolCtx::cwd`] stays exactly
/// the snapshot `PathBuf` it always was -- this handle is the mutable cell a
/// tool can `chdir()` through, cheaply cloned (an `Arc` refcount bump) into
/// every [`ToolCtx`] that shares it.
///
/// **No per-batch race, by construction of the call site, not this type.**
/// `conway_runtime::tools::runner::ToolRunner::run_batch` reads
/// [`Self::current`] exactly once, before spawning any concurrent tool
/// invocation for that batch, so every tool dispatched together observes the
/// identical snapshot regardless of completion order -- a `set` from
/// another tool in the same batch becomes visible starting the NEXT batch,
/// never the one it ran in. See that function's own doc for why this is
/// deliberate (Unix threads share one process `cwd` under the identical
/// constraint; this mirrors, rather than emulates around, that fact).
///
/// **Internal representation: `Arc<RwLock<PathBuf>>`, not a channel.** A
/// channel would still need a receiver task somewhere applying updates to a
/// value readers can see -- more moving parts for the same
/// single-writer(-at-a-time)/many-readers shape a lock expresses directly.
/// `set`'s critical section is one pointer-sized assignment with no `.await`
/// inside the guard, so the usual "never hold a `std` lock across an await
/// point" hazard does not apply here.
#[derive(Clone, Debug)]
pub struct CwdHandle {
    inner: Arc<RwLock<PathBuf>>,
}

impl CwdHandle {
    /// A fresh cell seeded with `initial`.
    pub fn new(initial: PathBuf) -> Self {
        Self {
            inner: Arc::new(RwLock::new(initial)),
        }
    }

    /// A snapshot of the cell's current value. Mirrors `getcwd()`: never
    /// fails. If some other clone of this handle poisoned the lock (a panic
    /// while holding [`Self::set`]'s write guard), the last successfully
    /// written value is recovered rather than the poison being propagated
    /// (P-10): a read has no natural failure mode, and inventing one here
    /// would force every caller -- including the runtime's per-batch
    /// snapshot -- to handle an error this operation should never surface.
    pub fn current(&self) -> PathBuf {
        match self.inner.read() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Sets the cell to `path`, unconditionally. This slice performs no
    /// root/containment check on `path` (see the item's "out of scope":
    /// confinement does not exist yet, and cwd was never the boundary).
    ///
    /// P-10: `path` is model-influenced. This never panics on any input --
    /// `path` is stored, never inspected or parsed -- and the one failure
    /// mode this can hit (a poisoned lock) is reported as a typed error
    /// rather than propagated as a panic or silently swallowed.
    pub fn set(&self, path: PathBuf) -> Result<(), CwdError> {
        let mut guard = self.inner.write().map_err(|_| CwdError::Poisoned)?;
        *guard = path;
        Ok(())
    }
}

/// Per-invocation context handed to `Tool::invoke`.
///
/// `Clone` (every field is an `Arc`, `Copy`, or otherwise cheap to clone).
/// **Not** `Serialize` — it holds trait objects (`events`, `subagents`).
/// This is the known T-8 limitation: `ToolCall` and `ToolOutput` are fully
/// serializable, so a future subprocess/RPC plugin transport only needs an
/// RPC-shaped form of `ToolCtx`, not this one.
///
/// **Deliberately NOT `#[non_exhaustive]`, and that is a decision, not an
/// oversight.** Adding a field here is a breaking change for any external
/// struct-literal construction, so the question comes up every time one is
/// added (`chdir` was the most recent). The reasons to leave it off:
///
/// - `#[non_exhaustive]` would not merely warn about literal construction,
///   it would *forbid* it outside this crate even with every field named,
///   which forces a constructor or builder. That is disproportionate for a
///   type built by hand in dozens of test fixtures.
/// - `ToolSpec` faced the same question and resolved it the same way; the
///   escape hatch there was a defaulted trait method (`Tool::path_args`),
///   which is not available for a capability that must arrive as a field.
/// - This type is already flagged above as due for a wholesale reshape for
///   the T-8 RPC-transport case. Hardening its literal-construction surface
///   now would be work spent on a shape that is expected to change.
///
/// If the project later wants that protection, the move is a builder plus
/// `#[non_exhaustive]` in one deliberate breaking change — not to add the
/// attribute alone.
#[derive(Clone)]
pub struct ToolCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    /// A snapshot of the agent's working directory as of the top of the
    /// batch this call was dispatched in -- see [`CwdHandle`]'s doc for the
    /// snapshot-once-per-batch guarantee. Unchanged by this slice: every
    /// existing consumer (`read`/`write`/`edit`/`glob`/`grep`/`bash`, every
    /// third-party tool) keeps reading this exactly as before.
    pub cwd: PathBuf,
    /// S1: the `cd` capability. Calling `chdir.set(path)` changes the
    /// working directory the NEXT batch snapshots into `cwd` -- never this
    /// call's own `cwd`, and never any other call already dispatched in the
    /// same batch (see [`CwdHandle`]'s doc). No built-in tool calls this
    /// yet; it lands on every [`Tool`] implementation uniformly (P-6), so a
    /// future `cd` tool needs no privileged access this type doesn't already
    /// expose to every plugin.
    pub chdir: CwdHandle,
    pub cancel: CancellationToken,
    /// Progress reporting; see [`EventSinkHandle`].
    pub events: EventSinkHandle,
    /// The cycle-breaker for the fork/spawn tool: the same trait object the
    /// developer API (`SessionHandle::fork`/`spawn`) calls.
    pub subagents: Arc<dyn SubagentHost>,
    pub config: Arc<PluginConfig>,
}

impl std::fmt::Debug for ToolCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolCtx")
            .field("agent_id", &self.agent_id)
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd)
            .field("chdir", &self.chdir)
            .field("cancel", &self.cancel)
            .field("events", &"<dyn EventSink>")
            .field("subagents", &"<dyn SubagentHost>")
            .field("config", &self.config)
            .finish()
    }
}

/// The outcome of a tool invocation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolOutput {
    pub blocks: Vec<ContentBlock>,
    pub is_error: bool,
    /// The tool declares how it wants oversized output handled; the runtime
    /// enforces the policy and records the truncation in the log.
    pub truncation: TruncationPolicy,
    pub artifacts: Vec<Artifact>,
}

/// The outgoing request payload a [`ContextHook`] may transform: the
/// assembled prompt segments (in send order, including the `ToolRegistry`
/// segment) and the tool set announced to the model for this turn.
///
/// **Tool announcement vs. execution (WI-126):** `tools` here is what the
/// model is TOLD it may call -- distinct from [`PermissionGate`], which
/// governs whether a call the model actually makes is allowed to run.
/// Narrowing `tools` hides a tool from the model entirely (it can never
/// propose calling it this turn); `PermissionGate` still gates every
/// proposed call regardless of what was announced. A tool a hook filters
/// out here was never a `PermissionGate` bypass -- it is simply never
/// offered.
#[derive(Clone, Debug, Default)]
pub struct ContextPayload {
    pub segments: Vec<PromptSegment>,
    pub tools: Vec<ToolSpec>,
}

/// Read-only identity/sizing context for one [`ContextHook`] invocation.
/// `estimated_tokens` reflects whatever payload is being transformed by
/// *this* call (the freshly built assembly for [`ContextHook::before_request`],
/// or the still-too-large one for [`ContextHook::on_overflow`]) -- a hook
/// does not need to recompute it itself.
#[derive(Clone, Debug)]
pub struct ContextHookCtx {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub turn: u32,
    /// The model this request is routed toward, if known yet. `None` for
    /// `before_request` on an unpinned role (routing hasn't run); `Some` for
    /// `before_request` when `AgentSpec::pin` fixes the model regardless of
    /// routing, and always `Some` for `on_overflow` (a specific route was
    /// already chosen and found to overflow by the time that fires).
    pub model: Option<ModelRef>,
    pub estimated_tokens: u32,
}

/// Why [`ContextHook::on_overflow`] fired: the same shortfall accounting as
/// `conway_core::error::RoutingError::ContextTooLarge`, so a hook can decide
/// how much to trim without the runtime recomputing anything the T-1 gate
/// already worked out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverflowInfo {
    pub max_context_tokens: u32,
    pub headroom_tokens: u32,
    /// `estimated_tokens + headroom_tokens`, saturating.
    pub required_tokens: u32,
    /// `required_tokens - max_context_tokens`, saturating.
    pub shortfall_tokens: u32,
}

/// Pluggable per-call context/tool curation (WI-126, architecture's
/// unifying hook primitive): invoked before every LLM request, with an
/// optional second invocation if the first invocation's output still
/// overflows the routed model's window.
///
/// **No hook registered is the whole contract for "default behavior
/// unchanged":** the runtime holds this as `Option<Arc<dyn ContextHook>>`
/// and never invokes anything when it is `None` -- not even a no-op
/// pass-through call. `conway-core` ships no implementation and no built-in
/// curation policy; every consumer (CLI/IDE/embedder) that wants masking,
/// system-prompt instrumentation, tool-announcement narrowing, or
/// overflow-time compaction supplies its own.
///
/// **One trait, three transforms:** `before_request`'s `ContextPayload`
/// bundles segments and announced tools together because the runtime treats
/// them as one outgoing request -- a hook can edit/drop a segment (e.g. the
/// `AgentDef`-provenance segment, to augment the system prompt; or any
/// segment, to apply an ad hoc exclusion mirroring WI-125's persisted
/// `ContextMask`) and/or narrow `tools` (announcement filtering) in the same
/// call. Async so an inference-driven hook can issue its own LLM call to
/// decide (criterion: "hooks may be pure scripts OR issue their own LLM
/// call").
///
/// **Overflow is a distinct, optional method, not a flag on
/// `before_request`:** `on_overflow` only fires when the *already-hooked*
/// payload still doesn't fit the routed model's window (the runtime's T-1
/// gate). Its default returns `None`, which the runtime treats identically
/// to no hook being registered at all: a hard `ContextTooLarge`. This
/// preserves "no hook registered -> today's behavior exactly" as a
/// per-method guarantee, not just a per-trait one -- a consumer can
/// implement curation (`before_request`) without accidentally also
/// suppressing the hard overflow error.
#[async_trait]
pub trait ContextHook: Send + Sync + 'static {
    /// Invoked once per assembled request, before it is routed/sent.
    /// Returning `payload` unchanged is always a valid implementation.
    async fn before_request(&self, ctx: &ContextHookCtx, payload: ContextPayload)
        -> ContextPayload;

    /// Invoked only when `before_request`'s output still exceeds the routed
    /// model's window. `Some(payload)` gives the runtime a smaller/edited
    /// payload to re-estimate and retry -- bounded by the runtime's own
    /// re-assembly loop, never by this trait. `None` (the default) falls
    /// through to the hard `ContextTooLarge` error.
    async fn on_overflow(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
        overflow: OverflowInfo,
    ) -> Option<ContextPayload> {
        let _ = (ctx, payload, overflow);
        None
    }
}

/// A minimal, serialization-free cancellation flag.
///
/// `conway-core` cannot depend on `tokio`, so this is a small
/// `Arc<AtomicBool>`-based token rather than `tokio_util::sync::
/// CancellationToken`. Downstream crates that need an async cancellation
/// *await* (rather than a poll of `is_cancelled`) bridge this token to
/// `tokio_util`'s token themselves — see `conway-runtime`.
///
/// `child()` produces a token that observes both its own cancellation and
/// every ancestor's, to arbitrary depth: internally each child holds a
/// shared handle to its parent (rather than the parent's raw flag alone), so
/// cancelling a root token cancels every descendant transitively.
#[derive(Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
    parent: Option<Arc<CancellationToken>>,
}

impl std::fmt::Debug for CancellationToken {
    // Manual impl: a derived Debug would walk (and print) the entire ancestor
    // chain, which is unbounded in a deep agent tree.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .field("has_parent", &self.parent.is_some())
            .finish()
    }
}

impl CancellationToken {
    /// A fresh, uncancelled, parentless token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks this token cancelled. Every token derived from it via
    /// [`Self::child`] (to any depth) observes this immediately.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    /// `true` if this token, or any ancestor it was derived from, has been
    /// cancelled. Iterative: walks the ancestor chain without recursion, so
    /// arbitrarily deep agent trees cannot overflow the stack.
    pub fn is_cancelled(&self) -> bool {
        let mut current = self;
        loop {
            if current.flag.load(Ordering::SeqCst) {
                return true;
            }
            match current.parent.as_deref() {
                Some(parent) => current = parent,
                None => return false,
            }
        }
    }

    /// A new token that is independently cancellable but also observes this
    /// token's (and its ancestors') cancellation.
    pub fn child(&self) -> CancellationToken {
        CancellationToken {
            flag: Arc::new(AtomicBool::new(false)),
            parent: Some(Arc::new(self.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let token = CancellationToken::new();
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_is_observed() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn child_observes_parent_cancellation() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn child_can_be_cancelled_independently_of_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn grandchild_observes_root_cancellation() {
        let root = CancellationToken::new();
        let child = root.child();
        let grandchild = child.child();
        assert!(!grandchild.is_cancelled());
        root.cancel();
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn plugin_manifest_round_trips() {
        let manifest = PluginManifest {
            id: "builtin.fs".into(),
            version: "0.1.0".into(),
            tools: vec![ToolName::new("read"), ToolName::new("write")],
            required_host_caps: vec![],
        };
        let json = serde_json::to_string(&manifest).unwrap();
        let back: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest, back);
    }

    #[test]
    fn tool_output_round_trips() {
        let out = ToolOutput {
            blocks: vec![ContentBlock::Text { text: "ok".into() }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: vec![],
        };
        let json = serde_json::to_string(&out).unwrap();
        let back: ToolOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(out, back);
    }

    fn hook_ctx() -> ContextHookCtx {
        ContextHookCtx {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            turn: 0,
            model: Some(ModelRef {
                backend: crate::ids::BackendId::new("anthropic"),
                model: crate::ids::ModelId::new("claude-sonnet-4-6"),
            }),
            estimated_tokens: 100,
        }
    }

    fn segment(text: &str) -> PromptSegment {
        PromptSegment::new(
            crate::content::Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            crate::provenance::Provenance::UserPrompt,
        )
    }

    /// A hook that drops every segment whose text contains "secret" and
    /// otherwise passes the payload through unchanged -- exercises the
    /// mask-like "drop a segment" transform (criterion 1a) without any
    /// dependency on WI-125's persisted `ContextMask`.
    struct DropSecretsHook;

    #[async_trait]
    impl ContextHook for DropSecretsHook {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            mut payload: ContextPayload,
        ) -> ContextPayload {
            payload.segments.retain(|s| {
                !s.content.iter().any(|b| match b {
                    ContentBlock::Text { text } => text.contains("secret"),
                    _ => false,
                })
            });
            payload
        }
    }

    #[test]
    fn before_request_can_drop_a_segment() {
        let hook: Arc<dyn ContextHook> = Arc::new(DropSecretsHook);
        let payload = ContextPayload {
            segments: vec![segment("hello"), segment("the secret plan")],
            tools: vec![],
        };
        let out = block_on(hook.before_request(&hook_ctx(), payload));
        assert_eq!(out.segments.len(), 1);
    }

    /// The default `on_overflow` -- what every hook gets unless it opts in
    /// by overriding it -- must return `None`, which the runtime treats
    /// identically to no hook being registered (hard `ContextTooLarge`).
    struct BeforeRequestOnlyHook;

    #[async_trait]
    impl ContextHook for BeforeRequestOnlyHook {
        async fn before_request(
            &self,
            _ctx: &ContextHookCtx,
            payload: ContextPayload,
        ) -> ContextPayload {
            payload
        }
    }

    #[test]
    fn default_on_overflow_is_none() {
        let hook = BeforeRequestOnlyHook;
        let payload = ContextPayload {
            segments: vec![segment("hi")],
            tools: vec![],
        };
        let overflow = OverflowInfo {
            max_context_tokens: 100,
            headroom_tokens: 10,
            required_tokens: 200,
            shortfall_tokens: 100,
        };
        let out = block_on(hook.on_overflow(&hook_ctx(), payload, overflow));
        assert!(out.is_none());
    }

    /// Dependency-free async-test helper (`conway-core` has no `tokio`/
    /// `futures-executor` dependency, even in dev-deps): every hook exercised
    /// by this module's tests does no real `.await`ing internally, so a
    /// single poll with a no-op waker always resolves `Ready` -- this is not
    /// a general-purpose executor, just enough to drive `async_trait`'s
    /// synchronous-bodied futures to completion in a unit test.
    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

        fn noop(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);

        let raw = RawWaker::new(std::ptr::null(), &VTABLE);
        let waker = unsafe { Waker::from_raw(raw) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        loop {
            if let Poll::Ready(val) = fut.as_mut().poll(&mut cx) {
                return val;
            }
        }
    }

    /// Object-safety proof (mirrors this module's own `_assert_object_safe`
    /// pattern in `ports/mod.rs`): `RuntimeDeps` needs to hold this as a
    /// trait object.
    #[test]
    fn context_hook_is_object_safe() {
        fn assert_object_safe(_: &dyn ContextHook) {}
        let hook = BeforeRequestOnlyHook;
        assert_object_safe(&hook);
    }

    // ---- Tool::render's default implementation ----

    /// A tool that accepts the trait's default `render` untouched -- proves
    /// a third-party `Tool` implementor (`ConwayBuilder::with_plugin`, GP-03)
    /// keeps compiling without implementing the new method (the widening
    /// this trait underwent to fix the "pattern grants are inert" bug).
    struct DefaultRenderTool;

    #[async_trait]
    impl Tool for DefaultRenderTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: crate::ids::ToolName::new("probe"),
                description: "test".into(),
                schema: schemars::schema_for!(serde_json::Value),
                category: crate::content::ToolCategory::Read,
                permission: crate::content::PermissionClass::Safe,
            }
        }

        async fn invoke(&self, _call: ToolCall, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
            unreachable!("not exercised by this test")
        }
    }

    #[test]
    fn default_render_reproduces_the_pre_widening_name_args_shape() {
        let tool = DefaultRenderTool;
        let rendered = tool.render(&serde_json::json!({"a": 1}));
        assert_eq!(rendered, "probe({\"a\":1})");
    }

    /// `Tool` must remain object-safe: `PluginRegistry`/third-party plugin
    /// consumers hold it as `Arc<dyn Tool>`.
    #[test]
    fn tool_is_object_safe() {
        fn assert_object_safe(_: &dyn Tool) {}
        let tool = DefaultRenderTool;
        assert_object_safe(&tool);
    }

    // ---- Tool::path_args' default ----

    /// A third-party `Tool` implementor that accepts the trait's default
    /// `path_args` untouched (same proof shape as
    /// `default_render_reproduces_...` above): the default must be
    /// `Unconfinable` with nothing checkable, never `None` -- "no declared
    /// paths" silently read as "nothing to check, therefore allow" would
    /// unconfine every tool that doesn't override this method.
    #[test]
    fn default_path_args_is_unconfinable_with_nothing_checkable() {
        let tool = DefaultRenderTool;
        assert_eq!(tool.path_args(), PathArgs::Unconfinable { checkable: &[] });
    }

    // ---- Tool::render_kind's default ----

    /// A third-party `Tool` implementor that accepts the trait's default
    /// `render_kind` untouched must get `ShellCommand`, the CONSERVATIVE
    /// choice -- never `Structured`. `Structured` skips the metacharacter
    /// gate entirely; defaulting to it would let a third-party tool that
    /// overrides `render` to emit something shell-interpretable (mirroring
    /// `bash`) silently defeat the chaining gate the moment a pattern is
    /// matched against it, with no explicit opt-in from the tool author.
    #[test]
    fn default_render_kind_is_shell_command_not_structured() {
        let tool = DefaultRenderTool;
        assert_eq!(
            tool.render_kind(),
            RenderKind::ShellCommand,
            "the default must fail closed: an undeclared render_kind must never silently \
             skip the metacharacter gate"
        );
    }

    // ---- CwdHandle (S1: the `cd` capability) ----

    #[test]
    fn fresh_handle_reports_its_seed() {
        let handle = CwdHandle::new(PathBuf::from("/a/b"));
        assert_eq!(handle.current(), PathBuf::from("/a/b"));
    }

    #[test]
    fn set_then_current_round_trips() {
        let handle = CwdHandle::new(PathBuf::from("/a/b"));
        handle.set(PathBuf::from("/c/d")).unwrap();
        assert_eq!(handle.current(), PathBuf::from("/c/d"));
    }

    /// A clone of a `CwdHandle` shares the same underlying cell (`Arc`) --
    /// this is what lets `AgentLoop` construct the cell once and clone it
    /// into every turn's `ToolBatchCtx`/`ToolCtx` and still observe writes
    /// a spawned tool task made through its own clone.
    #[test]
    fn clones_share_the_same_cell() {
        let handle = CwdHandle::new(PathBuf::from("/a"));
        let clone = handle.clone();
        clone.set(PathBuf::from("/b")).unwrap();
        assert_eq!(handle.current(), PathBuf::from("/b"));
    }

    /// P-10 adversarial case: a poisoned lock must not panic. Poisons the
    /// cell by panicking (via `catch_unwind`, so the test process itself
    /// survives) while holding the write guard directly -- reaching the
    /// private `inner` field is legitimate here since this test lives in
    /// the same module that defines it.
    fn poison(handle: &CwdHandle) {
        let inner = handle.inner.clone();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = inner.write().unwrap();
            panic!("deliberately poisoning the lock");
        }));
        assert!(result.is_err(), "the injected panic should have unwound");
    }

    #[test]
    fn set_after_poisoning_returns_a_typed_error_not_a_panic() {
        let handle = CwdHandle::new(PathBuf::from("/a"));
        poison(&handle);
        let result = handle.set(PathBuf::from("/b"));
        assert_eq!(result, Err(CwdError::Poisoned));
    }

    /// `current` recovers rather than propagating the poison (see its own
    /// doc): it must keep returning the last successfully written value,
    /// never panic.
    #[test]
    fn current_after_poisoning_still_returns_the_last_value_without_panicking() {
        let handle = CwdHandle::new(PathBuf::from("/a"));
        handle.set(PathBuf::from("/b")).unwrap();
        poison(&handle);
        assert_eq!(handle.current(), PathBuf::from("/b"));
    }

    /// A weird-but-legal `PathBuf` (non-UTF8 on Unix) must not panic `set`
    /// -- it is stored, never parsed (P-10: no `unwrap`/`expect` on the
    /// supplied path).
    #[test]
    #[cfg(unix)]
    fn set_accepts_non_utf8_paths_without_panicking() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let handle = CwdHandle::new(PathBuf::from("/a"));
        let weird = PathBuf::from(OsStr::from_bytes(b"/not/\xFFutf8"));
        handle.set(weird.clone()).unwrap();
        assert_eq!(handle.current(), weird);
    }
}

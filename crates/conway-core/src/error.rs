//! The complete error taxonomy for the conway workspace.
//!
//! All enums are `#[non_exhaustive]`, serde round-trippable (externally tagged,
//! owned data only), and carry `Display` messages via `thiserror`.
//!
//! The two T-1 variants (`RoutingError::ContextTooLarge`,
//! `RuntimeError::ForkContextOverflow`) are terminal by construction: no field
//! can express a truncation or escalation outcome.

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, LogSeq, ModelId, ModelRef, RoleAlias, SessionId, ToolName};
use crate::log::SubagentMode;

/// Errors produced by a `Backend` implementation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum BackendError {
    #[error("transport error: {detail}")]
    Transport { detail: String },
    #[error("rate limited (retry after {retry_after_secs:?} seconds)")]
    RateLimit { retry_after_secs: Option<u64> },
    #[error("authentication failed: {detail}")]
    Auth { detail: String },
    #[error("bad request: {detail}")]
    BadRequest { detail: String },
    #[error("server error (status {status}): {detail}")]
    ServerError { status: u16, detail: String },
    #[error("context overflow: request requires {required_tokens} tokens, model accepts at most {max_context_tokens}")]
    ContextOverflow {
        required_tokens: u32,
        max_context_tokens: u32,
    },
    /// `Backend::admit`'s rejection (headroom-and-refusal amendment, decision
    ///):
    /// this backend's own local, pre-flight estimate of `req`'s size, plus
    /// the resolved headroom, exceeds `model`'s declared window --
    /// discovered before any network call, unlike [`Self::ContextOverflow`]
    /// (which classifies a provider's own after-the-fact rejection of a
    /// request that was already sent). Distinct variant, deliberately: the
    /// two are different failure modes with different remediation, and
    /// collapsing them would blur which one actually happened. Terminal --
    /// no truncation or escalation is performed by core; every number
    /// behind the verdict travels with it rather than being collapsed into
    /// a bare boolean.
    #[error("context too large: {est_tokens} input tokens + {headroom_tokens} headroom = {required_tokens} exceeds {model}'s window of {max_context_tokens} tokens (short by {shortfall_tokens}); not trimmed or escalated")]
    ContextTooLarge {
        model: ModelId,
        /// This backend's own local estimate of the request's size.
        est_tokens: u32,
        /// Reserved output/reasoning budget, resolved by the caller from
        /// configuration and passed into `Backend::admit`.
        headroom_tokens: u32,
        /// `est_tokens + headroom_tokens`, saturating.
        required_tokens: u32,
        max_context_tokens: u32,
        /// `required_tokens - max_context_tokens`, saturating.
        shortfall_tokens: u32,
    },
    #[error("tool call parse failure: {detail}")]
    ToolParse { detail: String },
    #[error("request cancelled")]
    Cancelled,
}

impl BackendError {
    /// Whether the attempt loop should advance to the next candidate route.
    ///
    /// `ContextTooLarge` is included alongside `ContextOverflow`: both name
    /// a candidate this specific request does not fit, which is exactly
    /// the shape a fallback chain (an operator-configured list, not a
    /// silent escalation) exists to route past. What is forbidden is core
    /// inventing a substitute on its own initiative -- advancing to the
    /// NEXT entry the operator already declared is not that.
    pub fn is_failover_worthy(&self) -> bool {
        matches!(
            self,
            BackendError::Transport { .. }
                | BackendError::RateLimit { .. }
                | BackendError::ServerError { .. }
                | BackendError::ContextOverflow { .. }
                | BackendError::ContextTooLarge { .. }
        )
    }

    /// Whether this error is a signal about endpoint health.
    ///
    /// `Auth`, `BadRequest`, `ContextOverflow`, `ContextTooLarge`,
    /// `ToolParse`, and `Cancelled` are request problems, not
    /// endpoint-health signals (§8).
    pub fn is_health_signal(&self) -> bool {
        matches!(
            self,
            BackendError::Transport { .. }
                | BackendError::ServerError { .. }
                | BackendError::RateLimit { .. }
        )
    }
}

/// Errors produced by a `Tool` implementation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ToolError {
    #[error("invalid arguments: {detail}")]
    InvalidArguments { detail: String },
    #[error("permission denied: {reason}")]
    Denied { reason: String },
    #[error("tool cancelled")]
    Cancelled,
    #[error("tool timed out after {after_secs} seconds")]
    Timeout { after_secs: u64 },
    #[error("io error: {detail}")]
    Io { detail: String },
    #[error("internal tool error: {detail}")]
    Internal { detail: String },
}

/// Why a [`crate::ports::HookRunner`] invocation failed. **Every** distinct cause -- a nonzero exit,
/// a timeout, a missing/unexecutable command, or stdout that did not parse
/// as a [`crate::hook::HookAnswer`] -- lands here, uniformly, as the ONE
/// way this port reports failure: "fail-closed is the runner's job,"
/// enforced at every invocation, never merely at config-load time (the
/// trap this project has previously shipped the inverse of: a broken script
/// silently read as "no hook registered" instead of "a hook that must
/// fail").
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum HookFailure {
    /// The command ran and exited, but not with status 0. `code` is `None`
    /// when the process was killed by a signal rather than exiting normally
    /// (mirrors `std::process::ExitStatus::code`'s own `None` case).
    #[error("hook command exited nonzero: {code:?}")]
    NonzeroExit { code: Option<i32> },
    /// The command did not finish within its configured timeout and was
    /// killed (process-group SIGTERM, then SIGKILL after a grace period --
    /// see the implementing crate's process-group module).
    #[error("hook command timed out after {after_ms}ms")]
    TimedOut { after_ms: u64 },
    /// The command could not even be spawned: not found, not executable, or
    /// any other OS-level spawn failure. Also covers a malformed
    /// invocation this runner cannot even attempt (e.g. an empty `command`).
    #[error("hook command failed to spawn: {detail}")]
    Spawn { detail: String },
    /// The command exited 0 within its deadline, but its stdout was not
    /// valid JSON matching [`crate::hook::HookAnswer`]'s shape. A hook that
    /// crashes AFTER producing a well-formed answer is not this variant --
    /// only its exit status is; this is specifically "we cannot trust what
    /// it said," not "it also failed to finish cleanly."
    #[error("hook stdout did not parse as a hook answer: {detail}")]
    UnparseableAnswer { detail: String },
}

/// Errors produced by [`crate::ports::CwdHandle::set`] (S1: the `cd`
/// capability lands on `ToolCtx` as `chdir: CwdHandle`). `CwdHandle::current`
/// deliberately has no error type at all -- see its own doc for why only the
/// mutating operation can fail.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum CwdError {
    /// Some other clone of the same [`crate::ports::CwdHandle`] panicked
    /// while holding the write lock inside a prior `set` call (untrusted input: this is
    /// reported, never allowed to propagate as a panic here).
    #[error("cwd handle's lock was poisoned by a panic in a prior `set` call")]
    Poisoned,
}

/// Errors produced by [`crate::ports::ArtifactWriteHandle::write`] (board
/// item: the containment guard that makes it safe
/// for a [`crate::ports::ContextHook`] to spill content to disk).
///
/// **`OutsideRoot` is not a corner case -- it is the guard this whole port
/// exists to enforce.** See [`crate::ports::ArtifactWriteHandle`]'s own doc
/// for the resolution rule (mirrors `conway_runtime::permission::
/// resolve_like_the_tool_will` exactly -- one implementation, never restated) that
/// decides when this fires.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ArtifactWriteError {
    /// The hook-supplied name could not be resolved into a candidate path
    /// at all (untrusted input: a NUL byte, which the OS path APIs cannot represent --
    /// the same rejection `conway_tools::common::resolve_path` gives a
    /// tool's own path argument for the identical input). Distinct from
    /// [`Self::OutsideRoot`]: this candidate was never even evaluated
    /// against the root.
    #[error("artifact name could not be resolved to a path: {detail}")]
    InvalidName { detail: String },
    /// The resolved write target is not inside this agent's confinement
    /// root (or the resolved candidate could not even be evaluated -- see
    /// `conway_core::containment::Containment::Undecidable`, which this
    /// fuses with `Outside`: "can't check" is never "allow"). `path` is the
    /// resolved candidate, rendered for a human/log, never the raw
    /// hook-supplied name alone -- the whole point of this variant is
    /// showing where the write actually would have landed.
    #[error("artifact write to {path} refused: outside this agent's confinement root")]
    OutsideRoot { path: String },
    /// This agent's persisted confinement root no longer resolves on disk
    /// (`conway_runtime::permission::AgentRoot::Broken`). Fails closed,
    /// exactly like every other root-relevant call this agent makes while
    /// its root is broken -- never silently downgraded to unconfined.
    #[error("artifact write refused: this agent's confinement root could not be re-established")]
    RootBroken,
    /// The path resolved and passed containment, but the actual filesystem
    /// operation (creating parent directories, or writing the file itself)
    /// failed.
    #[error("artifact write io error: {detail}")]
    Io { detail: String },
}

/// Errors produced by a `SessionStore` implementation.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum StoreError {
    #[error("session {session} not found")]
    NotFound { session: SessionId },
    #[error("session {session} corrupt at line {line}: {detail}")]
    Corrupt {
        session: SessionId,
        line: u64,
        detail: String,
    },
    #[error("store io error: {detail}")]
    Io { detail: String },
    #[error("sequence {requested} out of range (head is {head})")]
    SeqOutOfRange { requested: LogSeq, head: LogSeq },
    #[error("session {session} already exists")]
    AlreadyExists { session: SessionId },
    #[error("session {session} cannot be removed: {reason}")]
    NotRemovable { session: SessionId, reason: String },
    #[error("session {session} cannot be promoted: {reason}")]
    NotPromotable { session: SessionId, reason: String },
}

/// Errors produced by the `Router`.
/// Renders `RoutingError::NoCandidate`'s `considered` list for `Display`:
/// empty when there was nothing to consider (e.g. the `RoleAlias` had no
/// chain at all), otherwise `": <model>: <reason>, <model>: <reason>, ..."`
/// so the actual per-candidate rejection cause -- not just the count -- is
/// visible wherever the error is printed.
fn render_considered(considered: &[(ModelRef, String)]) -> String {
    if considered.is_empty() {
        return String::new();
    }
    let rendered = considered
        .iter()
        .map(|(model, reason)| format!("{model}: {reason}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(": {rendered}")
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RoutingError {
    /// No candidate in the role's chain was admissible. The `String` in each
    /// pair is the rendered routing reason (a typed `RoutingReason` cannot be
    /// used here without a module cycle; keep the rendered form) --
    /// capability-skip ("missing: ...") or the backend/health failure a live
    /// attempt hit (e.g. a `BackendError`'s own `Display`, which for
    /// `ServerError` already carries the HTTP status and provider error
    /// body). `render_considered` is what makes those reasons
    /// visible instead of just the bare count: every wrapping layer's
    /// `Display` (`RuntimeError::Routing`, both `ConwayError::Routing`/
    /// `::Runtime`) forwards this variant's `Display` verbatim, so the
    /// per-candidate detail reaches every surfacing path (CLI top-level
    /// error print, `Event::Error` render) for free.
    #[error(
        "no candidate for role {role} ({} considered){}",
        considered.len(),
        render_considered(considered)
    )]
    NoCandidate {
        role: RoleAlias,
        considered: Vec<(ModelRef, String)>,
    },
    #[error("unknown role alias: {role}")]
    UnknownRole { role: RoleAlias },
    #[error("unknown model reference: {reference}")]
    UnknownModelRef { reference: String },
    /// T-1: the assembled context plus reserved headroom exceeds the model's
    /// window. Terminal — no truncation or escalation is performed.
    #[error("context rejected: {est_tokens} prompt + {headroom_tokens} reserved output = {required_tokens} tokens, but {model} accepts at most {max_context_tokens} (short by {shortfall_tokens}); no truncation or escalation is performed")]
    ContextTooLarge {
        role: RoleAlias,
        model: ModelRef,
        /// Assembled prompt estimate.
        est_tokens: u32,
        /// Reserved output/reasoning budget.
        headroom_tokens: u32,
        /// `est_tokens + headroom_tokens`, saturating.
        required_tokens: u32,
        max_context_tokens: u32,
        /// `required_tokens - max_context_tokens`, saturating.
        shortfall_tokens: u32,
    },
}

/// Errors produced by the runtime.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum RuntimeError {
    #[error("agent {agent} not found")]
    AgentNotFound { agent: AgentId },
    /// The agent exists (somewhere in this runtime) but is not a descendant
    /// of the session/handle the caller is acting through -- distinct from
    /// [`RuntimeError::AgentNotFound`], which means the agent is unknown
    /// entirely. Added for `conway::SessionHandle::
    /// ensure_agent_in_session` previously had no dedicated variant
    /// for this case and reused `AgentNotFound` for both.
    #[error("agent {agent} does not belong to session {session}")]
    AgentNotInSession { agent: AgentId, session: SessionId },
    /// The agent exists in the live tree but has already reached a terminal
    /// status (`Finished`/`Failed`/`Cancelled`) — it will never run another
    /// turn, so an operation whose whole effect depends on a future turn
    /// (B4's `Conway::pull_in`, which merges records into the parent agent's
    /// log for its NEXT turn to read) is refused rather than silently
    /// writing records nothing will ever consume. Distinct from
    /// [`RuntimeError::AgentNotFound`] (unknown to this runtime entirely):
    /// the agent is present, just done.
    #[error("agent {agent} is not live (it has reached a terminal status)")]
    AgentNotLive { agent: AgentId },
    #[error("agent {agent} exceeded its budget")]
    BudgetExceeded { agent: AgentId },
    #[error("agent {agent} cancelled: {reason}")]
    Cancelled { agent: AgentId, reason: String },
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("routing error: {0}")]
    Routing(#[from] RoutingError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    /// T-1 at the fork boundary. Terminal — no truncation or escalation.
    #[error("context rejected: {est_tokens} prompt + {headroom_tokens} reserved output = {required_tokens} tokens, but {model} accepts at most {max_context_tokens} (short by {shortfall_tokens}); no truncation or escalation is performed")]
    ForkContextOverflow {
        parent: AgentId,
        model: ModelRef,
        est_tokens: u32,
        headroom_tokens: u32,
        required_tokens: u32,
        max_context_tokens: u32,
        shortfall_tokens: u32,
    },
    /// `SubagentHost::ask` is fork-only — enforced HERE, at the trait
    /// boundary, not only at the `conway_ask` tool callsite (a
    /// `debug_assert!` alone compiles to nothing in release builds and
    /// leaves the invariant unenforced for any caller other than that one
    /// tool; see `conway-runtime`'s `subagent.rs` `ask` impl). A malformed
    /// `SubagentSpec::mode` reaching `ask` is a typed error, never a
    /// panic.
    #[error("ask requires SubagentMode::Fork (ask is fork+await-text, not a third primitive); got {mode:?}")]
    AskRequiresFork { mode: SubagentMode },
    ///, extended by
    ///: `steer`/`await_result`/`cancel`/`start`/
    /// `ask` may act only on an agent within the CALLER's own subtree (itself,
    /// or any descendant) -- enforced HERE, at the `SubagentHost` trait
    /// boundary (see that trait's own doc), not only at the
    /// `conway_steer`/`conway_await`/`conway_cancel`/`conway_fork`/
    /// `conway_spawn`/`conway_ask` tool callsites, so no other caller can
    /// bypass it (mirrors `AskRequiresFork`'s shape). A sibling (or any
    /// non-ancestor, non-self) `AgentId` a caller merely SAW -- in tool output,
    /// on the event stream, in `conway_fork`/`conway_spawn`'s own return value,
    /// or via `tree()` (which, as of a later change, only ever shows
    /// the caller's own subtree in the first place) -- is not enough to act on
    /// it. `target` (named `parent` on `start`/`ask`) is known to this runtime
    /// (an unknown one is [`RuntimeError::AgentNotFound`] instead); `caller` is
    /// who attempted the operation. Untrusted ids give a typed error, never a
    /// panic -- both ids may be model-supplied.
    #[error("agent {caller} may not act on agent {target}: it is outside {caller}'s own subtree")]
    AgentNotInSubtree { caller: AgentId, target: AgentId },
    /// A `SubagentSpec` (or `ResumeSpec`) the caller supplied is internally
    /// invalid or fails a runtime-side consistency check `conway-core`'s own
    /// `SubagentSpec::validate` cannot perform (it does no I/O) -- a
    /// nonexistent `cwd`, a `root` that does not canonicalize, or a `root`/
    /// `cwd` pair violating the containment algebra (`conway-runtime`'s
    /// `subagent.rs` `invalid_spec` helper and `runtime.rs`'s `resume_root`
    /// construct this; see that helper's own doc for the full list of call
    /// sites). Distinct from every other variant here: this is a rejection
    /// of the CALLER'S OWN supplied data, not an unknown/out-of-subtree
    /// agent id or an infrastructure failure -- `conway_core::ports::
    /// subagent::translate` maps it to `SubagentError::InvalidSpec`, which
    /// `conway-tools` in turn maps to `ToolError::InvalidArguments` (a
    /// model-correctable mistake), not `Internal`.
    #[error("invalid subagent spec: {detail}")]
    InvalidSpec { detail: String },
    /// A `prompt_submitted` hook refused the prompt before it reached the
    /// agent loop.
    ///
    /// **Surfaced to the CALLER of `start_root`/`prompt`, never to a model as
    /// a tool error** -- there is no model turn yet to report into, which is
    /// what distinguishes this from `pre_tool_use`'s denial (that one lands in
    /// a tool result the model reads).
    ///
    /// `reason` is the hook's own explanation, or the fail-closed message when
    /// a hook errored or timed out. It is a diagnosis for a human, and is
    /// never substituted for the prompt -- nothing may rewrite what the user
    /// typed.
    #[error("prompt denied by hook: {reason}")]
    PromptDenied { reason: String },
}

/// Errors produced by [`crate::ports::SubagentHandle`]'s five fallible
/// methods (`start`, `steer`, `await_result`, `cancel`, `ask`) -- the narrow,
/// tool-facing counterpart to [`RuntimeError`], which is what
/// [`crate::ports::SubagentHost`] (the port `SubagentHandle` wraps) actually
/// returns. `SubagentHandle` translates every `RuntimeError` a call to the
/// wrapped host can produce into one of these four variants at its own
/// boundary (see that type's own doc for the translation) -- nothing
/// downstream of the handle ever sees a raw `RuntimeError` again. Mirrors
/// [`CwdError`]'s house style: `#[non_exhaustive]`, serde round-trippable,
/// `Display` via `thiserror`.
///
/// **Four variants are caller mistakes a model can correct**: an unknown or
/// out-of-subtree `agent_id` it supplied, a malformed `SubagentMode`
/// reaching `ask`, or a `SubagentSpec` that fails a runtime-side validity
/// check. `From<SubagentError> for ToolError` (below) maps all four to
/// `ToolError::InvalidArguments`, distinct from [`Self::Host`] (genuine
/// infrastructure, `ToolError::Internal`) -- see that `impl`'s own doc for
/// why the split matters: `conway-tools`' pre-existing `host_error` helper
/// flattened every `RuntimeError`, including these four, to `Internal`,
/// which read as a host bug for what is, in each of these four cases, a
/// mistake in the model's own tool call.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum SubagentError {
    /// [`RuntimeError::AgentNotFound`]: `agent` is unknown to this runtime
    /// entirely.
    #[error("unknown agent: {agent}")]
    UnknownAgent { agent: AgentId },
    /// [`RuntimeError::AgentNotInSubtree`]: `target` exists but is outside
    /// `caller`'s own subtree. `caller` here is always the handle's
    /// own baked-in agent id -- see [`crate::ports::SubagentHandle`]'s own
    /// doc for why a tool has no way to supply a different one.
    #[error("agent {caller} may not act on agent {target}: it is outside {caller}'s own subtree")]
    NotInSubtree { caller: AgentId, target: AgentId },
    /// [`RuntimeError::AskRequiresFork`]: `ask` is fork+await-text,
    /// not a third primitive; a malformed `SubagentSpec::mode` reaching it
    /// is a typed error here, never a panic.
    #[error("ask requires SubagentMode::Fork (ask is fork+await-text, not a third primitive); got {mode:?}")]
    AskRequiresFork { mode: SubagentMode },
    /// [`RuntimeError::InvalidSpec`]: the caller's own `SubagentSpec` (or
    /// `ResumeSpec`) failed a runtime-side validity check -- a nonexistent
    /// `cwd`, a `root` that does not canonicalize, or a `root`/`cwd` pair
    /// violating the containment algebra. A model-correctable mistake, like
    /// the three variants above, so it maps to `ToolError::InvalidArguments`
    /// below rather than `Host`'s `Internal`.
    #[error("invalid subagent spec: {detail}")]
    InvalidSpec { detail: String },
    /// Infrastructure: every other `RuntimeError` the wrapped
    /// `SubagentHost` can return (`RuntimeError::Store`, the runtime having
    /// already been dropped, ...) -- not a model-correctable mistake.
    #[error("subagent host error: {detail}")]
    Host { detail: String },
}

/// The ONE place [`SubagentError`] becomes a [`ToolError`] -- one
/// implementation, never restated: every `conway-tools` subagent tool maps
/// through this, never restating the mapping per call site.
/// `UnknownAgent`/`NotInSubtree`/`AskRequiresFork`/ `InvalidSpec` are caller
/// mistakes a model can correct, so they become `ToolError::InvalidArguments`
/// -- not `Internal`, which is how `conway-tools`' pre-existing `host_error`
/// helper flattened every `RuntimeError` (see [`SubagentError`]'s own doc).
/// [`SubagentError::Host`]
/// -- genuine infrastructure -- maps to `ToolError::Internal`, unchanged.
impl From<SubagentError> for ToolError {
    fn from(err: SubagentError) -> Self {
        match err {
            SubagentError::UnknownAgent { .. }
            | SubagentError::NotInSubtree { .. }
            | SubagentError::AskRequiresFork { .. }
            | SubagentError::InvalidSpec { .. } => ToolError::InvalidArguments {
                detail: err.to_string(),
            },
            SubagentError::Host { detail } => ToolError::Internal { detail },
        }
    }
}

/// Errors produced by plugin registration and initialization.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum PluginError {
    #[error("plugin {plugin} failed to initialize: {detail}")]
    Init { plugin: String, detail: String },
    #[error("plugin {plugin} requires missing host capability {capability}")]
    MissingHostCapability { plugin: String, capability: String },
    #[error("duplicate tool name: {tool}")]
    DuplicateTool { tool: ToolName },
}

/// The crate-level umbrella error.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum ConwayError {
    #[error("backend error: {0}")]
    Backend(#[from] BackendError),
    #[error("tool error: {0}")]
    Tool(#[from] ToolError),
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("routing error: {0}")]
    Routing(#[from] RoutingError),
    #[error("runtime error: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("plugin error: {0}")]
    Plugin(#[from] PluginError),
    #[error("configuration error: {detail}")]
    Config { detail: String },
    #[error("parse error: {detail}")]
    Parse { detail: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{BackendId, ModelId};

    fn model_ref() -> ModelRef {
        ModelRef {
            backend: BackendId::new("local"),
            model: ModelId::new("qwen3-coder:30b"),
        }
    }

    #[test]
    fn context_too_large_exists_and_roundtrips() {
        let err = RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: model_ref(),
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 1_232,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: RoutingError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn fork_context_overflow_exists_and_roundtrips() {
        let err = RuntimeError::ForkContextOverflow {
            parent: AgentId::new(),
            model: model_ref(),
            est_tokens: 100_000,
            headroom_tokens: 16_000,
            required_tokens: 116_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 83_232,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: RuntimeError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn t1_display_names_all_four_numbers() {
        let routing = RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: model_ref(),
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 1_232,
        }
        .to_string();
        for needle in [
            "30000",
            "4000",
            "34000",
            "32768",
            "1232",
            "no truncation or escalation",
        ] {
            assert!(
                routing.contains(needle),
                "missing {needle:?} in {routing:?}"
            );
        }

        let runtime = RuntimeError::ForkContextOverflow {
            parent: AgentId::new(),
            model: model_ref(),
            est_tokens: 100_000,
            headroom_tokens: 16_000,
            required_tokens: 116_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 83_232,
        }
        .to_string();
        for needle in ["100000", "16000", "116000", "32768", "83232"] {
            assert!(
                runtime.contains(needle),
                "missing {needle:?} in {runtime:?}"
            );
        }
    }

    /// `conway-cli`'s exit-code classifier (`conway-cli/src/exit.rs`'s
    /// `classify_runtime_or_routing`, which maps a routing rejection to
    /// process exit code 4) cannot name these types -- the `conway` facade
    /// does not re-export them -- so it matches these exact `Display`
    /// substrings instead. This test is the pin that makes a wording change
    /// here fail HERE, loudly, rather than silently reclassifying a live
    /// exit code there. `no_candidate_display_names_role_count_and_zero_reasons`
    /// above pins the third needle (`"no candidate for role"`) by exact
    /// equality.
    #[test]
    fn routing_rejection_display_wordings_pin_the_cli_exit_classifier() {
        let unknown_role = RoutingError::UnknownRole {
            role: RoleAlias::new("doesnotexist"),
        }
        .to_string();
        assert!(
            unknown_role.contains("unknown role alias"),
            "conway-cli's exit-4 classifier matches this wording: {unknown_role:?}"
        );

        let too_large = RoutingError::ContextTooLarge {
            role: RoleAlias::new("planner"),
            model: model_ref(),
            est_tokens: 30_000,
            headroom_tokens: 4_000,
            required_tokens: 34_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 1_232,
        }
        .to_string();
        assert!(
            too_large.contains("context rejected:"),
            "conway-cli's exit-4 classifier matches this wording: {too_large:?}"
        );

        // Shares `ContextTooLarge`'s prefix deliberately (T-1 at the fork
        // boundary is the same rejection), so the CLI classifier covers it
        // with the same needle.
        let fork_overflow = RuntimeError::ForkContextOverflow {
            parent: AgentId::new(),
            model: model_ref(),
            est_tokens: 100_000,
            headroom_tokens: 16_000,
            required_tokens: 116_000,
            max_context_tokens: 32_768,
            shortfall_tokens: 83_232,
        }
        .to_string();
        assert!(
            fork_overflow.contains("context rejected:"),
            "conway-cli's exit-4 classifier matches this wording: {fork_overflow:?}"
        );
    }

    #[test]
    fn no_candidate_display_names_role_count_and_zero_reasons() {
        let err = RoutingError::NoCandidate {
            role: RoleAlias::new("coder"),
            considered: Vec::new(),
        };
        assert_eq!(
            err.to_string(),
            "no candidate for role coder (0 considered)"
        );
    }

    #[test]
    fn no_candidate_display_renders_per_candidate_reasons() {
        let other = ModelRef {
            backend: BackendId::new("openai"),
            model: ModelId::new("gpt-5"),
        };
        let err = RoutingError::NoCandidate {
            role: RoleAlias::new("coder"),
            considered: vec![
                (
                    model_ref(),
                    "server error (status 503): upstream unavailable".to_string(),
                ),
                (
                    other,
                    "rate limited (retry after Some(30) seconds)".to_string(),
                ),
            ],
        };
        let rendered = err.to_string();
        assert!(rendered.starts_with("no candidate for role coder (2 considered): "));
        assert!(rendered
            .contains("local/qwen3-coder:30b: server error (status 503): upstream unavailable"));
        assert!(rendered.contains("openai/gpt-5: rate limited (retry after Some(30) seconds)"));
    }

    /// [`BackendError::ContextTooLarge`]:
    /// roundtrips, and its `Display` names every one of the four inputs
    /// plus the derived shortfall -- the acceptance criterion's "typed
    /// error naming the input size, the resolved headroom, and the window"
    /// is this variant.
    #[test]
    fn context_too_large_exists_roundtrips_and_names_all_numbers() {
        let err = BackendError::ContextTooLarge {
            model: ModelId::new("ollama-cloud/glm-5.2"),
            est_tokens: 34_000,
            headroom_tokens: 16_000,
            required_tokens: 50_000,
            max_context_tokens: 40_000,
            shortfall_tokens: 10_000,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: BackendError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);

        let rendered = err.to_string();
        for needle in ["34000", "16000", "50000", "40000", "10000", "glm-5.2"] {
            assert!(
                rendered.contains(needle),
                "missing {needle:?} in {rendered:?}"
            );
        }
    }

    // ---- HookFailure ----

    #[test]
    fn hook_failure_variants_roundtrip_and_render() {
        let cases: Vec<(HookFailure, &str)> = vec![
            (HookFailure::NonzeroExit { code: Some(3) }, "3"),
            (HookFailure::NonzeroExit { code: None }, "None"),
            (HookFailure::TimedOut { after_ms: 5_000 }, "5000"),
            (
                HookFailure::Spawn {
                    detail: "No such file or directory".into(),
                },
                "No such file or directory",
            ),
            (
                HookFailure::UnparseableAnswer {
                    detail: "expected value".into(),
                },
                "expected value",
            ),
        ];
        for (err, needle) in cases {
            let json = serde_json::to_string(&err).unwrap();
            let back: HookFailure = serde_json::from_str(&json).unwrap();
            assert_eq!(err, back);
            assert!(
                err.to_string().contains(needle),
                "missing {needle:?} in {}",
                err
            );
        }
    }

    #[test]
    fn cwd_error_poisoned_exists_and_roundtrips() {
        let err = CwdError::Poisoned;
        let json = serde_json::to_string(&err).unwrap();
        let back: CwdError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        assert!(err.to_string().contains("poisoned"));
    }

    #[test]
    fn agent_not_in_session_exists_and_roundtrips() {
        let err = RuntimeError::AgentNotInSession {
            agent: AgentId::new(),
            session: SessionId::new(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: RuntimeError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn agent_not_in_subtree_exists_roundtrips_and_names_both_ids() {
        let caller = AgentId::new();
        let target = AgentId::new();
        let err = RuntimeError::AgentNotInSubtree { caller, target };
        let json = serde_json::to_string(&err).unwrap();
        let back: RuntimeError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);

        let rendered = err.to_string();
        assert!(rendered.contains(&caller.to_string()));
        assert!(rendered.contains(&target.to_string()));
    }

    // ---- SubagentError (C1: the SubagentHandle/ToolCtx capability) ----

    #[test]
    fn subagent_error_unknown_agent_roundtrips_and_names_the_agent() {
        let agent = AgentId::new();
        let err = SubagentError::UnknownAgent { agent };
        let json = serde_json::to_string(&err).unwrap();
        let back: SubagentError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        assert!(err.to_string().contains(&agent.to_string()));
    }

    #[test]
    fn subagent_error_not_in_subtree_roundtrips_and_names_both_ids() {
        let caller = AgentId::new();
        let target = AgentId::new();
        let err = SubagentError::NotInSubtree { caller, target };
        let json = serde_json::to_string(&err).unwrap();
        let back: SubagentError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        let rendered = err.to_string();
        assert!(rendered.contains(&caller.to_string()));
        assert!(rendered.contains(&target.to_string()));
    }

    #[test]
    fn subagent_error_ask_requires_fork_roundtrips_and_names_the_mode() {
        let err = SubagentError::AskRequiresFork {
            mode: SubagentMode::Spawn,
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: SubagentError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        assert!(err.to_string().contains("Spawn"));
    }

    #[test]
    fn subagent_error_host_roundtrips_and_names_the_detail() {
        let err = SubagentError::Host {
            detail: "the runtime has already been dropped".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: SubagentError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
        assert!(err
            .to_string()
            .contains("the runtime has already been dropped"));
    }

    /// The From<SubagentError> for ToolError mapping (the ONE place
    /// this happens) for the three caller-mistake variants:
    /// `UnknownAgent`/`NotInSubtree`/`AskRequiresFork` must all become
    /// `ToolError::InvalidArguments`, carrying the variant's own rendered
    /// `Display` as `detail` -- not `Internal`, which is how `conway-tools`'
    /// pre-existing `host_error` helper flattened every `RuntimeError`
    /// (see `SubagentError`'s own doc). Checking the carried `detail`
    /// against the source `Display`, not just the outer variant, is what
    /// makes this able to fail: a stub that maps to the right variant but
    /// drops/replaces the message would still be caught.
    #[test]
    fn unknown_agent_not_in_subtree_and_ask_requires_fork_map_to_invalid_arguments() {
        let agent = AgentId::new();
        let caller = AgentId::new();
        let target = AgentId::new();
        let mode = SubagentMode::Spawn;

        for subagent_err in [
            SubagentError::UnknownAgent { agent },
            SubagentError::NotInSubtree { caller, target },
            SubagentError::AskRequiresFork { mode },
        ] {
            let rendered = subagent_err.to_string();
            let tool_err: ToolError = subagent_err.into();
            let ToolError::InvalidArguments { detail } = tool_err else {
                panic!("expected InvalidArguments, got {tool_err:?}");
            };
            assert_eq!(detail, rendered);
        }
    }

    /// The other half of the same mapping: `SubagentError::Host`
    /// (infrastructure, not a caller mistake) must become
    /// `ToolError::Internal`, carrying its `detail` field through
    /// unrendered (no `Display` re-wrapping -- `Host { detail } => Internal
    /// { detail }` is a direct pass-through, unlike the three variants
    /// above).
    #[test]
    fn host_maps_to_internal_with_its_detail_passed_through() {
        let err = SubagentError::Host {
            detail: "store io error".into(),
        };
        let tool_err: ToolError = err.into();
        let ToolError::Internal { detail } = tool_err else {
            panic!("expected Internal, got {tool_err:?}");
        };
        assert_eq!(detail, "store io error");
    }

    #[test]
    fn backend_error_classification() {
        let cases: Vec<(BackendError, bool, bool)> = vec![
            (BackendError::Transport { detail: "x".into() }, true, true),
            (
                BackendError::RateLimit {
                    retry_after_secs: Some(7),
                },
                true,
                true,
            ),
            (
                BackendError::ServerError {
                    status: 503,
                    detail: "x".into(),
                },
                true,
                true,
            ),
            (
                BackendError::ContextOverflow {
                    required_tokens: 2,
                    max_context_tokens: 1,
                },
                true,
                false,
            ),
            (
                BackendError::ContextTooLarge {
                    model: ModelId::new("m"),
                    est_tokens: 34_000,
                    headroom_tokens: 16_000,
                    required_tokens: 50_000,
                    max_context_tokens: 40_000,
                    shortfall_tokens: 10_000,
                },
                true,
                false,
            ),
            (BackendError::Auth { detail: "x".into() }, false, false),
            (
                BackendError::BadRequest { detail: "x".into() },
                false,
                false,
            ),
            (BackendError::ToolParse { detail: "x".into() }, false, false),
            (BackendError::Cancelled, false, false),
        ];
        for (err, failover, health) in cases {
            assert_eq!(err.is_failover_worthy(), failover, "failover for {err:?}");
            assert_eq!(err.is_health_signal(), health, "health for {err:?}");
        }
    }

    #[test]
    fn umbrella_conversions() {
        let e: ConwayError = BackendError::Cancelled.into();
        assert!(matches!(e, ConwayError::Backend(_)));
        let e: ConwayError = RuntimeError::AgentNotFound {
            agent: AgentId::new(),
        }
        .into();
        assert!(matches!(e, ConwayError::Runtime(_)));
        let round: ConwayError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(e, round);
    }
}

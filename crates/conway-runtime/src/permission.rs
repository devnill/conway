//! `PermissionBroker`: a per-session decision cache layered over the
//! consumer's [`PermissionGate`] (architecture §4.3).
//!
//! The broker normalizes whatever the gate decides into a
//! [`PermissionOutcome`] the tool runner (WI-079) can act on directly, and
//! it owns the `AllowAlways` cache so a consumer answering "allow always"
//! is only ever asked once per scope. It never imposes a timeout on the
//! gate: architecture §8 requires the runtime to hold a pending call open
//! for as long as the gate takes to answer.

use std::collections::HashMap;

use conway_core::permission_mode::PermissionMode;
use conway_core::permission_pattern::{PatternOrigin, PatternRule, Rule, Then, When};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use conway_core::agent::{
    PermissionDecision, PermissionDecisionKind, PermissionRequest, PermissionScope,
};
use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::content::ToolCategory;
use conway_core::event::Event;
use conway_core::hook::{HookEvent, HookInvocation, HookPermissionVerdict};
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{HookRunner, PathArgs, PermissionGate, RenderKind};

use crate::context::prefix::canonical_json_bytes;
use crate::events::EventBus;

/// The requesting agent's identity and position in the tree, as seen by one
/// [`PermissionBroker::decide`] call.
///
/// `agent_path` is the full root→requester chain (§8 precondition on
/// [`PermissionRequest`]) — it is what makes an `AgentSubtree` grant
/// checkable by prefix membership without walking the live agent tree.
pub struct PermissionCtx {
    pub agent_id: AgentId,
    pub agent_path: Vec<AgentId>,
    pub session: SessionId,
    pub cwd: PathBuf,
    /// S5: this agent's confinement root, reconstructed once per agent (see
    /// [`AgentRoot::reconstruct`]) and cloned in unchanged for every call in
    /// every batch. `AgentRoot::Unconfined` — every existing caller before
    /// this field existed — makes [`PermissionBroker::decide`]'s root check
    /// a byte-for-byte no-op.
    pub root: AgentRoot,
}

/// The minimal, already-resolved slice of a proposed tool call the broker
/// needs to authorize it. `category` and `rendered` are supplied by the
/// caller (the tool runner, from the resolved `ToolSpec` and the tool's own
/// renderer) — the broker does not know how to categorize or render a tool
/// call itself.
#[derive(Clone, Debug)]
pub struct AuthorizedCall {
    pub call_id: String,
    pub tool: ToolName,
    pub category: ToolCategory,
    pub arguments: serde_json::Value,
    pub rendered: String,
    /// S5: the resolved tool's own [`Tool::path_args`](conway_core::ports::Tool::path_args)
    /// declaration, read straight from the resolved tool instance at the
    /// same call site that already produces `rendered` (`ToolRunner::
    /// execute_one`) — a plain, static, `'static`-lifetime enum copy, no
    /// I/O and no re-resolution of the tool by name. This is how the
    /// broker's decision point (which has no `PluginRegistry` access, and
    /// must not gain one just for this) learns which of `arguments`' fields
    /// carry filesystem paths without duplicating tool resolution.
    pub path_args: PathArgs,
    /// Board item 01KYT3NSWRHMPEAXVXRJ73KDYR: the resolved tool's own
    /// [`Tool::render_kind`](conway_core::ports::Tool::render_kind)
    /// declaration, read at the identical call site and for the identical
    /// reason as `path_args` above -- fed to [`PatternRule::matches_render`]
    /// so the metacharacter gate applies only when `rendered` could
    /// actually reach a shell.
    pub render_kind: RenderKind,
}

/// One `pre_tool_use` hook [`PermissionBroker::decide`] consults (board item
/// 01KZS00JP5QNBJSSHNFP9C47GM). Installed via
/// [`PermissionBroker::set_pre_tool_use_hooks`], translated by the facade
/// from `[hooks].rules[]` entries whose `event == "pre_tool_use"` and
/// `enabled` is `true` -- this crate has no dependency on `conway`'s config
/// schema and so knows nothing of `HookEntry` itself (`no_forbidden_deps`);
/// this is the narrow shape `decide()` actually needs, the same relationship
/// [`AuthorizedCall`] already has to a full `ToolSpec`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreToolUseHookSpec {
    /// The rule's operator-chosen identity (`HookEntry::id`), folded into a
    /// denial's rendered message so an operator sees WHICH hook refused the
    /// call, not merely that one did -- mirroring `deny_matches`' own
    /// `rule.describe()` in its rendered error.
    pub id: String,
    pub command: Vec<String>,
    pub timeout_ms: u64,
}

/// This agent's confinement root (S3's `SessionMeta.root`/`SubagentSpec.
/// root`), as seen by the permission broker's root-containment check (S5).
///
/// Reconstructed exactly ONCE per agent — by
/// [`AgentRoot::reconstruct`], called once in `AgentLoop::run_inner`
/// (mirroring the `CwdHandle` cell built alongside it) — and cloned
/// unchanged into every turn's `ToolBatchCtx`/`PermissionCtx` after that.
/// Cloning is cheap (a `PathBuf` clone at most): the one filesystem
/// `canonicalize` call this type ever performs happens inside
/// `reconstruct`, never inside `PermissionBroker::decide` and never per
/// path argument.
#[derive(Clone, Debug)]
pub enum AgentRoot {
    /// This agent has no confinement root. The root check is a no-op:
    /// every call proceeds exactly as it did before this slice existed.
    Unconfined,
    /// This agent's root, already canonicalized.
    Confined(CanonicalRoot),
    /// This agent HAS a persisted root (`Some(path)`), but `path` no longer
    /// canonicalizes (e.g. its directory was removed, or became
    /// unreadable, after the agent was spawned). FAILS CLOSED: every
    /// root-relevant call this agent makes for the rest of its run is
    /// denied — this is never silently downgraded to `Unconfined`, which
    /// would treat "can't tell where the root is" as "no root at all" and
    /// unconfine the agent by accident.
    Broken,
}

impl AgentRoot {
    /// Reconstructs an agent's confinement root from the persisted,
    /// already-canonical `PathBuf` carried on `AgentLoop`/`SessionMeta`
    /// (only the raw path survives a store round trip — the `CanonicalRoot`
    /// object itself does not). Call this exactly once per agent's run; see
    /// this type's own doc for why cloning the result afterward is cheap
    /// and safe.
    pub fn reconstruct(persisted: &Option<PathBuf>) -> Self {
        match persisted {
            None => AgentRoot::Unconfined,
            Some(path) => match CanonicalRoot::new(path) {
                Ok(root) => AgentRoot::Confined(root),
                Err(err) => {
                    tracing::error!(
                        root = %path.display(),
                        error = %err,
                        "agent's persisted confinement root no longer canonicalizes; \
                         failing closed -- every root-relevant tool call this agent \
                         makes is denied until the session's root is valid again"
                    );
                    AgentRoot::Broken
                }
            },
        }
    }
}

/// [`PermissionBroker::check_root`]'s result: whether the call is denied
/// outright, must skip straight to the operator's gate, or is unaffected
/// (proceeds through the ordinary allow paths exactly as before this
/// slice).
#[derive(Clone, Debug, PartialEq, Eq)]
enum RootDecision {
    /// No confinement applies, or every declared path argument checked out
    /// -- proceed through the ordinary allow paths unchanged.
    Proceed,
    /// At least part of this call cannot be statically confined
    /// (`PathArgs::Unconfinable`) and a root IS in effect: this call must
    /// always reach `gate.check`, bypassing the cache/pattern grants/
    /// `AutoAllow`. Not a denial.
    MustReachGate,
    /// A declared path argument resolved outside the root (or couldn't be
    /// resolved/parsed at all) -- denied before any other allow path is
    /// consulted.
    Denied(String),
}

/// Resolves a tool-supplied path argument exactly the way the call will
/// actually be resolved once it reaches the tool: relative inputs join onto
/// `cwd`, absolute inputs pass through unchanged. Returns `None` for a path
/// containing a NUL byte (the OS path APIs cannot represent it, so the tool
/// itself would fail to resolve it too).
///
/// **DUPLICATED, DELIBERATELY.** This mirrors `conway_tools::common::
/// resolve_path` byte-for-byte, but cannot call it: crate layering runs
/// `conway-tools -> conway-core` only, and `conway-runtime` (this crate)
/// must not gain a dependency on `conway-tools` just for this. If
/// `resolve_path`'s resolution rule ever changes, THIS copy must change
/// with it in the same commit, or a path could resolve one way at
/// permission-check time and a different way when the tool actually runs it
/// -- exactly the kind of bypass this slice exists to prevent. (Precedent:
/// `conway_core::permission_pattern` and `conway_core::text` share the
/// replace-semantics sanitizer so the gate and the runtime's `rendered`
/// seam cannot drift; the path-resolution rule below is duplicated for the
/// same reason -- `conway-runtime` cannot depend on `conway-tools`.)
///
/// `pub(crate)` so the crate's OTHER path-resolution consumers -- the
/// spawn-time confinement-root resolution in `subagent.rs` and
/// `runtime.rs` -- call THIS one rule (Min-1 / P-14, board item
/// 01KZ00VV3F3EBZ9WQSB292TBJZ) instead of inlining "absolute -> as-is,
/// relative -> join cwd" and silently dropping the NUL guard, as both did
/// until that item. Within `conway-runtime` this is the single resolution
/// rule; the `conway-tools` copy is the deliberate cross-crate mirror the
/// paragraph above obligates to change in lockstep.
pub(crate) fn resolve_like_the_tool_will(cwd: &Path, raw: &str) -> Option<PathBuf> {
    if raw.contains('\0') {
        return None;
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        Some(candidate.to_path_buf())
    } else {
        Some(cwd.join(candidate))
    }
}

/// Canonicalizes a [`When::PathsUnder`] prefix once, at install, so the
/// per-call `paths_under` check is a pure containment comparison (no
/// filesystem I/O on the decision hot path). Returns `None` for non-`PathsUnder`
/// clauses (no canonical root to carry) and for a `PathsUnder` prefix that
/// does not canonicalize -- the caller drops the latter (fail closed: a
/// boundary that cannot be established cannot be trusted to confine).
///
/// B2: a RELATIVE prefix resolves against `base` -- via the SAME
/// [`resolve_like_the_tool_will`] helper the per-call check resolves path
/// arguments with (P-14: one resolution rule, not a third copy) -- never
/// against the process's cwd, which is what a bare `Path::canonicalize`
/// would use. The process cwd is wherever the operator happened to launch
/// conway from and has no relationship to the project the rule was written
/// to protect, so a relative prefix resolved there confers a boundary the
/// operator did not write (finding S5). `base` is an explicit parameter
/// supplied by the caller (the facade's permission-file loader passes the
/// PROJECT root for a project file, the agent cwd for the global file); the
/// broker deliberately never reads `std::env::current_dir()` here, which
/// would recreate the bug one level down. An ABSOLUTE prefix is unaffected
/// by `base` (it passes through `resolve_like_the_tool_will` unchanged),
/// and a prefix containing a NUL byte fails closed exactly like one that
/// does not canonicalize.
fn canonicalize_when(when: &When, base: &Path) -> Option<CanonicalRoot> {
    match when {
        When::PathsUnder(prefix) => {
            let resolved = resolve_like_the_tool_will(base, prefix)?;
            CanonicalRoot::new(&resolved).ok()
        }
        _ => None,
    }
}

/// The `paths_under` predicate: every one of the call's DECLARED path
/// arguments (per `Tool::path_args`, resolved exactly as
/// [`PermissionBroker::check_root`] resolves them -- via
/// [`resolve_like_the_tool_will`], from `call.arguments`, NEVER from the
/// sanitized/lossy `call.rendered`) must be contained under `root`. An
/// [`PathArgs::Unconfinable`] tool NEVER satisfies this (fail closed, the
/// same asymmetry root confinement uses -- a tool the broker cannot
/// statically confine cannot be auto-allowed by a path-scoped rule); a
/// tool with [`PathArgs::None`] never satisfies it either (no paths to
/// confine). A missing/null argument is skipped (the call simply does not
/// use it), matching `check_root`'s "absent is not an error" rule.
fn paths_under_match(ctx: &PermissionCtx, call: &AuthorizedCall, root: &CanonicalRoot) -> bool {
    let names: &[&str] = match call.path_args {
        PathArgs::Named(names) => names,
        // Unconfinable -- and `#[non_exhaustive]` fallback -- never satisfy
        // paths_under. A `checkable` cwd does NOT change this: the part of
        // the call the broker cannot statically confine (e.g. `bash`'s
        // `command`) can still reach outside `root`, so a `paths_under` rule
        // must not auto-allow it. This is the SAME asymmetry `check_root`
        // uses (`Unconfinable` always forces the gate under a root).
        PathArgs::Unconfinable { .. } => return false,
        PathArgs::None => return false,
        _ => return false,
    };
    for name in names {
        match call.arguments.get(*name) {
            None | Some(serde_json::Value::Null) => continue,
            Some(serde_json::Value::String(raw)) => {
                let Some(resolved) = resolve_like_the_tool_will(&ctx.cwd, raw) else {
                    return false;
                };
                match root.contains(&resolved) {
                    Containment::Inside => {}
                    // `Undecidable` fuses with `Outside`: "can't check" is
                    // never "allow" (same posture as `check_root`).
                    Containment::Outside | Containment::Undecidable => return false,
                }
            }
            // A declared path argument present with a non-string, non-null
            // value is suspicious, not merely unparseable: deny, fail closed
            // (same posture as `check_root`).
            Some(_) => return false,
        }
    }
    true
}

/// Whether an ALLOW [`Rule`] authorizes `(ctx, call)` -- the single allow
/// evaluator the flat and structured forms share. Render-based `when`
/// clauses (`Always`, `CommandPrefix`, `CategoryIn`) delegate to
/// [`Rule::matches_allow_render`] (the gate + select + when live there, in
/// `conway-core`, so `PatternRule::matches_render` and this path cannot
/// drift); [`When::PathsUnder`] is evaluated here, where
/// [`resolve_like_the_tool_will`] and [`CanonicalRoot`] live -- the SAME
/// path `check_root` uses, adding no new trusted code.
fn rule_allows(
    ctx: &PermissionCtx,
    call: &AuthorizedCall,
    rule: &Rule,
    canonical: Option<&CanonicalRoot>,
) -> bool {
    match &rule.when {
        When::PathsUnder(_) => {
            if !rule.select_matches(call.tool.as_str(), call.category) {
                return false;
            }
            // The allow-side gate applies to `PathsUnder` too: a
            // `ShellCommand` tool carrying a metacharacter must not be
            // auto-allowed even when its `checkable` paths are under the
            // rule's prefix (trap 4: the gate must not weaken).
            if !Rule::gate_allows(&call.rendered, call.render_kind) {
                return false;
            }
            let Some(root) = canonical else {
                return false;
            };
            paths_under_match(ctx, call, root)
        }
        _ => rule.matches_allow_render(
            call.tool.as_str(),
            call.category,
            &call.rendered,
            call.render_kind,
        ),
    }
}

/// Whether a DENY (or PROMPT) [`Rule`] matches `(ctx, call)` -- the single
/// deny/prompt evaluator, UNGATED (the deny/prompt asymmetry: a `;` must
/// not defeat a deny/prompt the way it defeats an allow). Render-based
/// `when` clauses delegate to [`Rule::matches_deny_render`]; `PathsUnder`
/// is evaluated here (no gate, per the asymmetry).
fn rule_denies_or_prompts(
    ctx: &PermissionCtx,
    call: &AuthorizedCall,
    rule: &Rule,
    canonical: Option<&CanonicalRoot>,
) -> bool {
    match &rule.when {
        When::PathsUnder(_) => {
            if !rule.select_matches(call.tool.as_str(), call.category) {
                return false;
            }
            let Some(root) = canonical else {
                return false;
            };
            // B1: decision-time fail-closed for the cases the install-time
            // registration check CANNOT see -- a `Select::Categories` (whose
            // member tools may register after the rule is loaded) or a
            // trailing-`*` wildcard `Select::Tools` (no tool is named `*`).
            // For an `Unconfinable` tool (e.g. `bash`) `paths_under_match`
            // returns `false` -- correct for ALLOW (fail-closed: don't
            // auto-allow) but fail-OPEN for deny/prompt: the operator wrote a
            // deny rule expecting the call to be refused, and silently NOT
            // matching it lets the call through. Mirror `check_root`'s
            // `Unconfinable { checkable }` posture -- a tool the broker
            // cannot statically confine can never be PROVEN to be outside the
            // prefix either -- so the deny/prompt rule MATCHES (fail-toward-
            // deny, P-13). The install-time `PathsUnderOnUnconfinedTool`
            // error is the primary fix for the common `Select::Tools` case;
            // this is the fallback for the shapes it cannot inspect.
            match call.path_args {
                PathArgs::Unconfinable { .. } => true,
                _ => paths_under_match(ctx, call, root),
            }
        }
        _ => rule.matches_deny_render(call.tool.as_str(), call.category, &call.rendered),
    }
}

/// The normalized result of [`PermissionBroker::decide`]. There is no
/// "abort" variant: every denial — whether a plain [`PermissionDecision::Deny`]
/// or a [`PermissionDecision::DenyWithFeedback`] — collapses to `Deny`, so a
/// caller matching on this type can only ever turn it into a model-visible
/// tool error, never mistake it for a reason to abort the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PermissionOutcome {
    Allow,
    Deny { rendered_error: String },
}

impl From<PermissionDecision> for PermissionOutcome {
    fn from(decision: PermissionDecision) -> Self {
        match decision {
            PermissionDecision::AllowOnce | PermissionDecision::AllowAlways { .. } => {
                PermissionOutcome::Allow
            }
            PermissionDecision::Deny { reason } => PermissionOutcome::Deny {
                rendered_error: reason,
            },
            PermissionDecision::DenyWithFeedback { message } => PermissionOutcome::Deny {
                rendered_error: message,
            },
            // `PermissionDecision` is `#[non_exhaustive]`: fail closed on any
            // future variant rather than silently allowing it.
            _ => PermissionOutcome::Deny {
                rendered_error: "permission gate returned an unrecognized decision".into(),
            },
        }
    }
}

/// The cache lookup key: a tool name plus a digest of its canonicalized
/// arguments, so semantically identical argument objects (differing only in
/// key order or insignificant whitespace) hit the same entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    tool: ToolName,
    args_digest: blake3::Hash,
}

impl CacheKey {
    fn for_call(call: &AuthorizedCall) -> Self {
        Self {
            tool: call.tool.clone(),
            args_digest: blake3::hash(&canonical_json_bytes(&call.arguments)),
        }
    }
}

/// One remembered `AllowAlways` grant, scoped per architecture §4.3.
///
/// `pub` so the structured-allow review surface can hand each rule's scope
/// back to the operator (`PermissionBroker::active_structured_allow_rules`)
/// -- an allow rule is the ONE rule kind that is scoped (D4 §3's asymmetry:
/// `deny`/`prompt` apply to every requester), so an inspection surface that
/// hid the scope would misrepresent how much of the agent tree a grant
/// actually covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GrantScope {
    Session,
    Agent(AgentId),
    Subtree(AgentId),
}

impl GrantScope {
    /// A `Session` grant hits for any requester; an `Agent` grant only for
    /// the exact granting agent; a `Subtree` grant for any requester whose
    /// `agent_path` contains the granting agent (a descendant, or the
    /// granting agent itself).
    fn covers(&self, ctx: &PermissionCtx) -> bool {
        match self {
            GrantScope::Session => true,
            GrantScope::Agent(granter) => *granter == ctx.agent_id,
            GrantScope::Subtree(granter) => ctx.agent_path.contains(granter),
        }
    }

    /// A human-readable rendering for the review surface, alongside
    /// `Rule::describe`/`PatternOrigin::describe` -- the third fact about a
    /// grant row: who it actually covers.
    pub fn describe(&self) -> String {
        match self {
            GrantScope::Session => "session".to_string(),
            GrantScope::Agent(granter) => format!("agent {granter}"),
            GrantScope::Subtree(granter) => format!("agent subtree under {granter}"),
        }
    }
}

/// Maps a `PermissionScope` onto the broker's internal `GrantScope`.
///
/// Shared by the `AllowAlways` cache and V2's pattern grants so the two
/// cannot drift on the `#[non_exhaustive]` fallback: an unknown scope
/// narrows to `Agent`, never widens to `Session`.
fn grant_scope_for(scope: PermissionScope, granting_agent: AgentId) -> GrantScope {
    match scope {
        PermissionScope::Session => GrantScope::Session,
        PermissionScope::Agent => GrantScope::Agent(granting_agent),
        PermissionScope::AgentSubtree => GrantScope::Subtree(granting_agent),
        // `PermissionScope` is `#[non_exhaustive]`: fall back to the
        // narrowest known grant rather than silently widening it.
        _ => GrantScope::Agent(granting_agent),
    }
}

/// F12: the stored form of an ALLOW rule. A `Rule` plus the `CanonicalRoot`
/// its `When::PathsUnder` prefix was resolved to once at install (`None` for
/// every render-based `when`), plus the `GrantScope` the rule was granted at
/// (an allow rule is scoped -- D4 §3's asymmetric half -- so it is checked
/// only for requesters that scope `covers`), plus the rule's `PatternOrigin`.
/// Named so the 4-tuple is not spelled out at every read/write site.
type AllowRuleStore = Vec<(Rule, Option<CanonicalRoot>, GrantScope, PatternOrigin)>;

/// F12: the stored form of a NARROWING rule (`deny` or `prompt`). A `Rule`
/// plus the `CanonicalRoot` its `When::PathsUnder` prefix resolved to once at
/// install (`None` for every render-based `when`), plus the rule's
/// `PatternOrigin`. Carries NO `GrantScope`: a narrowing rule applies to every
/// requester regardless of who is asking (D4 §3's asymmetry, extended to
/// `prompt` by extension-architecture.md §5.5), so the scope dimension the
/// allow store carries is absent here -- hence the separate 3-tuple type.
/// `prompt_patterns` is structurally identical and could use this alias too;
/// it stays inline only because it predates the alias.
type NarrowingRuleStore = Vec<(Rule, Option<CanonicalRoot>, PatternOrigin)>;

/// A per-session decision cache over the consumer's [`PermissionGate`]
/// (architecture §4.3). One broker instance is shared across an agent tree.
pub struct PermissionBroker {
    gate: Arc<dyn PermissionGate>,
    bus: Arc<EventBus>,
    cache: RwLock<HashMap<CacheKey, Vec<GrantScope>>>,
    /// V2: how much the operator is asked. Runtime-mutable via
    /// [`PermissionBroker::set_mode`] so `/settings` can switch out of an
    /// over-broad mode mid-session without a restart.
    mode: RwLock<PermissionMode>,
    /// V2: prefix-pattern ALLOW grants, paired with the scope they were
    /// granted at and where they came from. Checked BEFORE the gate, so a
    /// matching pattern spares the operator a prompt -- but only for
    /// commands that clear `PatternRule::matches_render`'s metacharacter
    /// gate (a shell command, per the call's `RenderKind`). Board item
    /// 01KYT8SGX32CP56PRJNG72V2W5: the CALLER (`conway`'s facade,
    /// `conway-cli`'s startup loader) is responsible for confirming trust
    /// before an allow rule loaded from a project file ever reaches
    /// `remember_pattern` -- this broker has no file-trust concept of its
    /// own and does not need one; it only stores what it is told to.
    patterns: RwLock<AllowRuleStore>,
    /// Board item 01KYT8SGX32CP56PRJNG72V2W5: prefix-pattern DENY rules.
    /// Unlike `patterns` above, these carry no `GrantScope` -- a `deny`
    /// rule is D4 §3's asymmetric half, "applies immediately, trusted or
    /// not, from any file, to any requester," so it is checked in
    /// `Self::decide` for EVERY call regardless of who is asking. Matched
    /// via `PatternRule::matches_deny`, which deliberately does not consult
    /// the metacharacter gate `patterns` above is gated by -- see that
    /// method's own doc.
    deny_patterns: RwLock<NarrowingRuleStore>,
    /// Board item 01KYTP1D3XWEZPW4AKPH54FNB3: prefix-pattern PROMPT rules --
    /// the second narrowing effect `.design/extension-architecture.md`
    /// §5.4 grants a plugin-contributed rule (`then: prompt`, alongside
    /// `deny`), which had NOTHING evaluating it anywhere in this broker
    /// before this item: `must_reach_gate` was set exclusively by
    /// `check_root`, so a `prompt` rule could never force `gate.check` and
    /// was inert in every mode (see this item's own board record for the
    /// two concrete failures this caused).
    ///
    /// Structurally identical to `deny_patterns` -- no `GrantScope` (a
    /// narrowing rule applies to every requester, D4 §3's asymmetry extends
    /// unchanged to `prompt`: extension-architecture.md §5.5 stage 1 admits
    /// `deny` AND `prompt` unconditionally, `allow` only when trusted), and
    /// matched with `PatternRule::matches_deny` for the identical
    /// anti-evasion reason (see `Self::prompt_matches`'s own doc). What
    /// differs is the EFFECT: a matched deny rule returns `Deny` immediately
    /// from `Self::decide`; a matched prompt rule does not deny anything --
    /// it sets the broker-level `must_reach_gate` accumulator, forcing this
    /// call past the cache/pattern/`AutoAllow` shortcuts and into
    /// `gate.check` exactly as `check_root`'s own `MustReachGate` already
    /// does for an unconfinable call under a root.
    ///
    /// F12: all three vectors now store [`Rule`]s (the flat form desugars
    /// via [`PatternRule::to_rule`] at the `remember_*` boundary) plus an
    /// optional [`CanonicalRoot`] -- `Some` for a [`When::PathsUnder`] rule
    /// (its prefix canonicalized ONCE at install, the same way
    /// [`AgentRoot::reconstruct`] canonicalizes a confinement root once),
    /// `None` for every render-based `when`. The `remember_pattern`/
    /// `remember_deny_pattern`/`remember_prompt_pattern` signatures stay
    /// [`PatternRule``]-typed so every existing caller (the
    /// `conway-runtime` broker tests, the `conway` seam tests, the TUI)
    /// keeps compiling unchanged; they desugar internally. The
    /// `remember_*_rule` companions take a [`Rule`] directly, for the
    /// structured form the flat syntax cannot express.
    prompt_patterns: RwLock<Vec<(Rule, Option<CanonicalRoot>, PatternOrigin)>>,
    /// Board item 01KZS00JP5QNBJSSHNFP9C47GM: the injected `pre_tool_use`
    /// hook dispatcher. `None` (the default, and every caller before this
    /// field existed) means the hook-check step in `Self::decide` is a
    /// byte-for-byte no-op -- see [`Self::set_hook_runner`]'s own doc for
    /// the full "additive, not a new dependency" contract.
    hook_runner: RwLock<Option<Arc<dyn HookRunner>>>,
    /// Board item 01KZS00JP5QNBJSSHNFP9C47GM: the `[hooks].rules[]` entries
    /// (already filtered to `event == "pre_tool_use" && enabled` by the
    /// facade) `Self::decide`'s hook-check step consults, in installation
    /// order. Empty (the default) is the same no-op as `hook_runner` being
    /// `None` -- both must be populated for the step to do anything, and
    /// either alone is inert by construction (see
    /// [`Self::pre_tool_use_hook_denial`]).
    pre_tool_use_hooks: RwLock<Vec<PreToolUseHookSpec>>,
}

impl PermissionBroker {
    pub fn new(gate: Arc<dyn PermissionGate>, bus: Arc<EventBus>) -> Self {
        Self {
            gate,
            bus,
            cache: RwLock::new(HashMap::new()),
            mode: RwLock::new(PermissionMode::default()),
            patterns: RwLock::new(Vec::new()),
            deny_patterns: RwLock::new(Vec::new()),
            prompt_patterns: RwLock::new(Vec::new()),
            hook_runner: RwLock::new(None),
            pre_tool_use_hooks: RwLock::new(Vec::new()),
        }
    }

    /// Injects (or clears, via `None`) the `pre_tool_use` hook dispatcher
    /// every call to `Self::decide` consults at the deny tier -- board item
    /// 01KZS00JP5QNBJSSHNFP9C47GM. Mirrors `Runtime::set_context_hook`'s own
    /// post-construction-setter shape (`conway::ConwayBuilder::
    /// with_hook_runner` is this method's own facade-level caller, via
    /// `Runtime::set_hook_runner`): not called at all (the default) leaves
    /// every existing `decide()` behavior byte-for-byte unchanged, since the
    /// hook-check step short-circuits to "no opinion" the instant it finds
    /// no runner installed, before it ever reads `pre_tool_use_hooks` or
    /// performs any I/O.
    pub fn set_hook_runner(&self, runner: Option<Arc<dyn HookRunner>>) {
        *self.hook_runner.write().expect("hook runner lock poisoned") = runner;
    }

    /// Installs the `pre_tool_use` hook specs `Self::decide`'s hook-check
    /// step consults, wholesale (replacing whatever was installed before) --
    /// board item 01KZS00JP5QNBJSSHNFP9C47GM. The facade computes this list
    /// once, from `[hooks].rules[]` filtered to `event == "pre_tool_use" &&
    /// enabled`, before any session starts; not called at all (the default,
    /// an empty list) is the identical no-op `Self::set_hook_runner(None)`
    /// is, and either one alone is enough to keep the hook-check step inert.
    pub fn set_pre_tool_use_hooks(&self, hooks: Vec<PreToolUseHookSpec>) {
        *self
            .pre_tool_use_hooks
            .write()
            .expect("pre_tool_use hooks lock poisoned") = hooks;
    }

    /// The current mode. Cheap enough to call per render -- the status
    /// line reads it every frame so the operator can never be uncertain
    /// which mode they are in.
    pub fn mode(&self) -> PermissionMode {
        *self.mode.read().expect("permission mode poisoned")
    }

    /// Switches mode at runtime. This is the escape hatch: an operator who
    /// finds themselves in an over-broad `AutoAllow` returns to `Prompt`
    /// without restarting the session.
    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.write().expect("permission mode poisoned") = mode;
    }

    /// Installs a pattern ALLOW grant at `scope`, attributed to `origin`
    /// for the review surface (board item 01KYT8SGX32CP56PRJNG72V2W5).
    ///
    /// Note this does NOT pre-validate the rule against metacharacters:
    /// the gate lives in `PatternRule::matches_render`, applied to the
    /// incoming COMMAND at decision time. Filtering at creation time
    /// instead would be the wrong shape -- it would let a rule created
    /// before the gate existed, or loaded from a file, slip past.
    ///
    /// This method trusts its caller completely: it installs whatever it
    /// is given, from whatever `origin` it is told. The trust DECISION for
    /// an `origin: PatternOrigin::File(path)` rule loaded from a
    /// project-scoped file belongs to the caller (`conway-cli`'s startup
    /// loader), made once, before this is ever called -- this broker is
    /// not the enforcement point and must not become one, or there would
    /// be two places a future change to that logic would need to agree.
    pub fn remember_pattern(
        &self,
        rule: PatternRule,
        scope: PermissionScope,
        granting_agent: AgentId,
        origin: PatternOrigin,
    ) {
        // A flat rule desugars to `When::Always`/`When::CommandPrefix` only
        // (`PatternRule::to_rule`) -- never `When::PathsUnder` -- so the
        // `base` `remember_pattern_rule` takes is never consulted on this
        // path. The placeholder is therefore not a resolution choice; it
        // only satisfies the signature. The `debug_assert!` turns that
        // comment into code (B2 review): if a future flat-form extension
        // ever desugars to `PathsUnder`, this placeholder would silently
        // resolve the prefix against `/` -- fail the test build instead.
        let desugared = rule.to_rule(Then::Allow);
        debug_assert!(
            !matches!(desugared.when, When::PathsUnder(_)),
            "flat rules must never desugar to PathsUnder: the placeholder \
             base would silently resolve the prefix against `/`"
        );
        self.remember_pattern_rule(desugared, scope, granting_agent, origin, Path::new("/"));
    }

    /// F12: the structured-form companion to [`Self::remember_pattern`].
    /// Installs an ALLOW [`Rule`] directly -- the form `parse_rules` returns
    /// and `load_permission_files` installs. Returns `false` (and installs
    /// nothing) if the rule carries a [`When::PathsUnder`] whose prefix
    /// cannot be canonicalized (fail closed: an uncanonicalizable path
    /// boundary is dropped, not stored inert -- a rule that can never match
    /// is a lie the operator will not notice, the mirror of the
    /// `68ea9b1` `read:*`-matched-nothing bug). The caller surfaces that
    /// failure; the broker itself never panics on untrusted input (P-10).
    ///
    /// `base` is the directory a RELATIVE `paths_under` prefix resolves
    /// against (B2 -- see [`canonicalize_when`]`s own doc for why this is an
    /// explicit parameter and never an implicit process-cwd read). It is
    /// ignored for every other `when` clause, and for an absolute prefix.
    pub fn remember_pattern_rule(
        &self,
        rule: Rule,
        scope: PermissionScope,
        granting_agent: AgentId,
        origin: PatternOrigin,
        base: &Path,
    ) -> bool {
        // An allow rule MUST be `then: allow` -- a deny/prompt rule routed
        // here would silently do the wrong thing at `pattern_allows`. The
        // `load_permission_files` split already separates by `then`, but
        // encoding the invariant here means a future caller cannot retrofit
        // a narrowing `then` through the allow path without crossing it.
        if rule.then != Then::Allow {
            return false;
        }
        // A4: a plugin-contributed rule may only NARROW (`deny`/`prompt`) --
        // extension-architecture.md §5.5 stage 1, "allow is operator-owned."
        // Today no plugin transport exists, so this guard is unreachable in
        // production; it is encoded HERE, at the broker boundary, so a
        // future transport that reuses `PatternOrigin::Plugin` to call the
        // allow path with `Then::Allow` hits a STRUCTURAL refusal rather
        // than silently installing a durable grant the operator never
        // authorized. The invariant rests on a guard, not on the absence of
        // a transport. P-10: a typed `false` (the existing rejection shape
        // the other `remember_*_rule` callers already honor), never a panic.
        if matches!(origin, PatternOrigin::Plugin) {
            return false;
        }
        let canonical = canonicalize_when(&rule.when, base);
        if canonical.is_none() && matches!(rule.when, When::PathsUnder(_)) {
            return false;
        }
        let grant = grant_scope_for(scope, granting_agent);
        self.patterns
            .write()
            .expect("permission patterns poisoned")
            .push((rule, canonical, grant, origin));
        true
    }

    /// Installs a DENY rule, attributed to `origin`. Unlike
    /// [`Self::remember_pattern`], there is no `scope` parameter: a deny
    /// rule applies to every requester in the session, unconditionally --
    /// narrowing what is authorized has no failure mode worth scoping
    /// (board item 01KYT8SGX32CP56PRJNG72V2W5, D4 §3).
    pub fn remember_deny_pattern(&self, rule: PatternRule, origin: PatternOrigin) {
        // Never-`PathsUnder` desugaring, so `base` is never consulted --
        // see `remember_pattern`'s own comment and its `debug_assert!`.
        let desugared = rule.to_rule(Then::Deny);
        debug_assert!(
            !matches!(desugared.when, When::PathsUnder(_)),
            "flat rules must never desugar to PathsUnder: the placeholder \
             base would silently resolve the prefix against `/`"
        );
        self.remember_deny_rule(desugared, origin, Path::new("/"));
    }

    /// F12: the structured-form companion to [`Self::remember_deny_pattern`].
    /// Installs a DENY [`Rule`] directly. Same fail-closed posture as
    /// [`Self::remember_pattern_rule`] for a [`When::PathsUnder`] whose
    /// prefix cannot be canonicalized; `base` has the identical meaning
    /// (B2) and is likewise ignored for every other `when` clause.
    pub fn remember_deny_rule(&self, rule: Rule, origin: PatternOrigin, base: &Path) -> bool {
        if rule.then != Then::Deny {
            return false;
        }
        let canonical = canonicalize_when(&rule.when, base);
        if canonical.is_none() && matches!(rule.when, When::PathsUnder(_)) {
            return false;
        }
        self.deny_patterns
            .write()
            .expect("permission deny patterns poisoned")
            .push((rule, canonical, origin));
        true
    }

    /// Installs a PROMPT rule, attributed to `origin`. Board item
    /// 01KYTP1D3XWEZPW4AKPH54FNB3: the second narrowing effect
    /// `.design/extension-architecture.md` §5.4 grants a
    /// plugin-contributed rule. Like [`Self::remember_deny_pattern`], there
    /// is no `scope` parameter -- a `prompt` rule applies to every
    /// requester, unconditionally: forcing an EXTRA ask has no failure mode
    /// worth scoping, the same reasoning `remember_deny_pattern`'s own doc
    /// gives for `deny`.
    pub fn remember_prompt_pattern(&self, rule: PatternRule, origin: PatternOrigin) {
        // Never-`PathsUnder` desugaring, so `base` is never consulted --
        // see `remember_pattern`'s own comment and its `debug_assert!`.
        let desugared = rule.to_rule(Then::Prompt);
        debug_assert!(
            !matches!(desugared.when, When::PathsUnder(_)),
            "flat rules must never desugar to PathsUnder: the placeholder \
             base would silently resolve the prefix against `/`"
        );
        self.remember_prompt_rule(desugared, origin, Path::new("/"));
    }

    /// F12: the structured-form companion to [`Self::remember_prompt_pattern`].
    /// Installs a PROMPT [`Rule`] directly. Same fail-closed posture as the
    /// other `remember_*_rule` companions for a [`When::PathsUnder`] whose
    /// prefix cannot be canonicalized; `base` has the identical meaning
    /// (B2) and is likewise ignored for every other `when` clause.
    pub fn remember_prompt_rule(&self, rule: Rule, origin: PatternOrigin, base: &Path) -> bool {
        if rule.then != Then::Prompt {
            return false;
        }
        let canonical = canonicalize_when(&rule.when, base);
        if canonical.is_none() && matches!(rule.when, When::PathsUnder(_)) {
            return false;
        }
        self.prompt_patterns
            .write()
            .expect("permission prompt patterns poisoned")
            .push((rule, canonical, origin));
        true
    }

    /// S5: the root-containment check. Evaluated before anything else in
    /// [`Self::decide`] — see that method's own doc for exactly why
    /// (structurally, this must precede every one of the four allow paths,
    /// not live inside `PermissionGate`).
    ///
    /// Reads `call.arguments` — **never** `call.rendered`, which has
    /// already passed through `runner.rs`'s `sanitize_rendered` and would
    /// reintroduce the exact fail-open bug class (a safe-looking
    /// transformation silently laundering evidence a security check
    /// depends on) that shipped in 0.5.0.
    fn check_root(ctx: &PermissionCtx, call: &AuthorizedCall) -> RootDecision {
        let root = match &ctx.root {
            AgentRoot::Unconfined => return RootDecision::Proceed,
            AgentRoot::Broken => {
                return RootDecision::Denied(format!(
                    "`{}` is denied: this agent's confinement root could not be \
                     re-established (it no longer resolves on disk)",
                    call.tool.as_str()
                ));
            }
            AgentRoot::Confined(root) => root,
        };

        // `Named` never forces the gate: a fully-confined tool whose every
        // declared path checks out proceeds through the ordinary allow
        // paths (cache/pattern/AutoAllow/gate) exactly as it did before
        // this slice. `Unconfinable` ALWAYS forces the gate under a root —
        // regardless of whether `checkable` is empty — because the part of
        // the call this broker cannot statically confine (e.g. `bash`'s
        // `command`) can still reach outside the root; `checkable` is
        // checked here in addition, not instead.
        let (names, must_reach_gate): (&[&str], bool) = match call.path_args {
            PathArgs::None => return RootDecision::Proceed,
            PathArgs::Named(names) => (names, false),
            PathArgs::Unconfinable { checkable } => (checkable, true),
            // `PathArgs` is `#[non_exhaustive]`: fail closed on any future
            // variant exactly like `PathArgs::Unconfinable { checkable: &[] }`
            // -- never `None`'s "nothing to check" shape.
            _ => (&[], true),
        };

        for name in names {
            match call.arguments.get(*name) {
                // Absent or explicitly `null`: this call simply doesn't use
                // this declared argument (e.g. `bash`'s optional `cwd`) —
                // not an error, and never silently skipped in the sense the
                // hazard inventory warns about (there is nothing here TO
                // check).
                None | Some(serde_json::Value::Null) => continue,
                Some(serde_json::Value::String(raw)) => {
                    let Some(resolved) = resolve_like_the_tool_will(&ctx.cwd, raw) else {
                        return RootDecision::Denied(format!(
                            "`{}` argument `{name}` ({raw:?}) cannot be resolved to a \
                             filesystem path",
                            call.tool.as_str()
                        ));
                    };
                    match root.contains(&resolved) {
                        Containment::Inside => {}
                        // `Undecidable` is fused with `Outside` here, same
                        // as everywhere else in this codebase that consults
                        // `Containment` — "can't check" is never "allow".
                        Containment::Outside | Containment::Undecidable => {
                            return RootDecision::Denied(format!(
                                "`{}` argument `{name}` resolves to {}, which is outside \
                                 this agent's confinement root ({})",
                                call.tool.as_str(),
                                resolved.display(),
                                root.as_path().display(),
                            ));
                        }
                    }
                }
                // A declared path argument present with a non-string,
                // non-null JSON value is suspicious, not merely
                // unparseable: silently skipping it (hazard #6) would let a
                // malformed or adversarial call slip past the check
                // entirely. Deny, fail closed.
                Some(other) => {
                    return RootDecision::Denied(format!(
                        "`{}` argument `{name}` must be a string path, found {other}",
                        call.tool.as_str()
                    ));
                }
            }
        }

        if must_reach_gate {
            RootDecision::MustReachGate
        } else {
            RootDecision::Proceed
        }
    }

    /// Every active pattern ALLOW grant, paired with its origin, for the
    /// settings menu's review list. An operator must be able to see what
    /// they have granted, AND where it came from; a rule set nobody can
    /// inspect -- or whose provenance nobody can tell -- is a trap (board
    /// item 01KYT8SGX32CP56PRJNG72V2W5).
    pub fn active_patterns(&self) -> Vec<(PatternRule, PatternOrigin)> {
        self.patterns
            .read()
            .expect("permission patterns poisoned")
            .iter()
            // F12: a stored `Rule` rounds back to the flat `PatternRule` it
            // desugared from when it can -- so the existing `PatternRule`-
            // shaped review surface keeps working unchanged for flat rules.
            // A structured rule the flat form cannot express (`paths_under`,
            // `categories`, `category_in`, multiple tools) is NOT returned
            // here; it is returned by [`Self::active_structured_allow_rules`]
            // so the review surface can list it without a type widening.
            .filter_map(|(rule, _, _, origin)| {
                rule.to_pattern_rule().map(|p| (p, origin.clone()))
            })
            .collect()
    }

    /// F12: every active ALLOW [`Rule`] the flat form cannot express, paired
    /// with its origin and the [`GrantScope`] it was remembered at -- the
    /// structured half of the allow review list, so a rule the operator
    /// installed via the `rules` array is inspectable the same way a flat
    /// `allow` grant is (a rule set nobody can inspect is a trap). Rules
    /// that round-trip to a flat [`PatternRule`] are listed by
    /// [`Self::active_patterns`] instead, so no rule appears in both. The
    /// scope rides along because an allow rule is the one rule kind that IS
    /// scoped (see [`GrantScope`]'s own doc): a review row that showed rule
    /// and origin but not scope would misrepresent how much of the agent
    /// tree the grant covers. The triple is also exactly the identity
    /// [`Self::revoke_pattern_rule`] addresses, so what the surface displays
    /// is what a revoke names.
    pub fn active_structured_allow_rules(&self) -> Vec<(Rule, PatternOrigin, GrantScope)> {
        self.patterns
            .read()
            .expect("permission patterns poisoned")
            .iter()
            .filter(|(rule, _, _, _)| rule.to_pattern_rule().is_none())
            .map(|(rule, _, scope, origin)| (rule.clone(), origin.clone(), *scope))
            .collect()
    }

    /// Every active DENY rule, paired with its origin -- the deny half's
    /// own review list, so a `deny` an operator did not expect (or forgot
    /// they wrote) is discoverable the same way an `allow` grant is.
    pub fn active_deny_patterns(&self) -> Vec<(PatternRule, PatternOrigin)> {
        self.deny_patterns
            .read()
            .expect("permission deny patterns poisoned")
            .iter()
            .filter_map(|(rule, _, origin)| {
                rule.to_pattern_rule().map(|p| (p, origin.clone()))
            })
            .collect()
    }

    /// F12: the structured half of the deny review list -- mirrors
    /// [`Self::active_structured_allow_rules`] for deny rules the flat form
    /// cannot express.
    pub fn active_structured_deny_rules(&self) -> Vec<(Rule, PatternOrigin)> {
        self.deny_patterns
            .read()
            .expect("permission deny patterns poisoned")
            .iter()
            .filter(|(rule, _, _)| rule.to_pattern_rule().is_none())
            .map(|(rule, _, origin)| (rule.clone(), origin.clone()))
            .collect()
    }

    /// Every active PROMPT rule, paired with its origin -- the prompt half's
    /// own review list, mirroring [`Self::active_deny_patterns`]: a `prompt`
    /// an operator did not expect (or forgot they trusted) must be
    /// discoverable the same way a `deny` is.
    pub fn active_prompt_patterns(&self) -> Vec<(PatternRule, PatternOrigin)> {
        self.prompt_patterns
            .read()
            .expect("permission prompt patterns poisoned")
            .iter()
            .filter_map(|(rule, _, origin)| {
                rule.to_pattern_rule().map(|p| (p, origin.clone()))
            })
            .collect()
    }

    /// F12: the structured half of the prompt review list -- mirrors
    /// [`Self::active_structured_allow_rules`] for prompt rules the flat
    /// form cannot express (the flat form has no prompt syntax at all, so
    /// every prompt rule the `rules` array installed lives here).
    pub fn active_structured_prompt_rules(&self) -> Vec<(Rule, PatternOrigin)> {
        self.prompt_patterns
            .read()
            .expect("permission prompt patterns poisoned")
            .iter()
            .filter(|(rule, _, _)| rule.to_pattern_rule().is_none())
            .map(|(rule, _, origin)| (rule.clone(), origin.clone()))
            .collect()
    }

    /// Drops every pattern ALLOW grant and every cached `AllowAlways`,
    /// returning the session to asking. The revocation half of the escape
    /// hatch.
    ///
    /// Deliberately leaves `deny_patterns` AND `prompt_patterns` untouched:
    /// revocation exists so an operator can back out of authority they
    /// granted (interactively, or via a trusted file) that turned out to be
    /// too broad. Neither `deny` nor `prompt` grants anything -- both
    /// narrow -- so there is nothing here for an operator to need an escape
    /// hatch FROM -- and most `deny`/`prompt` rules come from a file the
    /// operator does not control (or has not reviewed as carefully), so
    /// silently dropping them as a side effect of an unrelated "revoke my
    /// own grants" action would be a surprise in the unsafe direction.
    pub fn revoke_all_grants(&self) {
        self.patterns
            .write()
            .expect("permission patterns poisoned")
            .clear();
        self.cache
            .write()
            .expect("permission cache poisoned")
            .clear();
    }

    /// Revokes exactly ONE installed pattern ALLOW grant, addressed by the
    /// value it renders as -- `(rule, origin)` -- rather than by position
    /// in `active_patterns()`. Board item 01KYND4WGHSZXW5YQ6ZWHCDDNN.
    ///
    /// ## Why `(PatternRule, PatternOrigin)` identity, not an index
    ///
    /// An index into `active_patterns()` is the identity that is easiest to
    /// wire up -- the settings menu already renders one row per entry, in
    /// order -- but it is also the most fragile: a concurrent
    /// `remember_pattern` call (a permission prompt answered on another
    /// agent's turn, or a permissions file reloaded mid-session) can insert
    /// before the row the operator is looking at, so by the time the
    /// revoke reaches this method the index could address a DIFFERENT
    /// grant than the one the operator actually selected -- silently
    /// revoking the wrong rule. `(PatternRule, PatternOrigin)` is exactly
    /// the pair the review row already displays (`rule.describe()`,
    /// `origin.describe()`), so what the operator SAW is the identity
    /// ADDRESSED, and it stays correct regardless of what else is
    /// inserted or removed elsewhere in the vector between render and
    /// revoke.
    ///
    /// Removes the FIRST entry whose rule and origin both compare equal
    /// (both types are `Eq`) -- not every match. Two entries with
    /// byte-identical `(rule, origin)` would render as two
    /// indistinguishable rows; revoking "this row" removes exactly one
    /// grant instance and leaves the other in place (still shown, still
    /// revocable the same way on a second selection) rather than guessing
    /// the operator meant to clear both at once.
    ///
    /// Returns whether anything was removed. Deliberately narrower than
    /// [`Self::revoke_all_grants`]: it never touches `deny_patterns` (no
    /// deny counterpart exists here at all -- see `conway::Conway::
    /// revoke_permission_pattern`'s own doc for why per-rule deny
    /// revocation through this surface was decided against) and never
    /// touches `cache` (an `AllowAlways` cache entry is a *different*
    /// mechanism -- a gate's own per-call decision, not a pattern grant --
    /// so revoking a pattern must not reach into it).
    pub fn revoke_pattern(&self, rule: &PatternRule, origin: &PatternOrigin) -> bool {
        let mut patterns = self.patterns.write().expect("permission patterns poisoned");
        match patterns
            .iter()
            .position(|(r, _, _, o)| r.to_pattern_rule() == Some(rule.clone()) && o == origin)
        {
            Some(idx) => {
                patterns.remove(idx);
                true
            }
            None => false,
        }
    }

    /// The structured-allow counterpart to [`Self::revoke_pattern`] (board
    /// item A2): revokes exactly ONE installed ALLOW grant addressed by the
    /// full [`Rule`] value plus its origin, matched by `Rule` EQUALITY
    /// rather than by the flat `to_pattern_rule()` projection -- which is
    /// what makes a structured allow rule (`paths_under`, `categories`,
    /// `category_in`, multiple tools) individually revocable at all:
    /// `revoke_pattern` can never name one, because its key collapses every
    /// structured rule to `None`.
    ///
    /// The identity reasoning of [`Self::revoke_pattern`] applies unchanged
    /// -- `(rule, origin)` is exactly what the review row displays
    /// (`active_structured_allow_rules`), so what the operator saw is what
    /// is revoked, and no index can drift between render and revoke. Two
    /// deliberate choices:
    ///
    /// - **The [`GrantScope`] IS part of the key** (unlike `revoke_pattern`'s
    ///   flat key, which ignores the stored scope). The review row annotates
    ///   scope, so two entries equal in `(rule, origin)` but differing in
    ///   scope render as two distinguishable rows, and revoking one removes
    ///   THAT instance: "what you saw is what you revoke" holds exactly. A
    ///   scope-blind first-match could remove the session-scoped instance
    ///   when the operator pointed at the agent-scoped row (code-review
    ///   finding on this item; the failure direction was safe -- net
    ///   authority only shrank -- but the mismatch was user-visible).
    /// - **Any stored allow rule matches, not only structured ones.** A
    ///   flat-desugarable rule IS a `Rule` in the store, so equality can
    ///   name it too. That overlap is harmless: the review surface
    ///   partitions rows (`active_patterns` vs
    ///   `active_structured_allow_rules`), so each row is revoked through
    ///   exactly one of the two methods, and both remove one instance of
    ///   the same store.
    ///
    /// Returns whether anything was removed. Like [`Self::revoke_pattern`],
    /// never touches `deny_patterns`/`prompt_patterns` and never touches
    /// the `AllowAlways` cache (a different mechanism -- see that method's
    /// own doc).
    pub fn revoke_pattern_rule(
        &self,
        rule: &Rule,
        origin: &PatternOrigin,
        scope: &GrantScope,
    ) -> bool {
        let mut patterns = self.patterns.write().expect("permission patterns poisoned");
        match patterns
            .iter()
            .position(|(r, _, s, o)| r == rule && o == origin && s == scope)
        {
            Some(idx) => {
                patterns.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Whether any installed pattern authorizes this call.
    ///
    /// F12: evaluation is now over the stored [`Rule`]s, via the single
    /// `Rule` evaluator ([`Rule::matches_allow_render`] for render-based
    /// `when` clauses, plus the broker's own `paths_under` resolution for
    /// [`When::PathsUnder`] -- the same `resolve_like_the_tool_will` +
    /// [`CanonicalRoot::contains`] path [`Self::check_root`] uses). The
    /// metacharacter gate lives in [`Rule::gate_allows`], applied for every
    /// `when` (not just `command_prefix`), unchanged in behavior from
    /// `PatternRule::matches_render`: a chained SHELL command can never
    /// satisfy a pattern here regardless of what is installed.
    fn pattern_allows(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> bool {
        self.patterns
            .read()
            .expect("permission patterns poisoned")
            .iter()
            .any(|(rule, canonical, grant, _origin)| {
                grant.covers(ctx) && rule_allows(ctx, call, rule, canonical.as_ref())
            })
    }

    /// The first installed `deny` rule that refuses this call, if any.
    /// Board item 01KYT8SGX32CP56PRJNG72V2W5, D4 §3: checked for EVERY
    /// requester (no `GrantScope`), via the deny/prompt evaluator
    /// ([`Rule::matches_deny_render`] for render-based `when` clauses, plus
    /// the broker's own `paths_under` resolution for [`When::PathsUnder`])
    /// -- deliberately UNGATED, so a deny rule cannot be evaded by adding a
    /// shell metacharacter the way an allow rule is refused by one. See
    /// `PatternRule::matches_deny`'s own doc for the reasoning and its
    /// honest limit.
    fn deny_matches(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> Option<Rule> {
        self.deny_patterns
            .read()
            .expect("permission deny patterns poisoned")
            .iter()
            .find(|(rule, canonical, _origin)| {
                rule_denies_or_prompts(ctx, call, rule, canonical.as_ref())
            })
            .map(|(rule, _, _)| rule.clone())
    }

    /// The first installed `prompt` rule that matches this call, if any.
    /// Board item 01KYTP1D3XWEZPW4AKPH54FNB3.
    ///
    /// **Deliberately reuses the deny/prompt evaluator (ungated), not
    /// `Rule::matches_allow_render`.** The allow-side metacharacter gate
    /// exists to keep an ALLOW from being satisfied by a chained command
    /// riding a matched prefix -- a concern that only applies to a rule
    /// that GRANTS something. A `prompt` rule grants nothing; its only
    /// effect is "ask the operator instead of skipping the ask", which is
    /// safe (indeed MORE conservative) to fire on a chained command too.
    /// Gating it the allow way would have the opposite of the intended
    /// effect: adding a shell metacharacter would EVADE the extra scrutiny
    /// a `prompt` rule exists to add, exactly the inversion
    /// `PatternRule::matches_deny`'s own doc describes for `deny`. `prompt`
    /// and `deny` are both admitted unconditionally at
    /// extension-architecture.md §5.5's stage 1 (they narrow; only `allow`
    /// needs trust) precisely because neither can be evaded this way.
    fn prompt_matches(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> Option<Rule> {
        self.prompt_patterns
            .read()
            .expect("permission prompt patterns poisoned")
            .iter()
            .find(|(rule, canonical, _origin)| {
                rule_denies_or_prompts(ctx, call, rule, canonical.as_ref())
            })
            .map(|(rule, _, _)| rule.clone())
    }

    /// The rendered denial, if any INSTALLED `pre_tool_use` hook refuses
    /// this call -- board item 01KZS00JP5QNBJSSHNFP9C47GM, `Self::decide`'s
    /// only caller (see that method's own doc for WHY this sits at the deny
    /// tier).
    ///
    /// **Deny-only, not three-way (deny/prompt/no-opinion) -- decided, not
    /// left open.** A hook that wants a human decision rather than an
    /// outright refusal already has two existing ways to get one without
    /// this method growing a second `must_reach_gate` source of its own: an
    /// operator-installed `prompt` pattern rule (`Self::prompt_matches`,
    /// above) for a call shape a plugin author can identify statically, or
    /// the hook script itself simply choosing not to run in "blocking" mode.
    /// `HookPermissionVerdict` (this method's own answer type) has no
    /// `Prompt` variant for the identical reason `decide()`'s GP-13 bound
    /// applies to this whole item: one narrowing-only chain step, a FIXED
    /// amount of mechanism, not a variable one -- adding a second
    /// `must_reach_gate` source here, with different provenance than
    /// `prompt_matches`' own, is exactly the kind of branching growth that
    /// bound exists to block, for a capability the pattern-rule mechanism
    /// already covers.
    ///
    /// **Fail-closed inherits from the runner, not a second implementation
    /// of it.** `HookRunner::run`'s `Err(HookFailure)` -- a missing script,
    /// a timeout, or stdout that failed to parse as a [`conway_core::hook::
    /// HookAnswer`] -- is treated as a denial by this method directly; there
    /// is no separate "is this hook broken" check layered on top that could
    /// disagree with the runner's own verdict.
    ///
    /// Every `RwLock` this reads is acquired, cloned out of, and released
    /// BEFORE the only `.await` point below (`runner.run`) -- the same
    /// never-hold-a-lock-across-an-await invariant `Self::decide`'s own doc
    /// states for its cache lock.
    async fn pre_tool_use_hook_denial(
        &self,
        ctx: &PermissionCtx,
        call: &AuthorizedCall,
    ) -> Option<String> {
        let runner = self
            .hook_runner
            .read()
            .expect("hook runner lock poisoned")
            .clone()?;
        let hooks = self
            .pre_tool_use_hooks
            .read()
            .expect("pre_tool_use hooks lock poisoned")
            .clone();
        if hooks.is_empty() {
            return None;
        }

        // Built once, reused for every configured hook: `AuthorizedCall`'s
        // `tool`/`category`/`arguments`/`rendered` (this method's own board
        // item's own "what to build" section) plus enough of `PermissionCtx`
        // for a script to know who is asking and from where.
        let payload = serde_json::json!({
            "tool": call.tool.as_str(),
            "category": call.category,
            "arguments": call.arguments,
            "rendered": call.rendered,
            "agent_id": ctx.agent_id,
            "agent_path": ctx.agent_path,
            "session": ctx.session,
            "cwd": ctx.cwd,
        });

        for hook in &hooks {
            let invocation = HookInvocation {
                command: hook.command.clone(),
                timeout_ms: hook.timeout_ms,
                event: HookEvent {
                    name: "pre_tool_use".to_string(),
                    payload: payload.clone(),
                },
            };
            match runner.run(&invocation).await {
                Ok(answer) => {
                    if let HookPermissionVerdict::Deny { reason } = answer.permission {
                        return Some(format!(
                            "`{}` is denied by `pre_tool_use` hook `{}`: {reason}",
                            call.tool.as_str(),
                            hook.id
                        ));
                    }
                    // `HookPermissionVerdict::NoOpinion`: this hook has
                    // nothing to say -- consult the next one, if any.
                }
                Err(failure) => {
                    return Some(format!(
                        "`{}` is denied: `pre_tool_use` hook `{}` failed ({failure}) -- \
                         fail-closed",
                        call.tool.as_str(),
                        hook.id
                    ));
                }
            }
        }
        None
    }

    /// Authorize one tool call, consulting the cache first and the gate on a
    /// miss.
    ///
    /// Full ordering (board item 01KZS00JP5QNBJSSHNFP9C47GM added the
    /// **`pre_tool_use` hook** step; board item 01KYTP1D3XWEZPW4AKPH54FNB3
    /// added `prompt`; every step before each addition is unchanged): root →
    /// deny-pattern → **`pre_tool_use` hook** → plan-mode → prompt-pattern →
    /// cache → pattern-allow → `AutoAllow` → gate. Each step before `gate`
    /// either returns a decision outright (root denial, deny-pattern, the
    /// hook step, plan-mode) or narrows what the LATER steps in this list
    /// are even allowed to do (root's `MustReachGate`, and `prompt`, both
    /// set the `must_reach_gate` accumulator, which skips cache/pattern-
    /// allow/`AutoAllow` entirely and forces `gate.check`). Composition is
    /// most-restrictive-wins and registration order within a step (which
    /// `deny`/`prompt` rule matched first, which pattern grant was installed
    /// first, which hook was consulted first) never changes the outcome,
    /// only which single value is picked to report.
    ///
    /// **Why the hook step sits at the SAME tier as `deny_matches` --
    /// immediately after it, above every allow path in this method,
    /// including plan-mode.** A denying hook implemented downstream of
    /// `gate.check` (or as a `PermissionGate` itself) would see only the
    /// calls that reach the gate, and NONE resolved by the cache, a
    /// pattern-allow rule, or `AutoAllow` mode -- it would evaporate
    /// entirely under `AutoAllow`, which is exactly the mode with no human
    /// already in the loop to catch what the hook exists to catch. Placing
    /// the check here, returning `Deny` outright and unconditionally exactly
    /// as `deny_matches` does two lines below in the source, makes that
    /// evaporation structurally impossible: nothing after this point in
    /// `decide()` can run for a call this step already denied, and no mode,
    /// cache entry, or pattern grant is even consulted before it does.
    ///
    /// Emission sequence, strictly: `PermissionRequested` → (root denial, or
    /// plan-mode denial, or cache hit, or await the gate and insert a cache
    /// entry on `AllowAlways`) → `PermissionResolved`. The cache's `RwLock`
    /// is never held across the `await` on `self.gate.check` — every lock
    /// acquisition in this method is a short, synchronous read or write
    /// that completes before any `await` point.
    pub async fn decide(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> PermissionOutcome {
        let key = CacheKey::for_call(call);

        self.emit(
            ctx,
            Event::PermissionRequested {
                call_id: call.call_id.clone(),
                rendered: call.rendered.clone(),
            },
        );

        // S5: the root-containment check goes FIRST -- above every allow
        // path in this method, including the plan-mode gate immediately
        // below. A confined agent's root can never be widened, satisfied,
        // or bypassed by a pattern grant, a cached `AllowAlways`, or
        // `AutoAllow` mode: `must_reach_gate` (set when the call is
        // `PathArgs::Unconfinable` under a root) skips straight past all
        // three of those, forcing this call to the operator's gate instead
        // of auto-allowing it -- but it is not itself a denial. A `Denied`
        // root decision returns immediately, before the cache/pattern/
        // AutoAllow/gate are ever consulted.
        //
        // Board item 01KYTP1D3XWEZPW4AKPH54FNB3: `must_reach_gate` is now a
        // BROKER-LEVEL ACCUMULATOR, not `check_root`'s exclusive output --
        // `mut` below, OR'd with the prompt-rule check further down. It is
        // never cleared once set: every source that can set it (`check_root`
        // here, `Self::prompt_matches` below, and any future narrowing
        // source) may only ADD a reason to reach the gate, never remove one,
        // so this does not, and structurally cannot, weaken the root-forced
        // case a confined agent depends on (pinned by
        // `unconfinable_bash_command_always_reaches_the_gate_for_a_confined_root_agent`
        // in `crates/conway/tests/root_containment_seam.rs`).
        let mut must_reach_gate = match Self::check_root(ctx, call) {
            RootDecision::Denied(reason) => {
                self.emit(
                    ctx,
                    Event::PermissionResolved {
                        call_id: call.call_id.clone(),
                        decision: PermissionDecisionKind::Denied,
                    },
                );
                return PermissionOutcome::Deny {
                    rendered_error: reason,
                };
            }
            RootDecision::MustReachGate => true,
            RootDecision::Proceed => false,
        };

        // Board item 01KYT8SGX32CP56PRJNG72V2W5, D4 §3: the `deny` half of
        // the allow/deny asymmetry. Checked immediately after the root
        // floor and BEFORE the mode gate, the cache, pattern allows, and
        // AutoAllow -- a deny rule beats every one of those, regardless of
        // mode, regardless of whether its origin file was ever trusted
        // (deny applies unconditionally), and regardless of `must_reach_gate`
        // (a call that would otherwise be forced to the operator's gate is
        // denied outright instead, which is strictly MORE restrictive, not
        // less). This is "most-restrictive-wins, independent of order or
        // authorship" made concrete.
        if let Some(rule) = self.deny_matches(ctx, call) {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Denied,
                },
            );
            return PermissionOutcome::Deny {
                rendered_error: format!(
                    "`{}` is denied by a `deny` rule (`{}`)",
                    call.tool.as_str(),
                    rule.describe()
                ),
            };
        }

        // Board item 01KZS00JP5QNBJSSHNFP9C47GM: the `pre_tool_use` hook
        // step. SAME TIER as `deny_matches` immediately above -- checked
        // BEFORE the mode gate, the prompt-pattern step, the cache, pattern
        // allows, and `AutoAllow` -- see this method's own doc for why. A
        // denying hook beats every one of those, regardless of mode,
        // regardless of `must_reach_gate`, for the identical
        // most-restrictive-wins reason `deny_matches` already states. No
        // hook here can ever produce `Allow`: `Self::pre_tool_use_hook_
        // denial` returns `Some(..)` only for an explicit
        // `HookPermissionVerdict::Deny` or a runner failure (fail-closed),
        // never for `NoOpinion` -- and `HookPermissionVerdict` itself has no
        // `Allow` variant for a future edit to accidentally start acting on
        // (see that type's own doc).
        if let Some(rendered_error) = self.pre_tool_use_hook_denial(ctx, call).await {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Denied,
                },
            );
            return PermissionOutcome::Deny { rendered_error };
        }

        // V2 mode gate. Ordered deliberately: PLAN's denial is checked
        // before EVERY allow path -- the cache, pattern grants, and
        // AutoAllow alike -- so a plan-mode session cannot be talked out of
        // its denial by a cached `AllowAlways`, a pattern grant, or an
        // auto-allow left over from earlier. Plan mode is the mode an
        // operator selects when they want a guarantee, so it behaves like
        // one.
        let mode = self.mode();
        if mode == PermissionMode::Plan && !mode.allows_category(call.category) {
            self.emit(
                ctx,
                Event::PermissionResolved {
                    call_id: call.call_id.clone(),
                    decision: PermissionDecisionKind::Denied,
                },
            );
            return PermissionOutcome::Deny {
                // Min-4: the gap between the two sentences is `{:>22}` on an
                // empty string (22 spaces, byte-identical to the literal run
                // it replaces) -- the source holds no fragile 22-space run.
                rendered_error: format!(
                    "plan mode: `{}` is a {:?} tool, which plan mode does not permit.{:>22}Switch modes in /settings to run it.",
                    call.tool.as_str(),
                    call.category,
                    ""
                ),
            };
        }

        // Board item 01KYTP1D3XWEZPW4AKPH54FNB3: the PROMPT step.
        // Deliberately placed HERE -- after the deny check and the plan-mode
        // gate (both of which already returned a `Deny` and can never be
        // reached by a call this step would only ask about; "deny beats
        // prompt" therefore holds by construction, not by an ordering this
        // step has to get right), and BEFORE the cache/pattern/`AutoAllow`
        // block immediately below.
        //
        // **Why above the cache, specifically.** A plugin's `prompt` rule
        // existing at all is a claim that this class of call deserves a
        // human look EVERY time, not the first time. Checking it below the
        // cache would let the very first `AllowAlways` answer -- possibly
        // granted before the rule was ever installed, e.g. a plugin loaded
        // mid-session -- permanently suppress every future ask the rule was
        // meant to force. That is a real transfer of authority away from an
        // operator's own explicit "always allow" (see this method's own
        // history for that point being raised explicitly), but it is the
        // correct direction: `AllowAlways` is `PermissionScope`-bounded
        // consent to a class of call, not a promise that the class can never
        // later be flagged by a narrower rule -- exactly the same relationship
        // `deny` already has with the cache two branches above. Narrowing an
        // existing grant is always permitted (extension-architecture.md
        // §5.5 stage 1); WIDENING one never is, and this step only ever
        // narrows.
        //
        // **Why it must also beat `AutoAllow`, not just the cache/pattern
        // steps.** `AutoAllow` is the mode a guardrail plugin matters most
        // in: it is the one mode with no human already in the loop to catch
        // what the plugin's rule would have caught. A `prompt` effect that
        // worked in every mode except the one where it is load-bearing would
        // not be a partial fix, it would be the SAME bug restated -- so this
        // step sits above the whole `if !must_reach_gate` block, which is
        // what makes it structurally impossible for `AutoAllow`'s own branch
        // inside that block to ever run for a call this matched.
        //
        // **Attribution, decided rather than left implicit.** The operator
        // sees this ask through the ordinary `gate.check` path below, with
        // no marker distinguishing "a rule forced this" from an ordinary
        // first-time ask -- `PermissionDecisionKind` (`#[non_exhaustive]`,
        // so additive) is NOT extended by this item. That is deliberately
        // narrower than it could be, not an oversight: the hazard this
        // item's own acceptance criteria warn against is a NEW cause
        // silently reported as `Cached` (the cache/pattern/`AutoAllow` steps'
        // own label for "resolved without asking"), and this step cannot
        // produce that mislabeling BY CONSTRUCTION -- setting
        // `must_reach_gate` only ever routes a call INTO `gate.check`, whose
        // real `PermissionDecision` (`AllowOnce`/`AllowAlways`/`Denied`/
        // `DeniedWithFeedback`) is reported exactly as it already is for any
        // other first-time ask. What is genuinely missing is WHY the operator
        // is being asked -- surfacing "matched plugin rule `bash:curl`" in
        // the prompt UI needs a wire-visible field on `PermissionRequest`/
        // `Event::PermissionRequested`, a persisted-log-compatible change
        // (`#[serde(default)]`, mirroring `Event::AgentSpawned`'s `ephemeral`
        // field) this item leaves as a follow-up rather than bundling into
        // the mechanism fix.
        if self.prompt_matches(ctx, call).is_some() {
            must_reach_gate = true;
        }

        if !must_reach_gate {
            if self.cached_grant_covers(&key, ctx) {
                self.emit(
                    ctx,
                    Event::PermissionResolved {
                        call_id: call.call_id.clone(),
                        decision: PermissionDecisionKind::Cached,
                    },
                );
                return PermissionOutcome::Allow;
            }

            // A pattern grant spares the operator a prompt -- but only for a
            // command that clears the metacharacter gate inside
            // `PatternRule::matches_render` (consulted only when the call's
            // own `RenderKind` says it could reach a shell). A chained
            // shell command falls through to the gate below no matter what
            // patterns exist.
            if self.pattern_allows(ctx, call) {
                self.emit(
                    ctx,
                    Event::PermissionResolved {
                        call_id: call.call_id.clone(),
                        decision: PermissionDecisionKind::Cached,
                    },
                );
                return PermissionOutcome::Allow;
            }

            if mode == PermissionMode::AutoAllow {
                self.emit(
                    ctx,
                    Event::PermissionResolved {
                        call_id: call.call_id.clone(),
                        decision: PermissionDecisionKind::Cached,
                    },
                );
                return PermissionOutcome::Allow;
            }
        }

        let request = PermissionRequest {
            agent_id: ctx.agent_id,
            agent_path: ctx.agent_path.clone(),
            tool: call.tool.clone(),
            category: call.category,
            arguments: call.arguments.clone(),
            rendered: call.rendered.clone(),
            call_id: call.call_id.clone(),
            // The gate's prompt needs the SAME declaration `matches_render`
            // just evaluated above (a second lookup could disagree with the
            // value that decided the gate check): whether `rendered` is a
            // shell command decides what a pattern offer may honestly
            // propose (`suggested_rule`).
            render_kind: call.render_kind,
        };
        let decision = self.gate.check(request).await;

        if let PermissionDecision::AllowAlways { scope } = &decision {
            self.remember(key, *scope, ctx.agent_id);
        }

        let kind = PermissionDecisionKind::from(&decision);
        self.emit(
            ctx,
            Event::PermissionResolved {
                call_id: call.call_id.clone(),
                decision: kind,
            },
        );

        PermissionOutcome::from(decision)
    }

    fn cached_grant_covers(&self, key: &CacheKey, ctx: &PermissionCtx) -> bool {
        let cache = self.cache.read().expect("permission cache poisoned");
        cache
            .get(key)
            .is_some_and(|grants| grants.iter().any(|grant| grant.covers(ctx)))
    }

    fn remember(&self, key: CacheKey, scope: PermissionScope, granting_agent: AgentId) {
        let grant = grant_scope_for(scope, granting_agent);
        let mut cache = self.cache.write().expect("permission cache poisoned");
        let grants = cache.entry(key).or_default();
        // Dedup on insert: concurrent decide() races on the same key may
        // both reach here; duplicate grants are harmless but accumulate
        // (cycle-1 review M2).
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }

    fn emit(&self, ctx: &PermissionCtx, event: Event) {
        self.bus.emit(ctx.session, ctx.agent_id, event);
    }
}

/// Board item 01KZS00JP5QNBJSSHNFP9C47GM: the `pre_tool_use` hook step's own
/// tests. Inline (not `tests/permission_broker.rs`) so `cargo test -p
/// conway-runtime permission::` -- this item's own verification anchor --
/// finds them by module path.
///
/// The acceptance criteria this module proves, one test each: a denying
/// hook is enforced under `AutoAllow` (the failure this item's whole
/// placement analysis exists to prevent); the same beats a cached
/// `AllowAlways` grant and a matching pattern-allow rule (the other two
/// bypass paths a downstream-of-`gate.check` implementation would have
/// missed); a missing/failing/malformed-output hook denies via the
/// runner's own failure signal, not a second fail-closed implementation;
/// with nothing installed, `decide()` is unchanged (proving this is
/// additive); and no JSON shape a hook can send ever produces `Allow`.
#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use conway_core::agent::PermissionRequest;
    use conway_core::error::HookFailure;
    use conway_core::hook::HookAnswer;
    use conway_core::permission_pattern::{PatternOrigin, PatternRule};

    use super::*;

    /// A gate that records every call and always grants `AllowOnce` --
    /// installed in every test below that is not itself testing the gate,
    /// so a test asserting `call_count() == 0` is asserting the hook step
    /// stopped the call from ever reaching it, not merely that this
    /// particular double happened to deny.
    struct RecordingGate {
        calls: Mutex<u32>,
    }

    impl RecordingGate {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(0),
            })
        }

        fn call_count(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl PermissionGate for RecordingGate {
        async fn check(&self, _req: PermissionRequest) -> PermissionDecision {
            *self.calls.lock().unwrap() += 1;
            PermissionDecision::AllowOnce
        }
    }

    /// A gate that always grants `AllowAlways { scope: Session }` -- used
    /// only by the cache-bypass test, to populate a real cache entry the
    /// hook step must still beat.
    struct AllowAlwaysGate;

    #[async_trait]
    impl PermissionGate for AllowAlwaysGate {
        async fn check(&self, _req: PermissionRequest) -> PermissionDecision {
            PermissionDecision::AllowAlways {
                scope: PermissionScope::Session,
            }
        }
    }

    /// A `HookRunner` double that plays back one fixed answer (or failure)
    /// for every call, recording how many times it was invoked. This is the
    /// double every test in this module drives `Self::pre_tool_use_hook_
    /// denial` through -- there is no real process spawn anywhere in this
    /// module (that is `conway-tools`' `ProcessHookRunner`'s own test
    /// suite's job).
    struct ScriptedHookRunner {
        result: Result<HookAnswer, HookFailure>,
        calls: Mutex<u32>,
    }

    impl ScriptedHookRunner {
        fn no_opinion() -> Arc<Self> {
            Self::answer(HookAnswer::default())
        }

        fn deny(reason: &str) -> Arc<Self> {
            Self::answer(HookAnswer {
                permission: HookPermissionVerdict::Deny {
                    reason: reason.to_string(),
                },
                ..HookAnswer::default()
            })
        }

        fn answer(answer: HookAnswer) -> Arc<Self> {
            Arc::new(Self {
                result: Ok(answer),
                calls: Mutex::new(0),
            })
        }

        fn failing(failure: HookFailure) -> Arc<Self> {
            Arc::new(Self {
                result: Err(failure),
                calls: Mutex::new(0),
            })
        }

        fn call_count(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl HookRunner for ScriptedHookRunner {
        async fn run(&self, _invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
            *self.calls.lock().unwrap() += 1;
            self.result.clone()
        }
    }

    fn hook_spec(id: &str) -> PreToolUseHookSpec {
        PreToolUseHookSpec {
            id: id.to_string(),
            command: vec!["/usr/bin/env".to_string(), "true".to_string()],
            timeout_ms: 1_000,
        }
    }

    fn test_ctx(agent_id: AgentId, session: SessionId) -> PermissionCtx {
        PermissionCtx {
            agent_id,
            agent_path: vec![agent_id],
            session,
            cwd: PathBuf::from("/tmp"),
            root: AgentRoot::Unconfined,
        }
    }

    fn bash_call(call_id: &str, rendered: &str) -> AuthorizedCall {
        AuthorizedCall {
            call_id: call_id.into(),
            tool: ToolName::new("bash"),
            category: ToolCategory::Execute,
            arguments: serde_json::json!({ "command": rendered }),
            rendered: rendered.into(),
            path_args: PathArgs::Unconfinable {
                checkable: &["cwd"],
            },
            render_kind: RenderKind::ShellCommand,
        }
    }

    /// **With no hook runner and no hooks installed, `decide()` is
    /// unchanged.** The gate is consulted, allows, and nothing about the
    /// new hook step is even reachable -- proving the step is a true no-op
    /// absent configuration, the same claim `tests/permission_broker.rs`'s
    /// full pre-existing suite (run unmodified) proves at the crate's own
    /// integration-test layer.
    #[tokio::test]
    async fn decide_is_unchanged_when_no_hook_runner_is_installed() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert_eq!(outcome, PermissionOutcome::Allow);
        assert_eq!(gate.call_count(), 1);
    }

    /// **The single most important test in this item.** `AutoAllow` mode
    /// plus a denying `pre_tool_use` hook: the call is STILL denied, and
    /// the operator's gate is never consulted -- the exact failure this
    /// item's placement analysis exists to prevent. A hook implemented
    /// downstream of `gate.check` would never even run under `AutoAllow`;
    /// this asserts the opposite is true here.
    #[tokio::test]
    async fn denying_hook_blocks_the_call_even_in_autoallow_mode() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        let runner = ScriptedHookRunner::deny("touches a path this hook refuses");
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "rm -rf /")).await;

        match outcome {
            PermissionOutcome::Deny { rendered_error } => {
                assert!(
                    rendered_error.contains("guard"),
                    "denial must name which hook refused: {rendered_error}"
                );
                assert!(
                    rendered_error.contains("touches a path this hook refuses"),
                    "denial must carry the hook's own reason: {rendered_error}"
                );
            }
            PermissionOutcome::Allow => {
                panic!("AutoAllow must not bypass a denying pre_tool_use hook")
            }
        }
        assert_eq!(
            gate.call_count(),
            0,
            "the operator's gate must never be consulted for a hook-denied call"
        );
        assert_eq!(runner.call_count(), 1);
    }

    /// **Bypass path 1 of 2: a cached `AllowAlways` grant.** A hook
    /// installed AFTER a grant is already cached must still deny the next
    /// identical call -- proving the hook step sits above the cache lookup,
    /// not merely above the gate.
    #[tokio::test]
    async fn denying_hook_blocks_a_call_a_cached_allow_always_grant_would_otherwise_allow() {
        let broker = PermissionBroker::new(Arc::new(AllowAlwaysGate), EventBus::new(64));
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        // First call: no hook installed yet -- the gate grants `AllowAlways`
        // and the broker caches it.
        let first = broker.decide(&ctx, &bash_call("c1", "git status")).await;
        assert_eq!(first, PermissionOutcome::Allow);

        // Install a denying hook only now, after the grant is cached.
        let runner = ScriptedHookRunner::deny("blocked after the grant was cached");
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);

        // Second call: identical tool + arguments, which WOULD hit the
        // cache built by the first call.
        let second = broker.decide(&ctx, &bash_call("c2", "git status")).await;

        match second {
            PermissionOutcome::Deny { .. } => {}
            PermissionOutcome::Allow => {
                panic!("a denying pre_tool_use hook must beat a cached AllowAlways grant")
            }
        }
        assert_eq!(runner.call_count(), 1);
    }

    /// **Bypass path 2 of 2: a matching pattern-allow rule.** A pattern
    /// grant that would ordinarily spare the operator a prompt (and the
    /// hook step's own gate check further down `decide()`) must not spare
    /// it from a denying hook either.
    #[tokio::test]
    async fn denying_hook_blocks_a_call_a_matching_pattern_allow_rule_would_otherwise_allow() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        broker.remember_pattern(
            PatternRule::parse("bash:git status").expect("valid rule"),
            PermissionScope::Session,
            agent,
            PatternOrigin::Interactive,
        );
        let runner = ScriptedHookRunner::deny("blocked despite a matching pattern");
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        match outcome {
            PermissionOutcome::Deny { .. } => {}
            PermissionOutcome::Allow => {
                panic!("a denying pre_tool_use hook must beat a matching pattern-allow rule")
            }
        }
        assert_eq!(
            gate.call_count(),
            0,
            "the operator's gate must never be consulted for a hook-denied call"
        );
        assert_eq!(runner.call_count(), 1);
    }

    /// **Fail-closed: a missing/unexecutable command.** The runner's own
    /// `Spawn` failure denies the call -- not a second, weaker fail-closed
    /// implementation layered on top of the runner's contract.
    #[tokio::test]
    async fn a_hook_that_fails_to_spawn_denies_the_call() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = ScriptedHookRunner::failing(HookFailure::Spawn {
            detail: "no such file or directory".to_string(),
        });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "a hook that fails to spawn must deny, not silently proceed: {outcome:?}"
        );
        assert_eq!(gate.call_count(), 0);
    }

    /// **Fail-closed: a timed-out hook.** Same contract as the spawn
    /// failure above, exercised against the runner's other named failure
    /// mode.
    #[tokio::test]
    async fn a_hook_that_times_out_denies_the_call() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = ScriptedHookRunner::failing(HookFailure::TimedOut { after_ms: 5_000 });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "a hook that times out must deny, not silently proceed: {outcome:?}"
        );
        assert_eq!(gate.call_count(), 0);
    }

    /// **Fail-closed: malformed stdout.** The runner reports this as
    /// `HookFailure::UnparseableAnswer` (its own parse rule, exercised for
    /// real in `conway-tools`' test suite); this test proves `decide()`
    /// treats that failure signal as a denial exactly like every other one,
    /// via the SAME code path (P-15: proven by the observable outcome, not
    /// by asserting a config field defaulted).
    #[tokio::test]
    async fn a_hook_with_malformed_output_denies_the_call() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = ScriptedHookRunner::failing(HookFailure::UnparseableAnswer {
            detail: "not valid JSON".to_string(),
        });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "a hook with malformed output must deny, not silently proceed: {outcome:?}"
        );
        assert_eq!(gate.call_count(), 0);
    }

    /// A hook with `NoOpinion` (the default answer) does not deny -- the
    /// call proceeds through the rest of `decide()` exactly as if no hook
    /// existed. Paired with the `Allow`-shaped-JSON test below, this shows
    /// the FULL range of what an installed hook can do: nothing, or deny --
    /// never grant.
    #[tokio::test]
    async fn a_hook_with_no_opinion_does_not_block_the_call() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = ScriptedHookRunner::no_opinion();
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert_eq!(outcome, PermissionOutcome::Allow);
        assert_eq!(gate.call_count(), 1);
        assert_eq!(runner.call_count(), 1);
    }

    /// **No hook answer can produce `Allow` on its own -- proven at the
    /// wire boundary.** A stdout payload a hostile or buggy hook script
    /// shaped to *look* like a grant (`{"permission":{"allow":true}}`) has
    /// no `HookPermissionVerdict` variant to decode into: parsing
    /// `HookAnswer` from it is a hard error (unlike `HookAnswer`'s OWN
    /// unknown-key leniency -- see `conway_core::hook`'s
    /// `an_unknown_replace_shaped_key_is_ignored...` test for the contrast
    /// -- an externally-tagged enum's tag key must name a real variant),
    /// which `conway_tools::hook_runner::ProcessHookRunner::parse_answer`
    /// turns into `HookFailure::UnparseableAnswer` in production -- i.e.
    /// fail-closed, the OPPOSITE of what such a script would need for this
    /// to work (`a_hook_with_malformed_output_denies_the_call`, above,
    /// proves `decide()`'s side of that fail-closed chain). Together with
    /// `conway_core::hook`'s own type-level proof
    /// (`no_json_shape_decodes_to_an_allow_because_no_allow_variant_exists`)
    /// and `a_hook_with_no_opinion_does_not_block_the_call` above (the one
    /// verdict a hook CAN produce that isn't a denial merely falls through
    /// to the rest of `decide()`, never fabricating an `Allow` itself),
    /// this closes the loop: no JSON a hook script can write, and no code
    /// path in this broker, ever turns a hook's answer into a grant.
    #[test]
    fn an_allow_shaped_hook_answer_fails_to_parse_rather_than_decoding_to_anything() {
        let result: Result<HookAnswer, _> = serde_json::from_value(serde_json::json!({
            "permission": {"allow": true},
        }));
        assert!(
            result.is_err(),
            "an 'allow'-shaped permission payload must not parse as any HookPermissionVerdict \
             variant"
        );
    }
}

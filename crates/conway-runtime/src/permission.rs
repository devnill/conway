//! `PermissionBroker`: a per-session decision cache layered over the
//! consumer's [`PermissionGate`] (architecture §4.3).
//!
//! The broker normalizes whatever the gate decides into a
//! [`PermissionOutcome`] the tool runner can act on directly, and
//! it owns the `AllowAlways` cache so a consumer answering "allow always"
//! is only ever asked once per scope. It never imposes a timeout on the
//! gate: architecture §8 requires the runtime to hold a pending call open
//! for as long as the gate takes to answer.

use std::collections::HashMap;

use conway_core::permission_mode::PermissionMode;
use conway_core::permission_pattern::{
    ArgsMatchSpec, PatternOrigin, PatternRule, Rule, Then, When,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use conway_core::agent::{
    PermissionDecision, PermissionDecisionKind, PermissionRequest, PermissionScope,
};
use conway_core::canon::canonical_json_bytes;
use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::content::ToolCategory;
use conway_core::event::Event;
use conway_core::hook::{
    HookEvent, HookInvocation, HookOnFailure, HookOrigin, HookPermissionVerdict,
};
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{HookRunner, PathArgs, PermissionGate, RenderKind};

use crate::events::EventBus;

/// The already-prefixed per-agent plugin-config key `conway_tools::fs`'s
/// `FsPlugin` reads its own confinement root under
/// (`conway_tools::fs::mod`'s own `FULL_ROOT_CONFIG_KEY`, restated here
/// because `conway-runtime -> conway-tools` would be a new, backward
/// crate-layering edge this crate must not gain just for a string
/// constant -- see that constant's own doc for the identical note in the
/// other direction). `runtime::root::start_root` and `subagent::
/// SubagentHost::start` both derive an entry under this key from the SAME
/// `AgentRoot`/`SessionMeta.root`/`SubagentSpec.root` value they already
/// resolve and validate for the (unrelated, still-live) artifact-writer
/// confinement path (`crate::artifact_store::AgentArtifactWriter`) -- see
/// [`derive_fs_root_config`]'s own doc for why this is a DERIVATION from an
/// already-validated value, never a second validation.
pub(crate) const CONWAY_FS_ROOT_CONFIG_KEY: &str = "conway.fs.root";

/// Merges a `conway.fs.root` entry -- derived from `root` (this agent's
/// OWN already-canonicalized confinement root, exactly the value
/// `AgentRoot`/`SessionMeta.root`/`SubagentSpec.root` already carry) -- into
/// `requested` (whatever per-agent `PluginConfig` values a caller already
/// asked for), and returns the merged map. `None` for `root` leaves
/// `requested` untouched (an unconfined agent gets no `conway.fs.root`
/// entry, exactly as if this function were never called).
///
/// **This is what makes `--root`/`ConwayBuilder::with_root` (and a spawned
/// child's `SubagentSpec::root`) still confine ordinary `read`/`write`/
/// `edit`/`cd`/`glob`/`grep` calls after the retirement.** Before
/// (`PermissionBroker::check_root`'s
/// per-tool `PathArgs::Named` walk retired), `root` alone was sufficient --
/// the harness checked every declared path argument against it directly,
/// and nothing needed to tell `conway.fs` anything. Now `conway.fs`
/// enforces its OWN root, read from PER-AGENT PLUGIN CONFIG
/// (`conway_core::ports::Plugin::narrowable_keys`), which `root`
/// alone does not populate -- without this derivation, an operator's
/// `--root` (or a caller's `SubagentSpec::root`) would keep confining
/// artifact writes (the OTHER, still-live consumer of `AgentRoot`) while
/// SILENTLY no longer confining any ordinary tool call at all, which is
/// exactly the regression this item's own security preamble forbids.
///
/// **A derivation, not a second validation.** `root` reaching this function
/// has ALREADY been resolved, canonicalized, and (for a spawned child)
/// narrowing-checked against its parent by the SAME caller that is about to
/// use it for `SessionMeta.root`/`AgentLoop.root` -- this function trusts
/// that work completely and does not repeat it. If `requested` already
/// names [`CONWAY_FS_ROOT_CONFIG_KEY`] explicitly (a caller that set
/// `conway.fs.root` directly via `SubagentSpec::plugin_config`, independent
/// of `SubagentSpec::root`), that explicit value is kept: this function
/// only FILLS IN the key when the caller left it unset, never overrides an
/// explicit choice. The result is NOT itself narrowing-validated here --
/// the caller's own `PluginConfig::narrow` call (already made, for
/// unrelated reasons, at every call site this function has) validates the
/// WHOLE merged map, including this entry, exactly like every other key.
///
/// **`fs_root_is_narrowable` gates the derivation entirely.** A `Runtime`
/// with no `conway.fs`-shaped plugin installed at all (a `conway-runtime`
/// test fixture using a minimal registry, or a real embedder who never
/// installs `FsPlugin`) has nothing declaring [`CONWAY_FS_ROOT_CONFIG_KEY`]
/// narrowable -- `PluginConfig::narrow` would refuse the WHOLE spawn/start
/// outright for a key nothing recognizes, turning a caller's unrelated
/// `--root`/`SubagentSpec::root` (whose ONLY other consumer,
/// `AgentArtifactWriter`, needs no plugin at all) into a hard failure it
/// never asked for. The caller passes whether ITS OWN currently-installed
/// plugin set actually declares the key (`PluginRegistry::narrowing_rules`)
/// -- `false` skips the derivation entirely (byte-for-byte the pre-this-
/// function behavior: `root` still confines the artifact-writer path,
/// simply does not reach a plugin that isn't there to be reached).
pub(crate) fn derive_fs_root_config(
    root: Option<&Path>,
    requested: Option<&conway_core::ports::PluginConfig>,
    fs_root_is_narrowable: bool,
) -> Option<conway_core::ports::PluginConfig> {
    let Some(root) = root else {
        return requested.cloned();
    };
    if !fs_root_is_narrowable {
        return requested.cloned();
    }
    let mut values = requested
        .map(|config| config.values.clone())
        .unwrap_or_default();
    values
        .entry(CONWAY_FS_ROOT_CONFIG_KEY.to_string())
        .or_insert_with(|| serde_json::json!(root.display().to_string()));
    Some(conway_core::ports::PluginConfig { values })
}

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
    /// S5: the resolved tool's own
    /// [`Tool::path_args`](conway_core::ports::Tool::path_args) declaration,
    /// read straight from the resolved tool instance at the same call site that
    /// already produces `rendered` (`ToolRunner:: execute_one`) — a plain,
    /// static, `'static`-lifetime enum copy, no I/O and no re-resolution of the
    /// tool by name. This is how the broker's decision point (which has no
    /// `PluginRegistry` access, and must not gain one just for this) learns
    /// which of `arguments`' fields carry filesystem paths without duplicating
    /// tool resolution.
    pub path_args: PathArgs,
    /// The resolved tool's own
    /// [`Tool::render_kind`](conway_core::ports::Tool::render_kind)
    /// declaration, read at the identical call site and for the identical
    /// reason as `path_args` above -- fed to [`PatternRule::matches_render`],
    /// which now refuses EVERY pattern grant outright when this is
    /// [`RenderKind::ShellCommand`] (see `conway_core::permission_pattern`'s
    /// own module doc).
    pub render_kind: RenderKind,
}

/// One `pre_tool_use` hook [`PermissionBroker::decide`] consults. Installed via
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
    /// The rule's `HookEntry::match_tool` , carried through untouched. `None` (the
    /// config default) consults this hook for every `pre_tool_use` call,
    /// unchanged from before this field existed -- see
    /// [`crate::hook_dispatch::HookSpec::matcher`]'s own doc for the
    /// identical rule applied to the observation tier's `post_tool_use`.
    /// `pre_tool_use` always carries a tool name (`AuthorizedCall::tool`),
    /// so unlike that sibling field there is no "payload with no tool"
    /// case to defend against here.
    pub matcher: Option<String>,
    /// This registration's own `on_failure` policy -- what
    /// `PermissionBroker::pre_tool_use_hook_denial` does when THIS hook's
    /// `HookRunner::run` call itself fails (a missing script, a timeout, or
    /// stdout that failed to parse), never consulted for a hook that ran to
    /// completion and returned an explicit
    /// [`HookPermissionVerdict::Deny`] -- see [`HookOnFailure`]'s own doc
    /// for why those are two different facts. Defaults to
    /// [`HookOnFailure::Deny`] via `HookEntry::on_failure`'s own
    /// `#[serde(default)]`, so an existing `[hooks].rules[]` entry that
    /// never sets `on_failure` denies on outage exactly as it always did.
    pub on_failure: HookOnFailure,
    /// Where this rule came from -- an operator's own merged
    /// `[hooks].rules[]` entry, or an installed plugin's own
    /// `conway_core::ports::Plugin::hooks()` declaration (board item
    /// `01M129QW0GV90QTQS6B3BY3DAR`). Defaults to [`HookOrigin::Operator`]
    /// (`HookOrigin`'s own `Default` impl), so every construction site
    /// that predates this field and never sets it explicitly keeps
    /// reporting exactly what it always implicitly was. Read by
    /// `crates/conway/src/conway.rs`'s
    /// `Conway::active_deny_capable_hook_rules` to report a plugin-
    /// contributed rule's real source rather than the blanket
    /// "settings.json (merged config)" label every rule used to get
    /// unconditionally -- see [`HookOrigin`]'s own doc for the full
    /// argument.
    pub origin: HookOrigin,
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
/// actually be resolved once it reaches the tool: `raw` beginning with
/// exactly `~` or a leading `~/` expands against the process's home
/// directory; any other absolute input passes through unchanged; a
/// relative input joins onto `cwd`. `Err` for a path containing a NUL byte,
/// or one beginning with `~` in a form this crate does not expand -- see
/// [`conway_core::containment::ResolveError`].
///
/// **A thin, same-crate wrapper around the one shared implementation,
/// [`conway_core::containment::resolve_candidate`]
///.** This function used to carry its own
/// restated copy of the resolution rule (kept in sync with `conway_tools::
/// common::resolve_path` only by a doc comment demanding lockstep edits,
/// never enforced by the compiler); it is now a direct call, so the two
/// crates' wrappers cannot independently drift or independently drop the
/// NUL guard the way two inlined copies already did once -- and cannot
/// independently drift on tilde expansion either (board item
/// `01M10HSENWKTEE4G691XJXBH6T`): a `paths_under` permission-rule prefix
/// and the call argument it is meant to bound both resolve through THIS
/// function, so a `~`-prefixed rule and a `~`-prefixed argument can never
/// expand two different ways (P-13). It still cannot simply BE
/// `resolve_path`: crate layering runs `conway-tools -> conway-core` and
/// `conway-runtime -> conway-core` only, never `conway-runtime ->
/// conway-tools`, so this crate must keep its own `pub(crate)` entry point
/// into the shared core function rather than gaining a dependency on
/// `conway-tools` just for this.
///
/// `pub(crate)` so the crate's OTHER path-resolution consumers -- the
/// spawn-time confinement-root resolution in `subagent.rs` and `runtime.rs` --
/// call THIS one rule -- one implementation, never restated (Min-1
///) instead of inlining "absolute -> as-is, relative
/// -> join cwd" and silently dropping the NUL guard, as both did until that
/// item.
pub(crate) fn resolve_like_the_tool_will(
    cwd: &Path,
    raw: &str,
) -> Result<PathBuf, conway_core::containment::ResolveError> {
    conway_core::containment::resolve_candidate(cwd, raw)
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
/// arguments with (one resolution rule, not a third copy) -- never
/// against the process's cwd, which is what a bare `Path::canonicalize`
/// would use. The process cwd is wherever the operator happened to launch
/// conway from and has no relationship to the project the rule was written
/// to protect, so a relative prefix resolved there confers a boundary the
/// operator did not write (finding S5). `base` is an explicit parameter
/// supplied by the caller (the facade's permission-file loader passes the
/// PROJECT root for a project file, the agent cwd for the global file); the
/// broker deliberately never reads `std::env::current_dir()` here, which
/// would recreate the bug one level down. An ABSOLUTE prefix is unaffected
/// by `base` (it passes through `resolve_like_the_tool_will` unchanged); a
/// `~`/`~/`-prefixed one expands against the process's home directory
/// instead of `base` (board item `01M10HSENWKTEE4G691XJXBH6T`) -- the same
/// resolution a tool's own path argument gets, so an operator who writes
/// `~/notes` in BOTH a `paths_under` rule and a `read` call sees the two
/// agree (P-13). A prefix containing a NUL byte, or one beginning with `~`
/// in a form this crate does not expand, fails closed exactly like one that
/// does not canonicalize.
fn canonicalize_when(when: &When, base: &Path) -> Option<CanonicalRoot> {
    match when {
        When::PathsUnder(prefix) => {
            let resolved = resolve_like_the_tool_will(base, prefix).ok()?;
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
                let Ok(resolved) = resolve_like_the_tool_will(&ctx.cwd, raw) else {
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
            // `ShellCommand` tool can never be auto-allowed by a pattern
            // grant at all, even when its `checkable` paths are under the
            // rule's prefix (trap 4: the gate must not weaken). `gate_allows`
            // reads only `call.render_kind` -- never `call.rendered` -- see
            // `Rule::gate_allows`'s own doc in `conway-core`.
            if !Rule::gate_allows(call.render_kind) {
                return false;
            }
            let Some(root) = canonical else {
                return false;
            };
            paths_under_match(ctx, call, root)
        }
        // Structured argument-field equality. Lives here (not in
        // `matches_allow_render`) for the same reason `PathsUnder` does: it
        // needs `call.arguments`, which that fn's signature does not carry.
        // The gate applies first -- a `ShellCommand` tool is refused for
        // free, identical to every other allow `when`, so an `ArgsMatch`
        // grant on `bash` authorizes nothing (same posture the AMENDED
        // section guarantees for `CommandPrefix`/`Always`).
        When::ArgsMatch(spec) => {
            if !rule.select_matches(call.tool.as_str(), call.category) {
                return false;
            }
            if !Rule::gate_allows(call.render_kind) {
                return false;
            }
            args_match(spec, &call.arguments)
        }
        _ => rule.matches_allow_render(
            call.tool.as_str(),
            call.category,
            &call.rendered,
            call.render_kind,
        ),
    }
}

/// Whether `call_args` satisfies an [`ArgsMatchSpec`]: every pinned field
/// must be present and equal its expected value under canonical JSON, and
/// fields not in the spec are wildcard (don't care). A pinned field absent
/// from the call is a non-match. An empty `pinned` matches every call -- the
/// `tool:*` equivalent. Equality goes through [`canonical_json_bytes`] so
/// object key order and insignificant whitespace never cause a miss (the
/// same primitive `CacheKey::for_call` hashes the whole args object with).
fn args_match(spec: &ArgsMatchSpec, call_args: &serde_json::Value) -> bool {
    let Some(obj) = call_args.as_object() else {
        // A non-object call can satisfy no pinned field, so it only matches
        // the empty-spec (any-call) case -- which `spec.pinned.is_empty()`
        // handles below. A non-object with a non-empty spec: no match.
        return spec.pinned.is_empty();
    };
    for (field, expected) in &spec.pinned {
        let Some(actual) = obj.get(field) else {
            return false;
        };
        if canonical_json_bytes(actual) != canonical_json_bytes(expected) {
            return false;
        }
    }
    true
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
            // trailing-`*` wildcard `Select::Tools` (no tool is named `*`). For
            // an `Unconfinable` tool (e.g. `bash`) `paths_under_match` returns
            // `false` -- correct for ALLOW (fail-closed: don't auto-allow) but
            // fail-OPEN for deny/prompt: the operator wrote a deny rule
            // expecting the call to be refused, and silently NOT matching it
            // lets the call through. Mirror `check_root`'s `Unconfinable {
            // checkable }` posture -- a tool the broker cannot statically
            // confine can never be PROVEN to be outside the prefix either -- so
            // the deny/prompt rule MATCHES (fail-toward- deny -- a narrowing
            // rule fails closed). The install-time `PathsUnderOnUnconfinedTool`
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

/// The facade boundary conversion (Stage 2b,
/// board item `01KZVYZM7BZRQ54RRB8P814KV9`): `conway::Conway`'s public API
/// (`active_structured_allow_rules`, `revoke_structured_allow_rule`) carries
/// `conway_core::agent::GrantScope`, never this crate's own `GrantScope`,
/// so the facade re-exports no `conway-runtime` type. This crate's internal
/// `GrantScope` stays exactly as it was -- the broker's cache/store
/// machinery and `covers()` (which needs the runtime-only `PermissionCtx`)
/// are unaffected -- and converts at its own edge instead.
impl From<GrantScope> for conway_core::agent::GrantScope {
    fn from(scope: GrantScope) -> Self {
        match scope {
            GrantScope::Session => conway_core::agent::GrantScope::Session,
            GrantScope::Agent(id) => conway_core::agent::GrantScope::Agent(id),
            GrantScope::Subtree(id) => conway_core::agent::GrantScope::Subtree(id),
        }
    }
}

/// The reverse of the `From<GrantScope> for conway_core::agent::GrantScope`
/// conversion above -- `Conway::revoke_structured_allow_rule` receives the
/// facade-level scope from a caller and must convert it back to address
/// `PermissionBroker::revoke_pattern_rule`, which still keys on this crate's
/// own `GrantScope`.
impl From<conway_core::agent::GrantScope> for GrantScope {
    fn from(scope: conway_core::agent::GrantScope) -> Self {
        match scope {
            conway_core::agent::GrantScope::Session => GrantScope::Session,
            conway_core::agent::GrantScope::Agent(id) => GrantScope::Agent(id),
            conway_core::agent::GrantScope::Subtree(id) => GrantScope::Subtree(id),
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
    /// matching pattern spares the operator a prompt -- but never for a
    /// call whose tool declares `RenderKind::ShellCommand` (e.g. `bash`):
    /// `PatternRule::matches_render` refuses those outright, so no pattern
    /// installed here can ever cover one -- see `conway_core::
    /// permission_pattern`'s own module doc.
    ///: the CALLER (`conway`'s facade,
    /// `conway-cli`'s startup loader) is responsible for confirming trust
    /// before an allow rule loaded from a project file ever reaches
    /// `remember_pattern` -- this broker has no file-trust concept of its
    /// own and does not need one; it only stores what it is told to.
    patterns: RwLock<AllowRuleStore>,
    /// Prefix-pattern DENY rules.
    /// Unlike `patterns` above, these carry no `GrantScope` -- a `deny`
    /// rule is D4 §3's asymmetric half, "applies immediately, trusted or
    /// not, from any file, to any requester," so it is checked in
    /// `Self::decide` for EVERY call regardless of who is asking. Matched
    /// via `PatternRule::matches_deny`, which deliberately does not consult
    /// `RenderKind` at all -- unlike `patterns` above, a deny rule matches a
    /// `ShellCommand` tool the same way it matches any other -- see that
    /// method's own doc.
    deny_patterns: RwLock<NarrowingRuleStore>,
    /// Prefix-pattern PROMPT rules --
    /// the second narrowing effect the extension design grants a plugin-contributed rule (`then: prompt`, alongside
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
    /// The injected `pre_tool_use`
    /// hook dispatcher. `None` (the default, and every caller before this
    /// field existed) means the hook-check step in `Self::decide` is a
    /// byte-for-byte no-op -- see [`Self::set_hook_runner`]'s own doc for
    /// the full "additive, not a new dependency" contract.
    hook_runner: RwLock<Option<Arc<dyn HookRunner>>>,
    /// The `[hooks].rules[]` entries
    /// (already filtered to `event == "pre_tool_use" && enabled` by the
    /// facade) `Self::decide`'s hook-check step consults, in installation
    /// order. Empty (the default) is the same no-op as `hook_runner` being
    /// `None` -- both must be populated for the step to do anything, and
    /// either alone is inert by construction (see
    /// [`Self::pre_tool_use_hook_denial`]).
    pre_tool_use_hooks: RwLock<Vec<PreToolUseHookSpec>>,
}

/// WHY a [`HookStepOutcome::Denied`] denies -- an explicit hook verdict, or
/// this hook's own outage resolved (by its `on_failure` policy) to `Deny`.
/// **This is the structural fix
/// (`docs/vision/DESIGN-permission-modes.md` §3a/§3c): the two used to be
/// the identical `Option<String>` value, distinguishable only by parsing
/// the rendered text for the trailing `-- fail-closed`.** Now a downstream
/// consumer -- a future status surface, or a test -- can match on `cause`
/// directly and never read `rendered_error` at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookDenialCause {
    /// A hook ran to completion and returned
    /// [`HookPermissionVerdict::Deny`] itself. The guard said no.
    Verdict,
    /// This hook's `HookRunner::run` call failed (a missing script, a
    /// timeout, or stdout that failed to parse), and its own `on_failure`
    /// policy is [`HookOnFailure::Deny`] (today's only behavior, and still
    /// the default). The guard is down.
    Outage,
}

/// The distinguishable outcome of consulting every INSTALLED `pre_tool_use`
/// hook once for one call -- [`PermissionBroker::pre_tool_use_hook_denial`]'s
/// own return type, `PermissionBroker::decide`'s only caller (see that
/// method's own doc for WHY this sits at the deny tier). Replaces a plain
/// `Option<String>`, whose collapse of a hook's own verdict and a hook's own
/// outage into the same value is exactly the defect [`HookDenialCause`]'s
/// own doc names.
#[derive(Clone, Debug, PartialEq, Eq)]
enum HookStepOutcome {
    /// No installed hook had anything to say about this call --
    /// `PermissionBroker::decide` proceeds exactly as if the hook step did
    /// not exist.
    NoOpinion,
    /// This call is refused outright, tagged with WHY (see
    /// [`HookDenialCause`]). `PermissionBroker::decide` returns
    /// [`PermissionOutcome::Deny`] for either `cause` identically -- the
    /// RENDERED effect is unchanged from before this type existed -- but
    /// the two are now different VALUES, not merely different substrings of
    /// one rendered message.
    Denied {
        rendered_error: String,
        cause: HookDenialCause,
    },
    /// A hook's runner failed and its `on_failure` policy resolved to
    /// [`HookOnFailure::Prompt`], with no HARDER denial (an explicit
    /// verdict, or another hook's `on_failure: Deny` outage) matched
    /// anywhere in the same pass. Not a denial: forces
    /// `PermissionBroker::decide`'s `must_reach_gate` accumulator exactly
    /// as `PermissionBroker::prompt_matches` already does, so the call
    /// proceeds to the operator's own `gate.check` -- never the cache, a
    /// pattern grant, or `AutoAllow`. `PermissionBroker::decide`'s existing
    /// step order (deny-pattern, then this step, then plan-mode) already
    /// places an operator `Deny` rule before this step and plan-mode's own
    /// denial after it -- both still apply unconditionally, so an
    /// `on_failure: Prompt` firing can only ever narrow, never bypass
    /// either (the subordination boundary
    /// `docs/vision/DESIGN-permission-modes.md` §3c requires).
    MustReachGate,
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
    /// every call to `Self::decide` consults at the deny tier
    ///. Mirrors `Runtime::set_context_hook`'s own
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
    ///. The facade computes this list
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

    /// Every currently-installed `pre_tool_use` hook spec, in dispatch order
    /// -- the review-list counterpart of [`Self::set_pre_tool_use_hooks`],
    /// mirroring [`Self::active_patterns`]'s own shape -- a hook that can silently deny a call is a
    /// permission rule, and an operator cannot revoke what they cannot see).
    /// A rule installs here regardless of whether its command actually
    /// resolves at invocation time, so a hook whose script is broken or
    /// missing -- and which is therefore currently denying every matching
    /// call, per `Self::pre_tool_use_hook_denial`'s fail-closed posture --
    /// still appears rather than being silently omitted; this method has no
    /// way to know a hook's script is broken without running it, and does
    /// not try.
    pub fn active_pre_tool_use_hooks(&self) -> Vec<PreToolUseHookSpec> {
        self.pre_tool_use_hooks
            .read()
            .expect("pre_tool_use hooks lock poisoned")
            .clone()
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
    /// for the review surface.
    ///
    /// Note this does NOT pre-validate the rule against the tool's
    /// `RenderKind` (e.g. reject a `bash:...` rule on sight):
    /// `rule` is desugared to a [`Rule`] immediately below (`to_rule`), and
    /// the gate lives in [`Rule::matches_allow_render`] -- the ONE evaluator
    /// `Self::pattern_allows` consults at decision time -- `PatternRule::matches_render` is never
    /// reached from here or anywhere else in this broker -- it is a public,
    /// test-facing convenience that itself now delegates to the same
    /// evaluator, not a second decision path). Filtering at creation time
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
    /// failure; the broker itself never panics on untrusted input.
    ///
    /// `base` is the directory a RELATIVE `paths_under` prefix resolves
    /// against (B2 -- see `canonicalize_when``s own doc for why this is an
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
        // production; it is encoded HERE, at the broker boundary, so a future
        // transport that reuses `PatternOrigin::Plugin` to call the allow path
        // with `Then::Allow` hits a STRUCTURAL refusal rather than silently
        // installing a durable grant the operator never authorized. The
        // invariant rests on a guard, not on the absence of a transport.
        // Untrusted input gives a typed `false` (the existing rejection shape
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
    /// (an earlier design item, D4 §3).
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

    /// Installs a PROMPT rule, attributed to `origin`.
    ///: the second narrowing effect
    /// the extension design grants a
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

    /// S5, NARROWED by (`AgentRoot`/`SubagentSpec::root`/`--root` confinement
    /// finally reachable, and the per-tool `PathArgs::Named` root walk
    /// retired). Evaluated before anything else in [`Self::decide`] — see
    /// that method's own doc for exactly why (structurally, this must
    /// precede every one of the four allow paths, not live inside
    /// `PermissionGate`).
    ///
    /// **This function no longer checks a `PathArgs::Named` tool's own
    /// declared path arguments at all -- that is now `conway.fs`'s job**
    /// (`conway_tools::fs::beneath`, wired into `read`/`write`/`edit`/`cd`/
    /// `glob`/`grep` before their I/O runs, open-relative, closing the
    /// TOCTOU gap this function's PREDECESSOR left open by checking
    /// `candidate` here and opening it in a SEPARATE step, in a different
    /// crate, after an `await` on the operator's own gate). Re-checking the
    /// same named arguments here, ahead of that plugin-level enforcement,
    /// would be exactly the "two implementations of one boundary" P-14
    /// exists to prevent -- see this crate's own permission module doc and
    /// `crates/conway/tests/root_containment_seam.rs`'s own module doc for
    /// the fuller accounting of what changed and why.
    ///
    /// **What this function still does, and why it cannot move to a
    /// plugin.** A `PathArgs::Unconfinable` call (`bash`'s own free-form
    /// shell `command`, most concretely) has no path a ROOT CHECK -- of any
    /// kind, harness- or plugin-level -- can statically confine; `bash`
    /// belongs to `conway.shell`, a DIFFERENT plugin than the one whose
    /// root might matter, so `conway.fs`'s own enforcement cannot reach it
    /// either. This is exactly the asymmetry GP-13 records: a harness-level
    /// root APPEARS to cover every tool while actually covering only those
    /// declaring path arguments. What this function still guarantees for
    /// that residual case is narrower than "confined": under a confined
    /// agent, an `Unconfinable` call is never silently auto-allowed by the
    /// cache, a pattern grant, or `AutoAllow` mode -- it is always forced to
    /// the operator's own `gate.check`, so a human (or whatever the
    /// operator's `PermissionGate` implementation is) gets a chance to see
    /// it. This is a GATE-ROUTING POLICY, not a containment check: it never
    /// inspects `call.arguments` for the pure-`Unconfinable` remainder
    /// (only for `checkable`, below), and it grants no guarantee that the
    /// command itself stays inside any root.
    ///
    /// `checkable` (the `Unconfinable` variant's OWN sub-list of arguments
    /// that ARE staticaly confinable, e.g. `bash`'s optional `cwd`) is still
    /// walked here exactly as before -- this is the OTHER half of the
    /// answer to "what did the harness cover that `conway.fs` does not":
    /// `bash`'s `cwd` belongs to a tool `conway.fs` has never had any
    /// jurisdiction over, so nothing but this function has ever confined
    /// it, and nothing else confines it now.
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

        // `None` and `Named` both proceed unconditionally now: `None` never
        // had anything to check, and `Named`'s own containment check moved
        // into the tool's own plugin (see this function's own doc) --
        // re-checking it here would be the retired second implementation.
        // `Unconfinable` is the one variant this function still has
        // jurisdiction over: it ALWAYS forces the gate under a root --
        // regardless of whether `checkable` is empty — because the part of
        // the call this broker (or any plugin) cannot statically confine
        // (e.g. `bash`'s `command`) can still reach outside the root;
        // `checkable` is checked here in addition, not instead.
        let (names, must_reach_gate): (&[&str], bool) = match call.path_args {
            PathArgs::None => return RootDecision::Proceed,
            PathArgs::Named(_) => return RootDecision::Proceed,
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
                    let resolved = match resolve_like_the_tool_will(&ctx.cwd, raw) {
                        Ok(resolved) => resolved,
                        Err(err) => {
                            return RootDecision::Denied(format!(
                                "`{}` argument `{name}` ({raw:?}) cannot be resolved to a \
                                 filesystem path: {err}",
                                call.tool.as_str()
                            ));
                        }
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
    /// item).
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
            .filter_map(|(rule, _, _, origin)| rule.to_pattern_rule().map(|p| (p, origin.clone())))
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
            .filter_map(|(rule, _, origin)| rule.to_pattern_rule().map(|p| (p, origin.clone())))
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
            .filter_map(|(rule, _, origin)| rule.to_pattern_rule().map(|p| (p, origin.clone())))
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
    /// in `active_patterns()`.
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
    /// gate lives in [`Rule::gate_allows`], applied for every `when` (not
    /// just `command_prefix`): a `RenderKind::ShellCommand` tool (e.g.
    /// `bash`) can never satisfy a pattern here at all, chained or not,
    /// regardless of what is installed -- see `conway_core::
    /// permission_pattern`'s own module doc for why a durable pattern grant
    /// no longer exists for a shell-rendered tool.
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
    /// An earlier design item, D4 §3: checked for EVERY
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
    /// ///
    /// **Deliberately reuses the deny/prompt evaluator (ungated), not
    /// `Rule::matches_allow_render`.** The allow-side `RenderKind` gate
    /// exists to keep an ALLOW from ever being satisfied for a
    /// `ShellCommand` tool -- a concern that only applies to a rule that
    /// GRANTS something. A `prompt` rule grants nothing; its only effect is
    /// "ask the operator instead of skipping the ask", which is safe
    /// (indeed MORE conservative) to fire for `bash` too -- a `prompt`
    /// rule targeting `bash` is exactly how an operator asks to be
    /// consulted on shell commands now that a durable `bash` ALLOW pattern
    /// no longer exists at all. Gating it the allow way would have the
    /// opposite of the intended effect: it would EVADE the extra scrutiny a
    /// `prompt` rule exists to add, exactly the inversion `PatternRule::
    /// matches_deny`'s own doc describes for `deny`. `prompt` and `deny` are
    /// both admitted unconditionally at extension-architecture.md §5.5's
    /// stage 1 (they narrow; only `allow` needs trust) precisely because
    /// neither can be evaded this way.
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

    /// The `pre_tool_use` hook-check step: consults every INSTALLED hook
    /// once for this call, returning the distinguishable [`HookStepOutcome`]
    /// (see that type's own doc for WHY this sits at the deny tier, and
    /// [`HookDenialCause`]'s own doc for the structural fix this replaces).
    ///
    /// **Deny/prompt-only, not open-ended -- decided, not left open.** A
    /// hook that WANTS a human decision rather than an outright refusal
    /// already has two existing ways to get one without this method growing
    /// an unbounded set of `must_reach_gate` sources of its own: an
    /// operator-installed `prompt` pattern rule (`Self::prompt_matches`,
    /// above) for a call shape a plugin author can identify statically, or
    /// the hook script itself simply choosing not to run in "blocking"
    /// mode. `HookPermissionVerdict` (a successfully-run hook's own answer
    /// type) still has no `Prompt` variant -- a hook's own VERDICT can only
    /// ever narrow to `Deny` or say nothing; only its OUTAGE, via
    /// `on_failure`, may narrow to `Prompt`, and only that one, fixed,
    /// per-registration knob.
    ///
    /// **Fail-closed still inherits from the runner by default, never
    /// re-implemented.** `HookRunner::run`'s `Err(HookFailure)` -- a missing
    /// script, a timeout, or stdout that failed to parse as a
    /// [`conway_core::hook::HookAnswer`] -- resolves through THIS hook's own
    /// `on_failure` policy, which defaults to [`HookOnFailure::Deny`]:
    /// unchanged from before this policy existed for every registration
    /// that does not set it. There is still no separate "is this hook
    /// broken" check layered on top that could disagree with the runner's
    /// own verdict; `on_failure` decides what to DO about that failure, it
    /// never second-guesses whether it happened.
    ///
    /// **A `Prompt`-resolved outage does not short-circuit the loop.** The
    /// remaining installed hooks are still consulted -- a LATER hook's
    /// explicit `Deny`, or another hook's OWN `on_failure: Deny` outage,
    /// must still win over an earlier hook's `on_failure: Prompt` outage,
    /// most-restrictive-wins, exactly the posture `Self::decide`'s own
    /// `must_reach_gate` accumulator already documents for `check_root`'s
    /// `MustReachGate` and `Self::prompt_matches`.
    ///
    /// Every `RwLock` this reads is acquired, cloned out of, and released
    /// BEFORE the only `.await` point below (`runner.run`) -- the same
    /// never-hold-a-lock-across-an-await invariant `Self::decide`'s own doc
    /// states for its cache lock.
    async fn pre_tool_use_hook_denial(
        &self,
        ctx: &PermissionCtx,
        call: &AuthorizedCall,
    ) -> HookStepOutcome {
        let Some(runner) = self
            .hook_runner
            .read()
            .expect("hook runner lock poisoned")
            .clone()
        else {
            return HookStepOutcome::NoOpinion;
        };
        let hooks = self
            .pre_tool_use_hooks
            .read()
            .expect("pre_tool_use hooks lock poisoned")
            .clone();
        if hooks.is_empty() {
            return HookStepOutcome::NoOpinion;
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

        // Accumulates an `on_failure: Prompt` outage across the loop -- see
        // `HookStepOutcome::MustReachGate`'s own doc for why this does not
        // return immediately: a later hook's explicit `Deny`, or another
        // hook's own `on_failure: Deny` outage, must still be able to win.
        let mut must_reach_gate = false;

        for hook in hooks.iter().filter(|hook| {
            // a matcher only
            // NARROWS which calls consult this hook -- absent (`None`) is
            // the pre-existing "fire for every call" behavior, unchanged.
            hook.matcher.as_deref().is_none_or(|pattern| {
                conway_core::hook::tool_matcher_matches(pattern, call.tool.as_str())
            })
        }) {
            let invocation = HookInvocation::new(
                hook.command.clone(),
                hook.timeout_ms,
                HookEvent::new("pre_tool_use", payload.clone()),
            );
            match runner.run(&invocation).await {
                // `HookPermissionVerdict::denies` is the single
                // implementation of what an unrecognized future variant
                // means (fail closed) -- see that method's own doc. This
                // is one of its two callers; the other is
                // `hook_dispatch::HookDispatcher::dispatch_deny_only`
                // (`prompt_submitted` and other deny-only events), which
                // now shares the identical judgment rather than
                // re-deriving it.
                Ok(answer) => {
                    if answer.permission.denies() {
                        // A hook's own VERDICT -- `on_failure` is never
                        // consulted here: it governs ONLY what happens when
                        // this hook's runner cannot be reached at all, never
                        // a hook that ran and had an opinion. An explicit
                        // `Deny` denies, full stop, regardless of this
                        // hook's `on_failure` setting. A recognized `Deny`
                        // reports its own `reason`; any other denying
                        // (i.e. non-`NoOpinion`) variant is one this build
                        // does not recognize, and is reported as such --
                        // an operator upgrading `conway` before every hook
                        // script it drives must never see calls silently
                        // start passing through a hook that used to guard
                        // them.
                        let reason = match &answer.permission {
                            HookPermissionVerdict::Deny { reason } => reason.clone(),
                            _ => "unrecognized permission verdict -- fail-closed".to_string(),
                        };
                        return HookStepOutcome::Denied {
                            rendered_error: format!(
                                "`{}` is denied by `pre_tool_use` hook `{}`: {reason}",
                                call.tool.as_str(),
                                hook.id
                            ),
                            cause: HookDenialCause::Verdict,
                        };
                    }
                    // `NoOpinion` (the only non-denying case `denies()`
                    // recognizes): this hook has nothing to say -- consult
                    // the next one, if any.
                }
                Err(failure) => match hook.on_failure {
                    HookOnFailure::Deny => {
                        return HookStepOutcome::Denied {
                            rendered_error: format!(
                                "`{}` is denied: `pre_tool_use` hook `{}` failed ({failure}) \
                                 -- fail-closed",
                                call.tool.as_str(),
                                hook.id
                            ),
                            cause: HookDenialCause::Outage,
                        };
                    }
                    HookOnFailure::Prompt => {
                        // Narrows only -- does not deny, does not
                        // short-circuit the remaining hooks. See this
                        // method's own doc.
                        must_reach_gate = true;
                    }
                    // `HookOnFailure` is `#[non_exhaustive]` too: an
                    // unrecognized outage policy fails closed exactly like
                    // its own documented default (`Deny`) -- an operator
                    // who set a since-removed/newer variant this binary
                    // does not know about gets the safe behavior, not a
                    // silently widened one.
                    _ => {
                        return HookStepOutcome::Denied {
                            rendered_error: format!(
                                "`{}` is denied: `pre_tool_use` hook `{}` failed ({failure}) \
                                 -- unrecognized on_failure policy, fail-closed",
                                call.tool.as_str(),
                                hook.id
                            ),
                            cause: HookDenialCause::Outage,
                        };
                    }
                },
            }
        }

        if must_reach_gate {
            HookStepOutcome::MustReachGate
        } else {
            HookStepOutcome::NoOpinion
        }
    }

    /// Authorize one tool call, consulting the cache first and the gate on a
    /// miss.
    ///
    /// Full ordering ( added the
    /// **`pre_tool_use` hook** step;
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
        // `must_reach_gate` is now a
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

        // An earlier design item, D4 §3: the `deny` half of
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

        // the `pre_tool_use` hook
        // step. SAME TIER as `deny_matches` immediately above -- checked
        // BEFORE the mode gate, the prompt-pattern step, the cache, pattern
        // allows, and `AutoAllow` -- see this method's own doc for why. A
        // denying hook beats every one of those, regardless of mode,
        // regardless of `must_reach_gate`, for the identical
        // most-restrictive-wins reason `deny_matches` already states. No
        // hook here can ever produce `Allow`: `Self::pre_tool_use_hook_
        // denial` returns `HookStepOutcome::Denied` only for an explicit
        // `HookPermissionVerdict::Deny` or a runner failure whose
        // `on_failure` resolved to `Deny` -- never for `NoOpinion` -- and
        // neither `HookPermissionVerdict` nor `HookOnFailure` has an
        // `Allow` variant for a future edit to accidentally start acting on
        // (see each type's own doc).
        //
        // `HookStepOutcome::MustReachGate` -- an `on_failure: Prompt`
        // outage, and only that -- is NOT a denial: it ONLY sets
        // `must_reach_gate`, the same accumulator `check_root` and
        // `Self::prompt_matches` already write to, and execution falls
        // through to the plan-mode gate immediately below exactly as it
        // would with nothing installed here at all. This is what makes the
        // operator's own `Deny` rules (checked above, unconditionally) and
        // plan-mode's own refusal (checked immediately below, also
        // unconditionally) still outrank an `on_failure: Prompt` firing --
        // neither check is skipped, so a narrowing here can never widen
        // past either.
        match self.pre_tool_use_hook_denial(ctx, call).await {
            HookStepOutcome::NoOpinion => {}
            HookStepOutcome::MustReachGate => {
                must_reach_gate = true;
            }
            HookStepOutcome::Denied {
                rendered_error,
                cause: _,
            } => {
                self.emit(
                    ctx,
                    Event::PermissionResolved {
                        call_id: call.call_id.clone(),
                        decision: PermissionDecisionKind::Denied,
                    },
                );
                return PermissionOutcome::Deny { rendered_error };
            }
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

        // the PROMPT step.
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

            // A pattern grant spares the operator a prompt -- but never for
            // a call whose tool declares `RenderKind::ShellCommand`:
            // `PatternRule::matches_render` refuses those unconditionally
            // now (see `conway_core::permission_pattern`'s own module
            // doc). Any `bash` call -- chained or not -- falls through to
            // the gate below no matter what patterns exist.
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
        //.
        if !grants.contains(&grant) {
            grants.push(grant);
        }
    }

    fn emit(&self, ctx: &PermissionCtx, event: Event) {
        self.bus.emit(ctx.session, ctx.agent_id, event);
    }
}

/// The `pre_tool_use` hook step's own
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

    use std::collections::BTreeMap;

    use conway_core::agent::PermissionRequest;
    use conway_core::error::HookFailure;
    use conway_core::hook::{ContextDelta, HookAnswer};
    use conway_core::permission_pattern::{PatternOrigin, PatternRule, Select};

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
            Self::answer(HookAnswer::new(
                ContextDelta::default(),
                HookPermissionVerdict::Deny {
                    reason: reason.to_string(),
                },
            ))
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

    /// A `HookRunner` double that answers PER `command[0]` rather than
    /// `ScriptedHookRunner`'s one fixed answer for every invocation --
    /// `PermissionBroker` holds a single shared `Arc<dyn HookRunner>` for
    /// every installed hook, so proving the two-hook interaction below
    /// (one hook's outage, a DIFFERENT hook's own verdict) needs one
    /// double that can tell the two invocations apart and answer each
    /// differently. Also records the ORDER `command[0]` values were seen
    /// in, so a test can assert both hooks were actually consulted, and in
    /// registration order.
    struct PerCommandHookRunner {
        scripted: BTreeMap<String, Result<HookAnswer, HookFailure>>,
        calls: Mutex<Vec<String>>,
    }

    impl PerCommandHookRunner {
        fn new(scripted: Vec<(&str, Result<HookAnswer, HookFailure>)>) -> Arc<Self> {
            Arc::new(Self {
                scripted: scripted
                    .into_iter()
                    .map(|(command, result)| (command.to_string(), result))
                    .collect(),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn call_order(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl HookRunner for PerCommandHookRunner {
        async fn run(&self, invocation: &HookInvocation) -> Result<HookAnswer, HookFailure> {
            let key = invocation
                .command
                .first()
                .cloned()
                .unwrap_or_else(|| "<empty command>".to_string());
            self.calls.lock().unwrap().push(key.clone());
            self.scripted.get(&key).cloned().unwrap_or_else(|| {
                panic!("PerCommandHookRunner invoked for unscripted command `{key}`")
            })
        }
    }

    fn hook_spec(id: &str) -> PreToolUseHookSpec {
        PreToolUseHookSpec {
            id: id.to_string(),
            command: vec!["/usr/bin/env".to_string(), "true".to_string()],
            timeout_ms: 1_000,
            matcher: None,
            // Today's -- and the default's -- fail-closed posture: every
            // EXISTING test below that builds its fixture through this
            // helper keeps exercising the exact same outage behavior as
            // before `on_failure` existed.
            on_failure: HookOnFailure::default(),
            // Every existing test through this helper predates plugin-
            // registered hooks -- `Operator` is what these fixtures always
            // implicitly were.
            origin: HookOrigin::Operator,
        }
    }

    /// Sibling of [`hook_spec`] for the tests that need a NON-default
    /// `on_failure` policy -- `..hook_spec(id)` keeps every other field
    /// identical, so only the one field under test ever varies.
    fn hook_spec_with_on_failure(id: &str, on_failure: HookOnFailure) -> PreToolUseHookSpec {
        PreToolUseHookSpec {
            on_failure,
            ..hook_spec(id)
        }
    }

    fn hook_spec_matching(id: &str, matcher: &str) -> PreToolUseHookSpec {
        PreToolUseHookSpec {
            matcher: Some(matcher.to_string()),
            ..hook_spec(id)
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

    /// Sibling of [`bash_call`] for a non-`bash` tool -- the matcher tests
    /// need to prove a rule fires for one tool and not another, and `bash`
    /// alone cannot show that.
    fn call_for_tool(call_id: &str, tool: &str) -> AuthorizedCall {
        AuthorizedCall {
            call_id: call_id.into(),
            tool: ToolName::new(tool),
            category: ToolCategory::Read,
            arguments: serde_json::json!({}),
            rendered: format!("{tool}({{}})"),
            path_args: PathArgs::None,
            render_kind: RenderKind::Structured,
        }
    }

    /// A `Structured` call with arbitrary `arguments` -- the fixture the
    /// `When::ArgsMatch` tests need: they assert a pinned-field grant
    /// auto-allows a matching call (gate never reached) and lets a
    /// non-matching call fall through to the gate.
    fn structured_call(call_id: &str, tool: &str, arguments: serde_json::Value) -> AuthorizedCall {
        AuthorizedCall {
            call_id: call_id.into(),
            tool: ToolName::new(tool),
            category: ToolCategory::Read,
            arguments,
            rendered: format!("{tool}(...)"),
            path_args: PathArgs::None,
            render_kind: RenderKind::Structured,
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

    /// A `When::ArgsMatch` allow rule (the `[p]` field editor's grant)
    /// auto-allows a call whose pinned field matches and spares the gate
    /// (it is never reached); a call with a different value for the pinned
    /// field is NOT covered and falls through to the gate. An empty `pinned`
    /// map is the `tool:*` equivalent and matches every call for the tool.
    /// A `ShellCommand` tool is refused for free by `gate_allows`, so an
    /// `ArgsMatch` grant on `bash` authorizes nothing (board
    /// `01KZDDPC5MMD49F6JPV9CW4TVM`'s posture preserved).
    #[tokio::test]
    async fn args_match_rule_auto_allows_matching_calls_and_refuses_shell() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        // Pin `path` to "/etc/hosts"; leave every other field wildcard.
        let mut pinned = BTreeMap::new();
        pinned.insert("path".to_string(), serde_json::json!("/etc/hosts"));
        let installed = broker.remember_pattern_rule(
            Rule::args_match_allow_rule("read", pinned),
            PermissionScope::Session,
            AgentId::new(),
            PatternOrigin::Interactive,
            Path::new("/"),
        );
        assert!(installed, "the ArgsMatch allow rule installs");

        // Matching call: gate is NOT reached (auto-allowed by the pattern).
        let matching = structured_call(
            "c1",
            "read",
            serde_json::json!({"path":"/etc/hosts","offset":0}),
        );
        assert_eq!(
            broker.decide(&ctx, &matching).await,
            PermissionOutcome::Allow,
        );
        assert_eq!(
            gate.call_count(),
            0,
            "a matching call must not reach the gate"
        );

        // Non-matching call (different pinned value): the rule does not
        // cover it, so it falls through to the gate.
        let other = structured_call("c2", "read", serde_json::json!({"path":"/etc/passwd"}));
        broker.decide(&ctx, &other).await;
        assert_eq!(gate.call_count(), 1, "a non-matching call reaches the gate");

        // Missing pinned field: also a non-match (falls through to gate).
        let missing = structured_call("c3", "read", serde_json::json!({"offset":0}));
        broker.decide(&ctx, &missing).await;
        assert_eq!(
            gate.call_count(),
            2,
            "a call missing the pinned field reaches the gate"
        );

        // Key-order independence: the same object with reordered keys
        // matches (canonical JSON equality, same primitive the cache hashes
        // with).
        let reordered = structured_call(
            "c4",
            "read",
            serde_json::json!({"offset":0,"path":"/etc/hosts"}),
        );
        assert_eq!(
            broker.decide(&ctx, &reordered).await,
            PermissionOutcome::Allow,
        );
        assert_eq!(
            gate.call_count(),
            2,
            "a reordered-keys match must not reach the gate"
        );

        // A different tool is not covered by this `select: Tools(["read"])`
        // rule, so it reaches the gate.
        let other_tool = structured_call("c5", "write", serde_json::json!({"path":"/etc/hosts"}));
        broker.decide(&ctx, &other_tool).await;
        assert_eq!(gate.call_count(), 3, "a different tool reaches the gate");

        // An `ArgsMatch` grant on a `ShellCommand` tool authorizes nothing:
        // `gate_allows` refuses it, so `bash` falls through to the gate
        // regardless of the rule (the AMENDED section's posture).
        let mut shell_pinned = BTreeMap::new();
        shell_pinned.insert("command".to_string(), serde_json::json!("git status"));
        assert!(
            broker.remember_pattern_rule(
                Rule::args_match_allow_rule("bash", shell_pinned),
                PermissionScope::Session,
                AgentId::new(),
                PatternOrigin::Interactive,
                Path::new("/"),
            ),
            "the ArgsMatch rule on bash installs (admission does not gate-check)",
        );
        let shell = bash_call("c6", "git status");
        broker.decide(&ctx, &shell).await;
        assert_eq!(
            gate.call_count(),
            4,
            "an ArgsMatch grant on a shell tool must not auto-allow -- the gate is reached",
        );
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
    /// via the SAME code path (proven by the observable outcome, not
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

    // ---------------------------------------------------------------- matcher --

    /// ACCEPTANCE: a matcher on a
    /// `pre_tool_use` rule narrows which tool calls consult it -- a denying
    /// hook matching `read` never runs for a `bash` call.
    #[tokio::test]
    async fn a_matching_pre_tool_use_hook_denies_only_its_own_tool() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = ScriptedHookRunner::deny("reads are refused");
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec_matching("read-guard", "read")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        // The matching tool is denied, and the hook never reaches the gate.
        let denied = broker.decide(&ctx, &call_for_tool("c1", "read")).await;
        assert!(
            matches!(denied, PermissionOutcome::Deny { .. }),
            "a matching hook must still deny: {denied:?}"
        );
        assert_eq!(runner.call_count(), 1);

        // A DIFFERENT tool never consults this hook at all -- proceeds
        // straight through to the gate, unaffected.
        let allowed = broker.decide(&ctx, &bash_call("c2", "git status")).await;
        assert_eq!(allowed, PermissionOutcome::Allow);
        assert_eq!(
            runner.call_count(),
            1,
            "the hook must not have been consulted a second time for a non-matching tool"
        );
        assert_eq!(
            gate.call_count(),
            1,
            "only the non-matching call reaches the gate"
        );
    }

    /// Sibling of `decide_is_unchanged_when_no_hook_runner_is_installed`: an
    /// ABSENT matcher (not merely an absent runner) preserves today's
    /// fire-for-every-tool behavior -- a hook with no `matcher` set still
    /// denies every tool, exactly as before this field existed.
    #[tokio::test]
    async fn an_absent_matcher_denies_every_tool() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = ScriptedHookRunner::deny("no tool is exempt");
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let read_outcome = broker.decide(&ctx, &call_for_tool("c1", "read")).await;
        let bash_outcome = broker.decide(&ctx, &bash_call("c2", "git status")).await;

        assert!(matches!(read_outcome, PermissionOutcome::Deny { .. }));
        assert!(matches!(bash_outcome, PermissionOutcome::Deny { .. }));
        assert_eq!(gate.call_count(), 0);
    }

    // ---------------------------------------------------------------------
    // `on_failure` (board item `01M0X1AH44SNMK5TZ507K30QNP`):
    // `docs/vision/DESIGN-permission-modes.md` §3a/§3c. A hook VERDICT
    // (`HookPermissionVerdict::Deny`) and a hook OUTAGE (`Err(HookFailure)`,
    // resolved through this registration's own `on_failure` policy) are two
    // structurally different facts -- "the guard said no" versus "the guard
    // is down" -- and these tests pin both the structural distinction
    // itself (`HookStepOutcome`'s `cause` field) and the behavior it now
    // makes possible (`on_failure: Prompt` degrading an outage to the
    // operator's own gate instead of bricking the session).
    // ---------------------------------------------------------------------

    /// **The structural fix, proven directly.** A hook that runs to
    /// completion and returns an explicit `Deny`, and a hook whose runner
    /// itself fails (with `on_failure` at its default, `Deny`), both still
    /// deny -- but `HookStepOutcome::Denied`'s `cause` field is DIFFERENT
    /// for the two, provably: a downstream consumer (a future status
    /// surface, or this test) can match on `cause` alone and never inspect
    /// `rendered_error` at all. Before this item, both were the identical
    /// `Option<String>` value.
    #[tokio::test]
    async fn hook_step_outcome_distinguishes_a_verdict_denial_from_an_outage_denial_structurally() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        // A hook that RAN and said no.
        let verdict_broker = PermissionBroker::new(RecordingGate::new(), EventBus::new(64));
        verdict_broker.set_hook_runner(Some(ScriptedHookRunner::deny(
            "touches a path this hook refuses",
        )));
        verdict_broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let verdict_outcome = verdict_broker
            .pre_tool_use_hook_denial(&ctx, &bash_call("c1", "git status"))
            .await;
        assert!(
            matches!(
                verdict_outcome,
                HookStepOutcome::Denied {
                    cause: HookDenialCause::Verdict,
                    ..
                }
            ),
            "a hook that ran and said no must tag its denial `Verdict`: {verdict_outcome:?}"
        );

        // A hook whose runner could not be reached at all -- `on_failure`
        // at its default, `Deny`.
        let outage_broker = PermissionBroker::new(RecordingGate::new(), EventBus::new(64));
        outage_broker.set_hook_runner(Some(ScriptedHookRunner::failing(HookFailure::Spawn {
            detail: "no such file or directory".to_string(),
        })));
        outage_broker.set_pre_tool_use_hooks(vec![hook_spec("guard")]);
        let outage_outcome = outage_broker
            .pre_tool_use_hook_denial(&ctx, &bash_call("c2", "git status"))
            .await;
        assert!(
            matches!(
                outage_outcome,
                HookStepOutcome::Denied {
                    cause: HookDenialCause::Outage,
                    ..
                }
            ),
            "a hook whose runner failed must tag its denial `Outage`: {outage_outcome:?}"
        );
    }

    /// **Acceptance 1: omitting `on_failure` reproduces today's exact
    /// behavior, message included.** `hook_spec` never sets `on_failure`
    /// explicitly (it uses `HookOnFailure::default()`) -- this is the SAME
    /// fixture every pre-existing fail-closed test in this module already
    /// uses (`a_hook_that_fails_to_spawn_denies_the_call`,
    /// `a_hook_that_times_out_denies_the_call`,
    /// `a_hook_with_malformed_output_denies_the_call`, all run UNEDITED),
    /// so this test only needs to pin the exact rendered message stays
    /// byte-for-byte what it was before `on_failure` existed.
    #[tokio::test]
    async fn omitting_on_failure_denies_with_the_exact_pre_existing_message() {
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

        match outcome {
            PermissionOutcome::Deny { rendered_error } => {
                assert_eq!(
                    rendered_error,
                    "`bash` is denied: `pre_tool_use` hook `guard` failed \
                     (hook command failed to spawn: no such file or directory) -- fail-closed",
                );
            }
            PermissionOutcome::Allow => panic!("a hook that fails to spawn must deny"),
        }
        assert_eq!(gate.call_count(), 0);
    }

    /// **Acceptance 2: `on_failure: Prompt` whose runner fails reaches the
    /// operator's gate, not a denial.** Mirrors
    /// `plugin_prompt_forces_the_gate_even_under_autoallow`'s own shape:
    /// `AutoAllow` is the mode this matters most in (no human already in
    /// the loop), so proving the gate is still reached there is the
    /// load-bearing case.
    #[tokio::test]
    async fn on_failure_prompt_whose_runner_fails_reaches_the_operators_gate_not_a_denial() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        let runner = ScriptedHookRunner::failing(HookFailure::Spawn {
            detail: "connection refused".to_string(),
        });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec_with_on_failure(
            "local-model-guard",
            HookOnFailure::Prompt,
        )]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert_eq!(
            outcome,
            PermissionOutcome::Allow,
            "the gate grants AllowOnce -- an outage resolved to `Prompt` is not a denial"
        );
        assert_eq!(
            gate.call_count(),
            1,
            "an `on_failure: Prompt` outage must force the gate even under AutoAllow -- \
             not auto-allowed and not denied"
        );
        assert_eq!(runner.call_count(), 1);
    }

    /// **Acceptance 3: a hook declaring `Prompt` whose runner returns an
    /// EXPLICIT `Deny` still denies -- an outage and a verdict take
    /// different paths.** `on_failure` governs ONLY what happens when the
    /// runner itself cannot be consulted; it is never consulted for a hook
    /// that ran to completion and had an opinion. Paired with the previous
    /// test in this same file: same hook, same `on_failure: Prompt`
    /// registration, but the runner SUCCEEDS this time and says no --
    /// denied outright, gate never reached.
    #[tokio::test]
    async fn on_failure_prompt_whose_runner_returns_an_explicit_deny_still_denies() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        let runner = ScriptedHookRunner::deny("touches a path this hook refuses");
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec_with_on_failure(
            "local-model-guard",
            HookOnFailure::Prompt,
        )]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        match outcome {
            PermissionOutcome::Deny { rendered_error } => {
                assert!(
                    rendered_error.contains("touches a path this hook refuses"),
                    "an explicit verdict's own reason must still be rendered: {rendered_error}"
                );
            }
            PermissionOutcome::Allow => {
                panic!(
                    "a hook's own explicit Deny verdict must still deny, regardless of that \
                     hook's `on_failure` setting -- `on_failure` governs outages only"
                )
            }
        }
        assert_eq!(
            gate.call_count(),
            0,
            "the operator's gate must never be consulted for an explicit hook denial"
        );
        assert_eq!(runner.call_count(), 1);
    }

    /// **Acceptance 4: an unparseable answer takes the `on_failure` path,
    /// never a guessed verdict.** `HookFailure::UnparseableAnswer` is just
    /// another `Err(HookFailure)` as far as this broker is concerned --
    /// routed through the SAME `on_failure` policy as a spawn failure or a
    /// timeout, never given a carve-out that silently denies (or silently
    /// allows) regardless of what the registration asked for.
    #[tokio::test]
    async fn unparseable_hook_output_with_on_failure_prompt_reaches_the_gate_not_a_guessed_verdict()
    {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        let runner = ScriptedHookRunner::failing(HookFailure::UnparseableAnswer {
            detail: "not valid JSON".to_string(),
        });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec_with_on_failure(
            "local-model-guard",
            HookOnFailure::Prompt,
        )]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert_eq!(
            outcome,
            PermissionOutcome::Allow,
            "an unparseable answer with `on_failure: Prompt` must reach the gate, not be \
             denied or silently allowed"
        );
        assert_eq!(gate.call_count(), 1);
    }

    /// **Acceptance 5, half one: `on_failure: Prompt` never bypasses an
    /// operator `Deny` rule -- the subordination boundary.** Mirrors
    /// `operator_deny_beats_plugin_prompt`'s own shape: the operator
    /// independently denied this tool; a guard's outage resolving to
    /// `Prompt` must not widen past that denial into an ask.
    #[tokio::test]
    async fn on_failure_prompt_never_bypasses_an_operator_deny_rule() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        assert!(
            broker.remember_deny_rule(
                operator_deny_rule("bash"),
                PatternOrigin::Interactive,
                Path::new("/"),
            ),
            "the operator deny rule installs"
        );
        let runner = ScriptedHookRunner::failing(HookFailure::Spawn {
            detail: "connection refused".to_string(),
        });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec_with_on_failure(
            "local-model-guard",
            HookOnFailure::Prompt,
        )]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "the operator's own deny rule must beat an `on_failure: Prompt` outage: {outcome:?}"
        );
        assert_eq!(
            gate.call_count(),
            0,
            "the operator deny fires before the hook step even runs; the outage never forces \
             a prompt"
        );
        // The operator's deny rule short-circuits `decide()` before the
        // hook step is ever reached, so the (failing) hook runner is never
        // even invoked -- the strongest form of "cannot bypass."
        assert_eq!(runner.call_count(), 0);
    }

    /// **Acceptance 5, half two: `on_failure: Prompt` never bypasses
    /// plan-mode refusal.** Mirrors `plan_mode_denial_beats_plugin_prompt`'s
    /// own shape: plan mode denies `bash` (an `Execute` tool) outright; a
    /// guard's outage resolving to `Prompt` -- checked BEFORE the plan-mode
    /// gate in `decide()`'s own step order -- sets `must_reach_gate` but
    /// must not widen past the mode refusal that follows it.
    #[tokio::test]
    async fn on_failure_prompt_never_bypasses_plan_mode_refusal() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::Plan);
        let runner = ScriptedHookRunner::failing(HookFailure::Spawn {
            detail: "connection refused".to_string(),
        });
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![hook_spec_with_on_failure(
            "local-model-guard",
            HookOnFailure::Prompt,
        )]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "plan mode's own refusal must beat an `on_failure: Prompt` outage: {outcome:?}"
        );
        assert_eq!(
            gate.call_count(),
            0,
            "plan mode fires before the gate; the outage never forces a prompt"
        );
        assert_eq!(
            runner.call_count(),
            1,
            "the hook step DOES run here (it precedes plan mode in decide()'s own order) -- \
             it is the RESULT (must_reach_gate, not a bypass) that plan mode still overrides"
        );
    }

    /// **The two-hook interaction `HookStepOutcome::MustReachGate`'s own
    /// doc promises, actually exercised.** Every OTHER test in this module
    /// installs exactly one hook; this one installs two. Hook A declares
    /// `on_failure: Prompt` and its runner FAILS -- an outage, resolved to
    /// `must_reach_gate = true`, which must NOT stop the loop. Hook B is a
    /// second, independently installed hook whose runner returns an
    /// outright `HookPermissionVerdict::Deny`. The call must come back
    /// `Deny` -- hook B's refusal winning over hook A's mere
    /// prompt-worthy outage -- not `Allow`, and not merely a forced trip
    /// to the operator's gate.
    ///
    /// **Why this goes red under the drift board item `01M0XQBTW4JMS7XQESDMS3KNZY`
    /// names.** `pre_tool_use_hook_denial`'s loop has no literal `continue`
    /// keyword in its `HookOnFailure::Prompt` arm today -- it merely sets
    /// `must_reach_gate = true` and falls off the end of the `for` body,
    /// which is what lets the next iteration (hook B) run at all. Every
    /// NEIGHBOURING arm in that same loop (`HookOnFailure::Deny`, and the
    /// `HookPermissionVerdict::Deny` check above it) instead `return`s
    /// immediately -- so a refactor that "regularizes" the `Prompt` arm to
    /// match its neighbours, turning `must_reach_gate = true;` into
    /// `return HookStepOutcome::MustReachGate;`, is exactly the drift this
    /// item's spec warns is the natural direction. Under that mutation,
    /// the loop would return after hook A without ever reaching hook B:
    /// `runner.call_order()` would contain only `"hook-a"` (proven below
    /// to contain both), `pre_tool_use_hook_denial` would report
    /// `MustReachGate` instead of `Denied`, `decide()` would fall through
    /// to `RecordingGate` (which always grants), and this test's `assert!
    /// (matches!(outcome, PermissionOutcome::Deny { .. }))` would fail
    /// against an `Allow` outcome -- as would the `gate.call_count() == 0`
    /// assertion, which would observe `1`. Both assertions fail for the
    /// same underlying reason, from two independent angles.
    #[tokio::test]
    async fn a_second_hooks_outright_refusal_still_wins_after_an_earlier_hooks_deferred_outage() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        let runner = PerCommandHookRunner::new(vec![
            (
                "hook-a",
                Err(HookFailure::Spawn {
                    detail: "no such file or directory".to_string(),
                }),
            ),
            (
                "hook-b",
                Ok(HookAnswer::new(
                    ContextDelta::default(),
                    HookPermissionVerdict::Deny {
                        reason: "hook B refuses outright".to_string(),
                    },
                )),
            ),
        ]);
        broker.set_hook_runner(Some(runner.clone()));
        broker.set_pre_tool_use_hooks(vec![
            PreToolUseHookSpec {
                command: vec!["hook-a".to_string()],
                on_failure: HookOnFailure::Prompt,
                ..hook_spec("hook-a")
            },
            PreToolUseHookSpec {
                command: vec!["hook-b".to_string()],
                ..hook_spec("hook-b")
            },
        ]);
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "hook B's outright refusal must win over hook A's deferred outage: {outcome:?}"
        );
        assert_eq!(
            runner.call_order(),
            vec!["hook-a".to_string(), "hook-b".to_string()],
            "both hooks must have been consulted, in registration order -- hook A's outage \
             must not have short-circuited the loop before hook B ran"
        );
        assert_eq!(
            gate.call_count(),
            0,
            "hook B's own verdict denies outright -- the operator's gate is never reached"
        );
    }

    // ---------------------------------------------------------------------
    // `permission.policy/1` subordination composition (board item
    // `01M03VKJG7JJ0JEKY265WA7MJ7`). A plugin's declared policy reaches the
    // broker as `PatternOrigin::Plugin` deny/prompt rules -- installed by
    // `ConwayBuilder::build` from each plugin's `Plugin::permission_rules()`.
    // These tests prove the LOAD-BEARING subordination property at the real
    // `PermissionBroker::decide` seam: a plugin may NARROW (deny / force the
    // gate) but may NEVER WIDEN past the operator's own
    // `permissions.json`/`PermissionMode`. The wire half (the policy
    // genuinely reaching the host over the persistent transport and being
    // stored in this shape) is proven in `conway-plugin-subprocess`'s
    // `tests/permission_policy.rs`; these tests take the broker-side
    // installation that the `conway` facade performs and assert the
    // `decide()` outcomes.
    // ---------------------------------------------------------------------

    /// A `Plugin::permission_rules()`-shaped deny rule, the exact `Rule` the
    /// `conway` facade installs for a `PluginPermissionVerdict::Deny`
    /// verdict: `Select::Tools([tool]) + When::Always + Then::Deny`,
    /// `PatternOrigin::Plugin`. The placeholder `/` is inert for
    /// `When::Always` (the facade uses it too).
    fn plugin_deny_rule(tool: &str) -> Rule {
        Rule {
            select: Select::Tools(vec![tool.to_string()]),
            when: When::Always,
            then: Then::Deny,
        }
    }

    /// The prompt twin of [`plugin_deny_rule`]: `Then::Prompt`,
    /// `PatternOrigin::Plugin`.
    fn plugin_prompt_rule(tool: &str) -> Rule {
        Rule {
            select: Select::Tools(vec![tool.to_string()]),
            when: When::Always,
            then: Then::Prompt,
        }
    }

    /// An operator-authored deny rule (the SAME shape, but
    /// `PatternOrigin::Interactive` -- the origin an operator's own
    /// `permissions.json` / interactive grant uses, NOT a plugin). Used to
    /// model "the operator independently marked this tool dangerous" in the
    /// subordination tests below.
    fn operator_deny_rule(tool: &str) -> Rule {
        Rule {
            select: Select::Tools(vec![tool.to_string()]),
            when: When::Always,
            then: Then::Deny,
        }
    }

    /// **A plugin `Prompt` verdict forces the operator's gate even under
    /// `AutoAllow`.** This is acceptance criterion 1's first half: a plugin
    /// declaring its tool dangerous (mapped to `prompt`) causes an approval
    /// prompt -- the call is NOT silently auto-allowed. `AutoAllow` is the
    /// mode a plugin's prompt matters most in (no human already in the
    /// loop); proving it here is the load-bearing case.
    #[tokio::test]
    async fn plugin_prompt_forces_the_gate_even_under_autoallow() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        // The facade installs a plugin's `prompt` verdict as a
        // `PatternOrigin::Plugin` prompt rule.
        assert!(
            broker.remember_prompt_rule(
                plugin_prompt_rule("greet"),
                PatternOrigin::Plugin,
                Path::new("/"),
            ),
            "the prompt rule installs"
        );
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &call_for_tool("c1", "greet")).await;

        assert_eq!(
            outcome,
            PermissionOutcome::Allow,
            "the gate grants AllowOnce"
        );
        assert_eq!(
            gate.call_count(),
            1,
            "a plugin prompt must force the gate even under AutoAllow -- not auto-allowed"
        );
    }

    /// **A plugin `Deny` verdict blocks the call even under `AutoAllow`.**
    /// Acceptance criterion 1's "or deny" alternative: a plugin declaring
    /// its tool dangerous (mapped to `deny`) refuses the call outright,
    /// before the gate is ever consulted.
    #[tokio::test]
    async fn plugin_deny_blocks_the_call_even_under_autoallow() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        assert!(
            broker.remember_deny_rule(
                plugin_deny_rule("greet"),
                PatternOrigin::Plugin,
                Path::new("/"),
            ),
            "the deny rule installs"
        );
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &call_for_tool("c1", "greet")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "a plugin deny must block the call: {outcome:?}"
        );
        assert_eq!(
            gate.call_count(),
            0,
            "the gate is never consulted for a deny"
        );
    }

    /// **LOAD-BEARING: an operator `Deny` beats a plugin `Abstain` -- the
    /// wire policy cannot widen.** A plugin declaring `Safe` (mapped to
    /// `abstain` -- no opinion, installs nothing) for a tool the operator
    /// INDEPENDENTLY marked dangerous (an operator `deny` rule) STAYS
    /// denied. The operator wins; the plugin's abstain narrows nothing AND
    /// widens nothing. This is the subordination test the spec names as
    /// load-bearing.
    #[tokio::test]
    async fn operator_deny_beats_plugin_abstain_the_wire_policy_cannot_widen() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        // The operator independently marked `greet` dangerous.
        assert!(
            broker.remember_deny_rule(
                operator_deny_rule("greet"),
                PatternOrigin::Interactive,
                Path::new("/"),
            ),
            "the operator deny rule installs"
        );
        // The plugin declared `abstain` for `greet` -- the facade installs
        // NOTHING for an abstain verdict (no rule added). Nothing to install
        // here mirrors that exactly.
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &call_for_tool("c1", "greet")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "the operator's deny stands -- a plugin abstain cannot widen it: {outcome:?}"
        );
        assert_eq!(gate.call_count(), 0, "denied before the gate");
    }

    /// **LOAD-BEARING: an operator `Deny` beats a plugin `Prompt`.** Even
    /// when the plugin asks for an approval prompt, the operator's own deny
    /// fires FIRST (step 2 of `decide`) and the plugin's prompt (step 4)
    /// is never reached -- the call is denied, not prompted. The operator
    /// wins; the plugin can narrow (ask) but cannot widen past an operator
    /// denial.
    #[tokio::test]
    async fn operator_deny_beats_plugin_prompt() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::AutoAllow);
        // The plugin asks for a prompt on `greet`.
        assert!(
            broker.remember_prompt_rule(
                plugin_prompt_rule("greet"),
                PatternOrigin::Plugin,
                Path::new("/"),
            ),
            "the plugin prompt rule installs"
        );
        // The operator independently denied `greet` -- installed AFTER the
        // plugin rule to prove ORDER does not matter (most-restrictive-wins:
        // a deny beats a prompt regardless of installation order).
        assert!(
            broker.remember_deny_rule(
                operator_deny_rule("greet"),
                PatternOrigin::Interactive,
                Path::new("/"),
            ),
            "the operator deny rule installs"
        );
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        let outcome = broker.decide(&ctx, &call_for_tool("c1", "greet")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "the operator deny wins over the plugin prompt -- denied, not prompted: {outcome:?}"
        );
        assert_eq!(
            gate.call_count(),
            0,
            "the operator deny fires before the gate; the plugin prompt never forces a prompt"
        );
    }

    /// **Plan-mode denial beats a plugin `Prompt`.** The operator selected
    /// `Plan` mode (which denies a non-permitted category); a plugin prompt
    /// for a tool in that category does NOT override the mode -- the call
    /// is denied, not prompted. The operator's mode wins; the plugin
    /// narrows but cannot widen past a mode refusal.
    #[tokio::test]
    async fn plan_mode_denial_beats_plugin_prompt() {
        let gate = RecordingGate::new();
        let broker = PermissionBroker::new(gate.clone(), EventBus::new(64));
        broker.set_mode(PermissionMode::Plan);
        assert!(
            broker.remember_prompt_rule(
                plugin_prompt_rule("bash"),
                PatternOrigin::Plugin,
                Path::new("/"),
            ),
            "the plugin prompt rule installs"
        );
        let session = SessionId::new();
        let agent = AgentId::new();
        let ctx = test_ctx(agent, session);

        // The plugin prompt rule is for `bash` -- the SAME tool the call uses
        // -- so `prompt_matches` (step 4) WOULD match and set `must_reach_gate`
        // if it ran. `bash_call` is `Execute`, which `Plan` mode denies at step
        // 3, BEFORE the prompt step. The test only pins the boundary if the
        // prompt actually matches; a non-matching tool (e.g. `greet`) would
        // leave the outcome to plan mode alone and pass vacuously.
        let outcome = broker.decide(&ctx, &bash_call("c1", "git status")).await;

        assert!(
            matches!(outcome, PermissionOutcome::Deny { .. }),
            "plan mode denies Execute; a matching plugin prompt cannot widen past the mode refusal: {outcome:?}"
        );
        assert_eq!(
            gate.call_count(),
            0,
            "plan mode fires before the gate; the matching plugin prompt never forces a prompt"
        );
    }

    /// **A plugin cannot install an `Allow` rule -- the structural guard.**
    /// `remember_pattern_rule` (the allow admission) rejects
    /// `PatternOrigin::Plugin` + `Then::Allow` outright, returning `false`.
    /// This is the broker-boundary guard `docs/plugins/hooks.md` point 8's
    /// own "may only narrow" property rests on -- a future plugin transport
    /// that reuses `PatternOrigin::Plugin` to call the allow path with
    /// `Then::Allow` is refused at the broker boundary, never silently
    /// installed. The `PluginPermissionVerdict` type has no `Allow` variant
    /// (so the facade never even reaches this guard), but this test pins
    /// the guard itself: the invariant rests on a guard, not on the absence
    /// of a transport.
    #[test]
    fn a_plugin_cannot_install_an_allow_rule() {
        let broker = PermissionBroker::new(
            RecordingGate::new() as Arc<dyn PermissionGate>,
            EventBus::new(64),
        );
        let allow_rule = Rule {
            select: Select::Tools(vec!["greet".to_string()]),
            when: When::Always,
            then: Then::Allow,
        };
        assert!(
            !broker.remember_pattern_rule(
                allow_rule,
                PermissionScope::Session,
                AgentId::new(),
                PatternOrigin::Plugin,
                Path::new("/"),
            ),
            "a Plugin-origin Allow rule is refused at the broker boundary -- the wire policy cannot widen"
        );
    }
}

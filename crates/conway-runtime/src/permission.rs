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
use conway_core::permission_pattern::{PatternOrigin, PatternRule};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use conway_core::agent::{
    PermissionDecision, PermissionDecisionKind, PermissionRequest, PermissionScope,
};
use conway_core::containment::{CanonicalRoot, Containment};
use conway_core::content::ToolCategory;
use conway_core::event::Event;
use conway_core::ids::{AgentId, SessionId, ToolName};
use conway_core::ports::{PathArgs, PermissionGate, RenderKind};

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
fn resolve_like_the_tool_will(cwd: &Path, raw: &str) -> Option<PathBuf> {
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantScope {
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
    patterns: RwLock<Vec<(PatternRule, GrantScope, PatternOrigin)>>,
    /// Board item 01KYT8SGX32CP56PRJNG72V2W5: prefix-pattern DENY rules.
    /// Unlike `patterns` above, these carry no `GrantScope` -- a `deny`
    /// rule is D4 §3's asymmetric half, "applies immediately, trusted or
    /// not, from any file, to any requester," so it is checked in
    /// `Self::decide` for EVERY call regardless of who is asking. Matched
    /// via `PatternRule::matches_deny`, which deliberately does not consult
    /// the metacharacter gate `patterns` above is gated by -- see that
    /// method's own doc.
    deny_patterns: RwLock<Vec<(PatternRule, PatternOrigin)>>,
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
    prompt_patterns: RwLock<Vec<(PatternRule, PatternOrigin)>>,
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
        }
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
        let grant = grant_scope_for(scope, granting_agent);
        self.patterns
            .write()
            .expect("permission patterns poisoned")
            .push((rule, grant, origin));
    }

    /// Installs a DENY rule, attributed to `origin`. Unlike
    /// [`Self::remember_pattern`], there is no `scope` parameter: a deny
    /// rule applies to every requester in the session, unconditionally --
    /// narrowing what is authorized has no failure mode worth scoping
    /// (board item 01KYT8SGX32CP56PRJNG72V2W5, D4 §3).
    pub fn remember_deny_pattern(&self, rule: PatternRule, origin: PatternOrigin) {
        self.deny_patterns
            .write()
            .expect("permission deny patterns poisoned")
            .push((rule, origin));
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
        self.prompt_patterns
            .write()
            .expect("permission prompt patterns poisoned")
            .push((rule, origin));
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
            .map(|(rule, _, origin)| (rule.clone(), origin.clone()))
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
            .cloned()
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
            .cloned()
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
        match patterns.iter().position(|(r, _, o)| r == rule && o == origin) {
            Some(idx) => {
                patterns.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Whether any installed pattern authorizes this call.
    ///
    /// The metacharacter gate is inside `PatternRule::matches_render`, so a
    /// chained SHELL command can never satisfy a pattern here regardless of
    /// what is installed. `call.render_kind` -- the resolved tool's own
    /// declaration -- decides whether that gate is even consulted at all
    /// (board item 01KYT3NSWRHMPEAXVXRJ73KDYR): `RenderKind::ShellCommand`
    /// gates exactly as before; `RenderKind::Structured` skips a gate that,
    /// for that tool's rendering, could only ever reject harmless JSON
    /// syntax.
    fn pattern_allows(&self, ctx: &PermissionCtx, call: &AuthorizedCall) -> bool {
        self.patterns
            .read()
            .expect("permission patterns poisoned")
            .iter()
            .any(|(rule, grant, _origin)| {
                grant.covers(ctx)
                    && rule.matches_render(call.tool.as_str(), &call.rendered, call.render_kind)
            })
    }

    /// The first installed `deny` rule that refuses this call, if any.
    /// Board item 01KYT8SGX32CP56PRJNG72V2W5, D4 §3: checked for EVERY
    /// requester (no `GrantScope`), via `PatternRule::matches_deny` --
    /// deliberately NOT `matches_render`, so a deny rule cannot be evaded
    /// by adding a shell metacharacter the way an allow rule is refused by
    /// one. See that method's own doc for the reasoning and its honest
    /// limit.
    fn deny_matches(&self, call: &AuthorizedCall) -> Option<PatternRule> {
        self.deny_patterns
            .read()
            .expect("permission deny patterns poisoned")
            .iter()
            .map(|(rule, _origin)| rule)
            .find(|rule| rule.matches_deny(call.tool.as_str(), &call.rendered))
            .cloned()
    }

    /// The first installed `prompt` rule that matches this call, if any.
    /// Board item 01KYTP1D3XWEZPW4AKPH54FNB3.
    ///
    /// **Deliberately reuses `PatternRule::matches_deny`, not
    /// `matches_render`.** `matches_render`'s metacharacter gate exists to
    /// keep an ALLOW from being satisfied by a chained command riding a
    /// matched prefix -- a concern that only applies to a rule that GRANTS
    /// something. A `prompt` rule grants nothing; its only effect is "ask
    /// the operator instead of skipping the ask", which is safe (indeed
    /// MORE conservative) to fire on a chained command too. Gating it the
    /// allow way would have the opposite of the intended effect: adding a
    /// shell metacharacter would EVADE the extra scrutiny a `prompt` rule
    /// exists to add, exactly the inversion `matches_deny`'s own doc
    /// describes for `deny`. `prompt` and `deny` are both admitted
    /// unconditionally at extension-architecture.md §5.5's stage 1 (they
    /// narrow; only `allow` needs trust) precisely because neither can be
    /// evaded this way.
    fn prompt_matches(&self, call: &AuthorizedCall) -> Option<PatternRule> {
        self.prompt_patterns
            .read()
            .expect("permission prompt patterns poisoned")
            .iter()
            .map(|(rule, _origin)| rule)
            .find(|rule| rule.matches_deny(call.tool.as_str(), &call.rendered))
            .cloned()
    }

    /// Authorize one tool call, consulting the cache first and the gate on a
    /// miss.
    ///
    /// Full ordering (board item 01KYTP1D3XWEZPW4AKPH54FNB3 added the
    /// `prompt` step; every step before it is unchanged): root →
    /// deny-pattern → plan-mode → **prompt-pattern** → cache →
    /// pattern-allow → `AutoAllow` → gate. Each step before `gate` either
    /// returns a decision outright (root denial, deny-pattern, plan-mode) or
    /// narrows what the LATER steps in this list are even allowed to do
    /// (root's `MustReachGate`, and now `prompt`, both set the
    /// `must_reach_gate` accumulator, which skips cache/pattern-allow/
    /// `AutoAllow` entirely and forces `gate.check`). Composition is
    /// most-restrictive-wins and registration order within a step (which
    /// `deny`/`prompt` rule matched first, which pattern grant was installed
    /// first) never changes the outcome, only which single value is picked
    /// to report.
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
        if let Some(rule) = self.deny_matches(call) {
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
                    rule.to_wire()
                ),
            };
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
                rendered_error: format!(
                    "plan mode: `{}` is a {:?} tool, which plan mode does not permit.                      Switch modes in /settings to run it.",
                    call.tool.as_str(),
                    call.category
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
        if self.prompt_matches(call).is_some() {
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

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
/// `conway_core::permission_pattern`'s own test suite keeps a hand-copy of
/// `crate::tools::runner::sanitize_rendered`'s body for the identical
/// layering reason -- `conway-core` cannot depend on `conway-runtime`
/// either.)
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

    /// Drops every pattern ALLOW grant and every cached `AllowAlways`,
    /// returning the session to asking. The revocation half of the escape
    /// hatch.
    ///
    /// Deliberately leaves `deny_patterns` untouched: revocation exists so
    /// an operator can back out of authority they granted (interactively,
    /// or via a trusted file) that turned out to be too broad. A `deny`
    /// rule narrows rather than grants, so there is nothing here for an
    /// operator to need an escape hatch FROM -- and most `deny` rules come
    /// from a file the operator does not control (or has not reviewed as
    /// carefully), so silently dropping them as a side effect of an
    /// unrelated "revoke my own grants" action would be a surprise in the
    /// unsafe direction.
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

    /// Authorize one tool call, consulting the cache first and the gate on a
    /// miss.
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
        let must_reach_gate = match Self::check_root(ctx, call) {
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

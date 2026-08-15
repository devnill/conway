//! Permission-file I/O: reading, validating, installing, and rewriting
//! `permissions.json` files. Board item `01KZVZ0ASR4CRFG822YWEAW30K`
//! (Stage 2c): this used to be inline inside `Conway` itself
//! (`crates/conway/src/conway.rs`); it moved here because "read/validate/
//! install/rewrite a permissions file" is a coherent unit with its own
//! tests, not forty-odd `Conway` methods' worth of unrelated facade
//! surface. It STAYS in this crate, deliberately, unlike the other two
//! Stage 2c moves (`intent`'s mechanism, `pull_in`/`promote`/`purge`) that
//! went to `conway-runtime`: reading `settings.json`/`permissions.json`
//! (`crate::config::discovery`, `crate::config::trust`) is facade-shaped
//! configuration concern, not a runtime one, and this module already
//! depends on both.
//!
//! **The one-implementation rule, honored by construction, not merely by
//! convention:** every function here that INSTALLS a rule
//! ([`install_allow_rule`]/[`install_deny_rule`]/[`install_prompt_rule`])
//! or CONSULTS the broker for a decision-relevant registration check
//! ([`validate_rule_registration`]/[`command_prefix_resolved_kinds`]) takes
//! a `&Runtime` and calls straight through to
//! `conway_runtime::permission::PermissionBroker`'s own methods -- the SAME
//! broker `Conway`'s other permission methods
//! (`grant_permission_pattern`/`revoke_permission_pattern`/...) call. No
//! function in this module re-implements or restates a permission
//! DECISION; every one either (a) performs pure file I/O (parse, read,
//! rewrite) that hands its result to the broker unchanged, or (b) is a
//! thin, unconditional pass-through to a broker method that already
//! existed before this move. Moving the CALL SITE cannot create a second
//! place a decision is reached, only a second place the SAME call is
//! written -- and every one of those call sites is right here, not
//! duplicated at `Conway`'s own methods (which now call into this module
//! instead of restating the sequence themselves).
//!
//! `Conway`'s own public methods
//! (`load_permission_files`/`trust_permission_file`/
//! `revoke_permission_pattern`/`revoke_structured_allow_rule`) keep their
//! full public documentation -- unchanged signatures, unchanged behavior --
//! and now delegate to this module's functions rather than containing the
//! logic inline. See each `Conway` method's own doc in `conway.rs` for the
//! full contract; this module's own doc comments are the maintainer-facing
//! ones, describing the mechanism rather than restating the contract a
//! second time.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use conway_core::agent::PermissionScope;
use conway_core::ids::{AgentId, ToolName};
use conway_core::permission_pattern::{
    self, PatternOrigin, Rule, RuleRegistrationError, RuleRegistrationReason, Select, Then, When,
};
use conway_runtime::runtime::Runtime;

/// The result of `Conway::load_permission_files` -- what `conway-cli`'s
/// startup loader needs to update `AppState` with.
#[derive(Debug, Clone, Default)]
pub struct PermissionLoadReport {
    /// Every candidate path considered, project-first then global, in the
    /// same precedence `crate::config::discovery::permission_file_paths`
    /// establishes -- present or not, so a caller wanting to trust a
    /// project file (`/trust permissions`) knows which path that is.
    pub paths: Vec<PathBuf>,
    /// Human-readable notices for anything the caller should surface
    /// (currently: a project file's allow rules skipped for lack of
    /// trust). Never an error -- see `Conway::load_permission_files`'s own
    /// doc for why every condition here is a silent, narrowing degrade by
    /// design. Distinct from [`Self::parse_errors`]: a notice describes a
    /// file that DID load, just not fully in effect yet (an untrusted
    /// project file's `allow` half); a parse error describes a file that
    /// did NOT load at all.
    pub notices: Vec<String>,
    /// F12: typed registration errors for rules the loader refused to
    /// install silently -- currently, a `command_prefix` rule paired with a
    /// tool whose `render_kind` is `Structured` (a rule that can never
    /// reliably match). Surfaced as a typed value, not folded into
    /// `notices`, so the caller can render it distinctly and a test can pin
    /// it (untrusted input -> typed errors, never a panic). The rule is
    /// carried whole so the operator sees exactly what was rejected.
    pub registration_errors: Vec<RuleRegistrationError>,
    /// One human-readable message per candidate file that named a
    /// top-level key this schema does not recognize
    /// (`conway_core::permission_pattern::permission_file_unknown_field_error`)
    /// -- a misspelled `"denys"` being the motivating case. A file listed
    /// here contributed ZERO rules -- allow, deny, AND prompt -- to this
    /// load; unlike every other condition [`Self::notices`] carries, this
    /// one IS the caller's signal that a file failed to load, not merely
    /// that it loaded with something degraded. Kept separate from
    /// [`Self::registration_errors`] (which is always about one
    /// already-parsed [`Rule`], never about the file failing to parse at
    /// all).
    pub parse_errors: Vec<String>,
}

/// The result of `Conway::trust_permission_file` -- the trust-path parallel
/// of [`PermissionLoadReport`]. Carries the count of allow rules actually
/// installed AND the typed registration errors for rules the broker
/// refused to install (today: a `paths_under` prefix that fails to
/// canonicalize, B3). The count and the errors are surfaced together so
/// `/trust permissions` can never report "N installed" when fewer than N
/// actually installed -- the trust path's version of the silent-no-op the
/// load path already surfaces through `PermissionLoadReport::
/// registration_errors`.
#[derive(Debug, Clone, Default)]
pub struct TrustPermissionReport {
    /// The number of allow rules actually installed by the broker (rules
    /// the broker dropped are NOT counted -- distinct from the raw parse
    /// count).
    pub installed: usize,
    /// Typed registration errors for rules the broker refused to install --
    /// surfaced to the operator through the SAME `Entry::Error { fatal:
    /// false }` channel `PermissionLoadReport::registration_errors` uses,
    /// so a registration failure reaches the operator instead of being
    /// produced and discarded.
    pub registration_errors: Vec<RuleRegistrationError>,
    /// A4: operator-visible notices for rules the broker DID install but
    /// that are partially inert (today: a `command_prefix` rule selecting a
    /// MIX of `Structured`- and `ShellCommand`-rendering tools -- the
    /// `ShellCommand` members install and match, the `Structured` members
    /// can never match and the operator is warned). Surfaced through the
    /// SAME `Entry::Notice` channel `PermissionLoadReport::notices` uses,
    /// so a partially-inert rule is never silently installed.
    pub notices: Vec<String>,
}

/// The result of `Conway::revoke_permission_pattern` -- what happened to
/// the in-session grant AND to whatever file it came from, so the caller
/// can tell the operator the whole truth rather than folding a failed
/// persist into a blanket "done".
#[derive(Debug, Clone)]
pub enum RevokeOutcome {
    /// No installed grant matched `(rule, origin)` -- nothing to revoke
    /// (already gone, e.g. a stale row left over from an earlier action in
    /// the same session).
    NotFound,
    /// Revoked for this session. `origin` was
    /// [`PatternOrigin::Interactive`] -- there was never a file backing
    /// this grant, so none was touched.
    RevokedNoFile,
    /// Revoked for this session AND removed from the file it came from.
    /// `retrust_warning`, when present, means the file was a TRUSTED
    /// project-scoped file whose bytes the rewrite just changed (which
    /// changes its content digest) and re-recording trust for the new
    /// bytes failed -- the revoke itself still fully succeeded, but the
    /// file's OTHER allow rules will require `/trust permissions` again
    /// until this is fixed. See `Conway::revoke_permission_pattern`'s own
    /// doc for why re-trusting here is the correct call, not a loophole.
    RevokedAndPersisted { retrust_warning: Option<String> },
    /// Revoked for this session, but the file it came from could not be
    /// rewritten. The rule no longer applies THIS session -- nothing on
    /// disk changed, so it returns at the next restart unless the file is
    /// fixed by hand. Revocation never fails open: the in-session grant is
    /// gone either way; only the DURABILITY of that removal failed, and
    /// this variant exists so the caller can say so rather than reporting a
    /// plain success that the next launch would quietly contradict.
    RevokedButPersistFailed { error: String },
}

/// A4: the outcome of [`validate_rule_registration`] -- either a hard
/// reject (the rule is refused installation and surfaced as a typed
/// `RuleRegistrationError` through `registration_errors`), or a notice (the
/// rule installs but the operator is warned a part of it is inert,
/// surfaced through `notices`). `None` means the rule is clean. This split
/// exists so the broadened `command_prefix`-on-`Structured` check can
/// distinguish the fully-inert all-`Structured` case (a hard reject -- no
/// working member to preserve) from the mixed `Structured`+`ShellCommand`
/// case (a notice -- the `ShellCommand` members install and the operator is
/// warned the `Structured` members are inert). Both arms are typed values,
/// never panics on untrusted input; both are operator-visible (one as a
/// transcript error, one as a transcript notice).
pub(crate) enum RegistrationCheck {
    Reject(RuleRegistrationError),
    Notice(String),
}

/// F12: validates a parsed [`Rule`] against the registered tools, the
/// single registration check the structured form needs. Returns a typed
/// [`RuleRegistrationError`] for a rule this loader will refuse to install
/// silently rather than store inert -- the mirror of the `68ea9b1`
/// `read:*`-matched-nothing bug. Two checks today:
/// (1) `when: command_prefix` paired with a `select: tools([t])` whose
/// resolved `render_kind` is `Structured` (a JSON dump whose token
/// boundaries the operator cannot predict);
/// (2) B1: `when: paths_under` paired with a `then: deny`/`prompt` rule
/// whose `select: tools([t...])` contains any exactly-named tool whose
/// resolved `PathArgs` is not `Named` (`Unconfinable` such as `bash`, or
/// `None`) -- a `paths_under` predicate can never confine such a tool, so
/// the rule is silently inert and fail-OPEN for deny/prompt. A `tools`
/// pattern naming an UNKNOWN tool is NOT a registration error here -- the
/// broker simply never matches it, and an unknown tool can be registered
/// later in the same session; refusing it at load time would be a
/// load-order hazard. A `Select::Categories` and a trailing-`*` wildcard
/// are not inspectable here (members may register later) and are left to
/// the decision-time fail-closed in `rule_denies_or_prompts`.
///
/// A4 broadens check (1) beyond a single-tool `Select::Tools`. See the
/// exact rejection/notice split at each match arm below -- copied
/// unchanged from this function's original home on `Conway` (this move is
/// a relocation, not a rewrite of the decision itself).
pub(crate) fn validate_rule_registration(rt: &Runtime, rule: &Rule) -> Option<RegistrationCheck> {
    match (&rule.select, &rule.when) {
        // A4: `command_prefix` on a Structured-rendering tool is inert for
        // that tool. Resolve the select (exact tools, trailing-`*`
        // wildcards, and categories -- all via `select_matches` over the
        // registered-tools metadata) and count Structured vs ShellCommand
        // members. Unknown tools are skipped (load-order hazard, mirroring
        // the single-tool check's `None` arm).
        (Select::Tools(_), When::CommandPrefix(_))
        | (Select::Categories(_), When::CommandPrefix(_)) => {
            let (structured, shell) = command_prefix_resolved_kinds(rt, rule);
            if structured > 0 && shell == 0 {
                Some(RegistrationCheck::Reject(RuleRegistrationError {
                    rule: rule.clone(),
                    reason: RuleRegistrationReason::CommandPrefixOnStructuredTool,
                }))
            } else if structured > 0 {
                Some(RegistrationCheck::Notice(format!(
                    "a `command_prefix` rule selecting {} matches no `Structured`-rendering \
                     tool it selects ({} of its selected tools render a JSON dump whose \
                     token boundaries the operator cannot predict); the `ShellCommand` \
                     members install, but the `Structured` members are inert -- split the \
                     rule if you meant the `Structured` tools to match, or use `always` \
                     (the `tool:*` flat form)",
                    rule.describe(),
                    structured,
                )))
            } else {
                None
            }
        }
        // B1: a `paths_under` rule can never confine a tool whose
        // `PathArgs` is not `Named`. For `then: deny/prompt` selecting an
        // `Unconfinable` tool (e.g. `bash`) that inertness is fail-OPEN --
        // the command can still reach the prefix, so the call the operator
        // expected to be refused instead goes through. For a `None` tool
        // (no path args) the rule is a no-op rather than fail-open, but a
        // no-op deny is still a trap worth surfacing. In both cases the
        // loader refuses to install it silently and surfaces a typed
        // error. For `then: allow` the same inertness is fail-CLOSED (the
        // broker simply never matches it and the call falls through to the
        // gate), so it is NOT raised here.
        //
        // Multi-tool / mixed Select: fire when ANY exactly-named selected
        // tool resolves to a non-`Named` `PathArgs` (an unenforceable deny
        // rule fails closed -- if any tool in the select can't be
        // path-confined, the deny/prompt rule is silently inert for that
        // tool, which is the hazard; the operator is informed and can
        // split the rule). A trailing-`*` wildcard pattern is NOT
        // resolvable to a single tool here (no tool is named `*`), so it
        // is skipped at install time -- the decision-time fail-closed in
        // `rule_denies_or_prompts` covers it. An unknown tool (`path_args
        // == None`) is skipped too, mirroring the CommandPrefix check's
        // load-order-hazard reasoning. A `Select::Categories` is not
        // inspectable at install time (its member tools may register
        // later in the same session), so it is left to the decision-time
        // fail-closed as well.
        (Select::Tools(ts), When::PathsUnder(_))
            if rule.then == Then::Deny || rule.then == Then::Prompt =>
        {
            ts.iter().find_map(|p| {
                // A trailing-`*` wildcard cannot be resolved to one tool.
                if p == "*" || p.ends_with('*') {
                    return None;
                }
                match rt.tool_path_args(&ToolName::new(p)) {
                    Some(conway_core::ports::PathArgs::Named(_)) | None => None,
                    // `Unconfinable` or `None` (or any future
                    // `#[non_exhaustive]` variant): a `paths_under` rule
                    // can never confine this tool -- fail closed.
                    Some(_) => Some(RegistrationCheck::Reject(RuleRegistrationError {
                        rule: rule.clone(),
                        reason: RuleRegistrationReason::PathsUnderOnUnconfinedTool,
                    })),
                }
            })
        }
        _ => None,
    }
}

/// A4: resolves a `command_prefix` rule's `select` against the registered
/// tools and returns `(structured_count, shell_count)` -- how many
/// resolvable selected tools render `Structured` vs `ShellCommand`.
/// Unknown tools are skipped (load-order hazard). Uses
/// `Runtime::registered_tools_metadata` (the registry enumeration) plus
/// `Rule::select_matches`, which handles exact `Tools` names, trailing-`*`
/// wildcards, and `Categories` membership uniformly -- so the broadened
/// check does not reimplement wildcard or category resolution, it reuses
/// the SAME `select_matches` the decision-time evaluator already uses (no
/// second implementation to drift).
pub(crate) fn command_prefix_resolved_kinds(rt: &Runtime, rule: &Rule) -> (usize, usize) {
    let mut structured = 0usize;
    let mut shell = 0usize;
    for (name, cat, rk) in rt.registered_tools_metadata() {
        if rule.select_matches(name.as_str(), cat) {
            match rk {
                conway_core::ports::RenderKind::Structured => structured += 1,
                _ => shell += 1,
            }
        }
    }
    (structured, shell)
}

/// F12: installs a parsed ALLOW [`Rule`] from a permissions file at
/// `origin_path`. The flat form desugars to a `Rule` too, so this is the
/// single install path for allow rules from config. Trust was already
/// confirmed by the caller ([`load_permission_files`]); this function does
/// not re-check it. Returns the `bool` from
/// `PermissionBroker::remember_pattern_rule`: `false` means the broker
/// dropped the rule -- today, the only reachable cause from the load path
/// is a [`When::PathsUnder`] prefix that cannot be canonicalized (B3). The
/// caller surfaces that as a typed
/// [`RuleRegistrationReason::PathsUnderPrefixUncanonicalizable`]
/// registration error rather than silently swallowing the `bool`.
///
/// `base` is the directory a RELATIVE `paths_under` prefix resolves
/// against (B2), computed by the caller, which knows which file the rule
/// came from -- see [`permission_rule_base`].
pub(crate) fn install_allow_rule(
    rt: &Runtime,
    rule: &Rule,
    scope: PermissionScope,
    granting_agent: AgentId,
    origin_path: PathBuf,
    base: &Path,
) -> bool {
    rt.permission_broker().remember_pattern_rule(
        rule.clone(),
        scope,
        granting_agent,
        PatternOrigin::File(origin_path),
        base,
    )
}

/// F12: installs a parsed DENY [`Rule`] from a permissions file at
/// `origin_path`. No trust precondition (D4 §3). Returns the `bool` from
/// `PermissionBroker::remember_deny_rule` -- see [`install_allow_rule`] for
/// the `false` contract (B3). `base` is as in [`install_allow_rule`].
pub(crate) fn install_deny_rule(
    rt: &Runtime,
    rule: &Rule,
    origin_path: PathBuf,
    base: &Path,
) -> bool {
    rt.permission_broker()
        .remember_deny_rule(rule.clone(), PatternOrigin::File(origin_path), base)
}

/// F12: installs a parsed PROMPT [`Rule`] from a permissions file at
/// `origin_path`. No trust precondition (extension-architecture.md §5.5
/// stage 1). Returns the `bool` from `PermissionBroker::
/// remember_prompt_rule` -- see [`install_allow_rule`] for the `false`
/// contract (B3). `base` is as in [`install_allow_rule`].
pub(crate) fn install_prompt_rule(
    rt: &Runtime,
    rule: &Rule,
    origin_path: PathBuf,
    base: &Path,
) -> bool {
    rt.permission_broker().remember_prompt_rule(
        rule.clone(),
        PatternOrigin::File(origin_path),
        base,
    )
}

/// B3: when an `install_*_rule` call returns `false` for a
/// [`When::PathsUnder`] rule, the broker dropped it because
/// `canonicalize_when` could not resolve the prefix on disk (a typo, or a
/// repo/subdirectory not yet cloned/checked out). Surface that as a typed
/// [`RuleRegistrationReason::PathsUnderPrefixUncanonicalizable`]
/// registration error instead of silently swallowing the `bool` -- the
/// mirror of `68ea9b1`'s `read:*`-matched-nothing bug. Returns `None` for
/// every other `when` clause: a non-`PathsUnder` rule's `remember_*_rule`
/// never returns `false` from the load path (the `then` mismatch is an
/// invariant the load split already enforces), so a `false` there would be
/// an invariant violation rather than an operator-visible condition.
pub(crate) fn uncanonicalizable_paths_under_error(
    rule: &Rule,
    installed: bool,
) -> Option<RuleRegistrationError> {
    if !installed && matches!(rule.when, When::PathsUnder(_)) {
        Some(RuleRegistrationError {
            rule: rule.clone(),
            reason: RuleRegistrationReason::PathsUnderPrefixUncanonicalizable,
        })
    } else {
        None
    }
}

/// B2: the directory a RELATIVE `paths_under` prefix in the permissions
/// file at `path` resolves against before the broker canonicalizes it (B2,
/// finding S5). Resolving a relative prefix against the process's cwd --
/// what a bare `Path::canonicalize` does -- points the rule at wherever
/// the operator happened to launch conway from, so the base is derived
/// here, where the file's own location is known:
///
/// - For a PROJECT file (`<project>/.conway/permissions.json`): the
///   PROJECT ROOT -- the directory containing `.conway/` -- not the
///   file's own parent (`.conway/` itself), which would point a prefix
///   like `"src"` at `<project>/.conway/src`, a directory no operator
///   means. (Note the base is derived from the FILE's own location, so
///   under ancestor discovery it is the ancestor holding `.conway/`, not
///   the launch cwd: `RootSpec.root`/`SubagentSpec.root` resolve against
///   the agent cwd, which coincides with the project root only in the
///   standard launch.)
/// - For the GLOBAL file (`~/.conway/permissions.json`, or
///   `$XDG_CONFIG_HOME/conway/permissions.json`): there is no containing
///   project, so the base is the AGENT CWD the load was initiated with --
///   the one directory a global rule can meaningfully be relative to at
///   load time. (Resolving against the config directory would make `"src"`
///   mean `~/.conway/src`, which protects nothing.)
///
/// An ABSOLUTE prefix is unaffected by this choice in both cases.
pub(crate) fn permission_rule_base(path: &Path, global_path: Option<&Path>, cwd: &Path) -> PathBuf {
    if global_path == Some(path) {
        return cwd.to_path_buf();
    }
    // `<project>/.conway/permissions.json` -> `<project>`. The fallback (a
    // permissions file somehow not two components deep) is the agent cwd,
    // the same uniform choice the global file makes.
    path.parent()
        .and_then(Path::parent)
        .unwrap_or(cwd)
        .to_path_buf()
}

/// Loads permissions files project-first then global
/// (`crate::config::discovery::permission_file_paths`) and installs their
/// rules into `rt`'s broker. See `Conway::load_permission_files`'s own doc
/// (`crates/conway/src/conway.rs`) for the full precedence/asymmetry
/// contract this function implements unchanged -- this is a relocation,
/// not a rewrite.
pub(crate) fn load_permission_files(
    rt: &Runtime,
    cwd: &Path,
    env: &HashMap<String, String>,
    scope: PermissionScope,
    granting_agent: AgentId,
) -> PermissionLoadReport {
    let paths = crate::config::discovery::permission_file_paths(cwd, env);
    let global_path = crate::config::discovery::xdg_config_path(env)
        .and_then(|settings| settings.parent().map(|dir| dir.join("permissions.json")));
    let trust_store = crate::config::trust::TrustStore::load(env);
    let mut notices = Vec::new();
    let mut registration_errors = Vec::new();
    let mut parse_errors = Vec::new();

    for path in &paths {
        let Ok(contents) = std::fs::read_to_string(path) else {
            continue;
        };

        // A misspelled top-level key (`"denys"` for `"deny"`) must not
        // silently install zero rules with nothing telling the operator --
        // checked BEFORE any of `parse_deny_rules`/`parse_prompt_rules`/
        // `parse_rules` run, so a typo'd file contributes NOTHING (not
        // even its correctly-spelled rules) rather than partially
        // installing around the typo, matching `settings.json`'s own "a
        // bad key rejects the whole file" precedent.
        if let Some(err) = permission_pattern::permission_file_unknown_field_error(&contents) {
            parse_errors.push(format!(
                "{} was not loaded: {err} -- fix or remove the unrecognized key; \
                 no rules (allow, deny, or prompt) from this file are in effect \
                 until it is fixed",
                path.display()
            ));
            continue;
        }

        let is_global = global_path.as_deref() == Some(path.as_path());
        // B2: the base a relative `paths_under` prefix in THIS file
        // resolves against -- the project root for a project file, the
        // agent cwd for the global file.
        let base = permission_rule_base(path, global_path.as_deref(), cwd);

        // Deny applies unconditionally, from every scope, regardless of
        // trust -- D4 §3. F12: this now also covers structured `then:
        // deny` rules from the `rules` array (`parse_deny_rules` returns
        // the union of flat `deny` and structured `then: deny`).
        for rule in permission_pattern::parse_deny_rules(&contents) {
            match validate_rule_registration(rt, &rule) {
                Some(RegistrationCheck::Reject(err)) => {
                    registration_errors.push(err);
                    continue;
                }
                Some(RegistrationCheck::Notice(msg)) => {
                    notices.push(msg);
                }
                None => {}
            }
            // B3: a `paths_under` prefix that fails to canonicalize is
            // dropped by the broker; surface it instead of silently
            // swallowing the `bool` (deny rules apply unconditionally, so
            // the operator believed this was protecting them).
            let installed = install_deny_rule(rt, &rule, path.clone(), &base);
            if let Some(err) = uncanonicalizable_paths_under_error(&rule, installed) {
                registration_errors.push(err);
            }
        }

        // F12: prompt rules apply unconditionally too (narrowing, D4 §3
        // extended to `prompt` -- extension-architecture.md §5.5 stage 1).
        // The flat form has no prompt syntax, so these come entirely from
        // the structured `rules` array.
        for rule in permission_pattern::parse_prompt_rules(&contents) {
            match validate_rule_registration(rt, &rule) {
                Some(RegistrationCheck::Reject(err)) => {
                    registration_errors.push(err);
                    continue;
                }
                Some(RegistrationCheck::Notice(msg)) => {
                    notices.push(msg);
                }
                None => {}
            }
            // B3: same surfacing as the deny arm -- a dropped `prompt`
            // narrowing rule is a trap the operator will not notice.
            let installed = install_prompt_rule(rt, &rule, path.clone(), &base);
            if let Some(err) = uncanonicalizable_paths_under_error(&rule, installed) {
                registration_errors.push(err);
            }
        }

        let trusted = is_global || trust_store.is_trusted(path, &contents);
        let allow_rules = permission_pattern::parse_rules(&contents);
        if !trusted {
            if !allow_rules.is_empty() {
                notices.push(format!(
                    "project permissions file {} has {} allow rule(s) that \
                     require an explicit trust decision before they take \
                     effect -- run `/trust permissions` to review and \
                     trust it (its `deny` rules, if any, already apply)",
                    path.display(),
                    allow_rules.len()
                ));
            }
            continue;
        }
        for rule in allow_rules {
            match validate_rule_registration(rt, &rule) {
                Some(RegistrationCheck::Reject(err)) => {
                    registration_errors.push(err);
                    continue;
                }
                Some(RegistrationCheck::Notice(msg)) => {
                    notices.push(msg);
                }
                None => {}
            }
            // B3: same surfacing as the deny/prompt arms -- a dropped
            // `paths_under` allow rule is fail-CLOSED (the call falls
            // through to the gate), but the operator still deserves to
            // know their rule did nothing.
            let installed =
                install_allow_rule(rt, &rule, scope, granting_agent, path.clone(), &base);
            if let Some(err) = uncanonicalizable_paths_under_error(&rule, installed) {
                registration_errors.push(err);
            }
        }
    }

    PermissionLoadReport {
        paths,
        notices,
        registration_errors,
        parse_errors,
    }
}

/// Records an explicit trust decision for `path`'s CURRENT bytes on disk
/// (`crate::config::trust::TrustStore::trust`) and immediately installs
/// its `allow` rules for this running session. See `Conway::
/// trust_permission_file`'s own doc (`crates/conway/src/conway.rs`) for
/// the full contract -- this is a relocation, not a rewrite. `cwd` is the
/// owning `Conway`'s own configured cwd (`self.config.cwd`), passed
/// explicitly since this function, unlike a `Conway` method, has no
/// `self.config` to read.
pub(crate) fn trust_permission_file(
    rt: &Runtime,
    env: &HashMap<String, String>,
    path: &Path,
    scope: PermissionScope,
    granting_agent: AgentId,
    cwd: &Path,
) -> std::io::Result<TrustPermissionReport> {
    let contents = std::fs::read_to_string(path)?;
    // Refuse a file naming an unrecognized top-level key BEFORE recording
    // a trust decision for it -- a typo'd file's rules were never going to
    // install anyway (see `permission_file_unknown_field_error`'s own
    // doc), so trusting it first would record a decision for content that
    // installs nothing, silently, on every subsequent load.
    if let Some(err) = permission_pattern::permission_file_unknown_field_error(&contents) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} was not trusted: {err} -- fix or remove the unrecognized key first",
                path.display()
            ),
        ));
    }
    crate::config::trust::TrustStore::trust(env, path)?;
    let rules = permission_pattern::parse_rules(&contents);
    // B2: the same relative-`paths_under` base `load_permission_files`
    // computes, so a rule installs with the SAME boundary whether it took
    // effect at startup (already-trusted file) or here (`/trust
    // permissions` mid-session). For the project file -- the only file
    // the TUI's `/trust permissions` ever targets -- the base is the
    // project root derived from the path itself, identical either way.
    // For the global file (no containing project) the base is the passed
    // `cwd`, the same choice `load_permission_files` makes with its
    // explicit `cwd`.
    let global_path = crate::config::discovery::xdg_config_path(env)
        .and_then(|settings| settings.parent().map(|dir| dir.join("permissions.json")));
    let base = permission_rule_base(path, global_path.as_deref(), cwd);
    let mut installed = 0;
    let mut registration_errors = Vec::new();
    let mut notices = Vec::new();
    for rule in rules {
        match validate_rule_registration(rt, &rule) {
            Some(RegistrationCheck::Reject(err)) => {
                // A registration error means the rule was never going to
                // match; do not count it as installed. `trust_permission_file`
                // is operator-triggered (`/trust permissions`), so the
                // operator will have already seen THIS class of registration
                // error (e.g. `PathsUnderOnUnconfinedTool`) from the prior
                // `load_permission_files` -- re-trusting does not silently
                // swallow it, it just does not re-report the structurally-
                // invalid cases here. B3's `PathsUnderPrefixUncanonicalizable`
                // is a DIFFERENT class: it is not caught by
                // `validate_rule_registration` (the prefix's existence on
                // disk is not a structural property), so it surfaces below
                // via the install `bool`, and IS re-reported here -- a
                // bad-prefix rule trusted mid-session must not silently
                // inflate the count.
                registration_errors.push(err);
                continue;
            }
            Some(RegistrationCheck::Notice(msg)) => {
                notices.push(msg);
            }
            None => {}
        }
        // B3: honor the install `bool` -- a rule the broker dropped
        // (today: a `paths_under` prefix that fails to canonicalize) must
        // NOT count as installed, and the operator must be told.
        let was_installed =
            install_allow_rule(rt, &rule, scope, granting_agent, path.to_path_buf(), &base);
        if let Some(err) = uncanonicalizable_paths_under_error(&rule, was_installed) {
            registration_errors.push(err);
            continue;
        }
        installed += 1;
    }
    Ok(TrustPermissionReport {
        installed,
        registration_errors,
        notices,
    })
}

/// Removes `rule`'s wire form from `path`'s `allow` list, tmp-then-rename
/// -- see `Conway::revoke_permission_pattern`'s own doc for the full
/// reasoning (why a parse failure is a hard error here, unlike the append
/// path; why no chmod hardening).
pub(crate) fn rewrite_permission_file_removing(
    path: &Path,
    rule: &conway_core::permission_pattern::PatternRule,
) -> std::io::Result<()> {
    let contents = std::fs::read_to_string(path)?;
    let mut file: permission_pattern::PermissionFile =
        serde_json::from_str(&contents).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} is not valid JSON, refusing to rewrite it blindly: {e}",
                    path.display()
                ),
            )
        })?;

    let wire = rule.to_wire();
    let before = file.allow.len();
    file.allow.retain(|w| w != &wire);
    if file.allow.len() == before {
        // Nothing to remove -- the goal state already holds. No write, so
        // there is nothing that could de-trust the file either.
        return Ok(());
    }

    let serialized = serde_json::to_string_pretty(&file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Removes `rule` from `path`'s structured `rules` array, tmp-then-rename
/// -- the structured counterpart to [`rewrite_permission_file_removing`],
/// for `Conway::revoke_structured_allow_rule`. Same posture throughout: an
/// unparseable file is a hard error (never blindly overwritten), a rule
/// already absent from an otherwise-valid file means the goal state
/// already holds so nothing is written, and every OTHER entry -- flat
/// `allow`/`deny` lists and the rest of `rules`, including the array's
/// `deny`/`prompt` entries -- is preserved verbatim. Matches by `Rule`
/// equality, the same identity the broker matched in memory, so the row
/// the operator selected is the entry removed from disk.
pub(crate) fn rewrite_permission_file_removing_structured(
    path: &Path,
    rule: &Rule,
) -> std::io::Result<()> {
    let contents = std::fs::read_to_string(path)?;
    let mut file: permission_pattern::PermissionFile =
        serde_json::from_str(&contents).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} is not valid JSON, refusing to rewrite it blindly: {e}",
                    path.display()
                ),
            )
        })?;

    // Remove exactly ONE matching entry -- the broker dropped exactly one
    // in-memory instance, so the file must lose exactly one too, or a
    // hand-duplicated entry would diverge durable state from session
    // state (the operator revokes one row, one instance survives in
    // memory, but a `retain` here would strip both from disk and the
    // "surviving" grant would silently vanish at the next restart). The
    // flat path's `retain` predates this reasoning; matching it is
    // consistency, fixing it here is the new code keeping its own
    // one-instance contract.
    let Some(idx) = file.rules.iter().position(|r| r == rule) else {
        // Nothing to remove -- the goal state already holds. No write, so
        // there is nothing that could de-trust the file either.
        return Ok(());
    };
    file.rules.remove(idx);

    let serialized = serde_json::to_string_pretty(&file)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serialized)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The shared persistence tail of `Conway::revoke_permission_pattern` and
/// `Conway::revoke_structured_allow_rule`: folds the file rewrite's result
/// into a [`RevokeOutcome`], re-trusting a rewritten trusted project file
/// (never the global file, which is trusted by authorship and never gated
/// on a digest at all) so the rewrite does not silently de-trust the
/// file's OTHER rules. The in-session grant is already gone when this
/// runs -- a failure here only means the removal is not durable, reported
/// honestly via [`RevokeOutcome::RevokedButPersistFailed`] /
/// [`RevokeOutcome::RevokedAndPersisted`]'s `retrust_warning` rather than
/// folded into a blanket "done".
pub(crate) fn persist_revoke_outcome(
    env: &HashMap<String, String>,
    path: &Path,
    rewrite: std::io::Result<()>,
) -> RevokeOutcome {
    match rewrite {
        Err(e) => RevokeOutcome::RevokedButPersistFailed {
            error: e.to_string(),
        },
        Ok(()) => {
            let global_path = crate::config::discovery::xdg_config_path(env)
                .and_then(|settings| settings.parent().map(|dir| dir.join("permissions.json")));
            let is_global = global_path.as_deref() == Some(path);
            let retrust_warning = if is_global {
                None
            } else {
                match crate::config::trust::TrustStore::trust(env, path) {
                    Ok(()) => None,
                    Err(e) => Some(format!(
                        "could not re-trust {} after removing a rule from it -- \
                         its other allow rules will need `/trust permissions` \
                         again ({e})",
                        path.display()
                    )),
                }
            };
            RevokeOutcome::RevokedAndPersisted { retrust_warning }
        }
    }
}

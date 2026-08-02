//! `Conway`: the live, assembled facade over one `conway-runtime::Runtime`
//! (WI-100). Constructed exclusively via `crate::builder::ConwayBuilder::build`.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use conway_core::agent::{AgentDefRef, AgentStatus, Budget, SubagentMode};
use conway_core::capabilities::RequiredCaps;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::ids::{AgentId, LogSeq, RoleAlias, SeqRange, SessionId};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::SessionStore;
use conway_core::provenance::Provenance;
use conway_core::routing::RouteRequest;
use conway_routing::{DeclarativeRouter, ExplainReport, RoutingExplain};
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime};

use crate::config::model_metadata::ModelMetadata;
use crate::config::{ConfigWarning, ConwayConfig};
use crate::error::{ConwayError, Result};
use crate::intent::AgentIntent;
use crate::session_handle::{SessionHandle, SessionSpec};
use crate::subagent_spec::ForkSpec;

/// The result of [`Conway::load_permission_files`] (board item
/// 01KYT8SGX32CP56PRJNG72V2W5) -- what `conway-cli`'s startup loader needs
/// to update `AppState` with.
#[derive(Debug, Clone, Default)]
pub struct PermissionLoadReport {
    /// Every candidate path considered, project-first then global, in the
    /// same precedence `crate::config::discovery::permission_file_paths`
    /// establishes -- present or not, so a caller wanting to trust a
    /// project file (`/trust permissions`) knows which path that is.
    pub paths: Vec<std::path::PathBuf>,
    /// Human-readable notices for anything the caller should surface
    /// (currently: a project file's allow rules skipped for lack of
    /// trust). Never an error -- see [`Conway::load_permission_files`]'s
    /// own doc for why every condition here is a silent, narrowing
    /// degrade by design.
    pub notices: Vec<String>,
    /// F12: typed registration errors for rules the loader refused to
    /// install silently -- currently, a `command_prefix` rule paired with
    /// a tool whose `render_kind` is `Structured` (a rule that can never
    /// reliably match). Surfaced as a typed value, not folded into
    /// `notices`, so the caller can render it distinctly and a test can
    /// pin it (P-10: untrusted input -> typed errors). The rule is carried
    /// whole so the operator sees exactly what was rejected.
    pub registration_errors: Vec<conway_core::permission_pattern::RuleRegistrationError>,
}

/// The result of [`Conway::revoke_permission_pattern`] (board item
/// 01KYND4WGHSZXW5YQ6ZWHCDDNN): what happened to the in-session grant AND to
/// whatever file it came from, so the caller can tell the operator the
/// whole truth rather than folding a failed persist into a blanket "done".
#[derive(Debug, Clone)]
pub enum RevokeOutcome {
    /// No installed grant matched `(rule, origin)` -- nothing to revoke
    /// (already gone, e.g. a stale row left over from an earlier action in
    /// the same session).
    NotFound,
    /// Revoked for this session. `origin` was
    /// [`conway_core::permission_pattern::PatternOrigin::Interactive`] --
    /// there was never a file backing this grant, so none was touched.
    RevokedNoFile,
    /// Revoked for this session AND removed from the file it came from.
    /// `retrust_warning`, when present, means the file was a TRUSTED
    /// project-scoped file whose bytes the rewrite just changed (which
    /// changes its content digest) and re-recording trust for the new
    /// bytes failed -- the revoke itself still fully succeeded, but the
    /// file's OTHER allow rules will require `/trust permissions` again
    /// until this is fixed. See [`Conway::revoke_permission_pattern`]'s own
    /// doc for why re-trusting here is the correct call, not a loophole.
    RevokedAndPersisted { retrust_warning: Option<String> },
    /// Revoked for this session, but the file it came from could not be
    /// rewritten. The rule no longer applies THIS session -- nothing on
    /// disk changed, so it returns at the next restart unless the file is
    /// fixed by hand. Revocation never fails open: the in-session grant is
    /// gone either way; only the DURABILITY of that removal failed, and
    /// this variant exists so the caller can say so rather than reporting
    /// a plain success that the next launch would quietly contradict.
    RevokedButPersistFailed { error: String },
}

/// The live, assembled facade: one `Runtime`, its resolved config, and (when
/// the builder compiled its own router rather than receiving an injected
/// one) the concrete router `explain_routing` projects through.
///
/// Cheap to `Clone`: every field is an `Arc`.
#[derive(Clone)]
pub struct Conway {
    rt: Arc<Runtime>,
    config: Arc<ConwayConfig>,
    // Also cloned into every `SessionHandle` this `Conway` mints
    // (`new_session`), which needs it for `SessionHandle::transcript`'s
    // ancestry walk (WI-101).
    store: Arc<dyn SessionStore>,
    router_explain: Option<Arc<DeclarativeRouter>>,
    warnings: Arc<Vec<ConfigWarning>>,
    // T3 follow-up: the local model-metadata map `ConwayBuilder::build`
    // already loads (`[models.metadata_path]`, step 2) to construct the
    // `CapabilityIndex` -- kept here too so `Self::model_metadata` can hand
    // the SAME loaded map back out. Before this field existed, every
    // consumer of that file (the TUI's `App::new`, ~app.rs) re-read and
    // re-parsed it from disk on its own, a second code path that agreed
    // with the builder's only by coincidence.
    model_metadata: Arc<ModelMetadata>,
    /// Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: set via
    /// `ConwayBuilder::with_root` -- see that method's own doc for the
    /// default (`None`, unconfined, unchanged) and the operator-facing
    /// contract. Consulted once per [`Self::new_session`] call, exactly like
    /// `self.config.cwd`.
    root: Option<std::path::PathBuf>,
}

/// How fresh a store liveness marker must be for `sweep_stale_modal_asks` to
/// treat it as a live owner and defer. 60s = 4× the TUI's 15s heartbeat
/// interval, so a few missed beats under load do not flip a live owner to
/// "stale". A crashed process stops heartbeating, so its marker crosses this
/// threshold shortly after death and the NEXT startup reaps its residue. See
/// [`Conway::sweep_stale_modal_asks`] (S1 follow-up to B5).
const SWEEP_LIVE_THRESHOLD: ChronoDuration = ChronoDuration::seconds(60);

impl Conway {
    pub(crate) fn new(
        rt: Arc<Runtime>,
        config: ConwayConfig,
        store: Arc<dyn SessionStore>,
        router_explain: Option<Arc<DeclarativeRouter>>,
        warnings: Vec<ConfigWarning>,
        model_metadata: ModelMetadata,
        root: Option<std::path::PathBuf>,
    ) -> Self {
        Self {
            rt,
            config: Arc::new(config),
            store,
            router_explain,
            warnings: Arc::new(warnings),
            model_metadata: Arc::new(model_metadata),
            root,
        }
    }

    /// Non-fatal warnings surfaced by `config::load` (currently only
    /// headroom-vs-context-window warnings). Empty when this `Conway` was
    /// built via `ConwayBuilder::from_parts`, which bypasses `load` entirely.
    pub fn warnings(&self) -> &[ConfigWarning] {
        &self.warnings
    }

    /// The resolved configuration this `Conway` was built from.
    /// The active permission mode (V2b).
    ///
    /// Read from the broker, which is the authority — the TUI keeps a
    /// display mirror for the status line, but that mirror is refreshed
    /// from here rather than trusted on its own.
    pub fn permission_mode(&self) -> conway_core::permission_mode::PermissionMode {
        self.rt.permission_broker().mode()
    }

    /// Switches the permission mode at runtime (V2b). This is the escape
    /// hatch out of an over-broad mode: no restart required.
    pub fn set_permission_mode(&self, mode: conway_core::permission_mode::PermissionMode) {
        self.rt.permission_broker().set_mode(mode);
    }

    /// Installs a pattern ALLOW grant approved through the interactive
    /// gate (V2b) -- origin `Interactive`. Use
    /// [`Self::grant_permission_pattern_from_file`] for a rule loaded from
    /// a permissions file, so the review surface can tell the two apart
    /// (board item 01KYT8SGX32CP56PRJNG72V2W5).
    ///
    /// Note the metacharacter gate is NOT applied here: it lives in
    /// `PatternRule::matches_render` and is evaluated against each
    /// incoming command at decision time. Gating at install time instead
    /// would let a rule loaded from a file bypass it.
    pub fn grant_permission_pattern(
        &self,
        rule: conway_core::permission_pattern::PatternRule,
        scope: conway_core::agent::PermissionScope,
        granting_agent: conway_core::ids::AgentId,
    ) {
        self.rt.permission_broker().remember_pattern(
            rule,
            scope,
            granting_agent,
            conway_core::permission_pattern::PatternOrigin::Interactive,
        );
    }

    /// Installs a pattern ALLOW grant loaded from a permissions file at
    /// `origin_path` (board item 01KYT8SGX32CP56PRJNG72V2W5).
    ///
    /// **This method trusts its caller completely.** It installs whatever
    /// it is given. The TRUST DECISION for a project-scoped file --
    /// whether its content matches a recorded, explicit trust record --
    /// belongs entirely to the caller (`conway-cli`'s startup loader,
    /// `crate::config::trust::TrustStore`), made ONCE before this is ever
    /// invoked. This method must never be the enforcement point, or there
    /// would be two places that decision could drift apart.
    pub fn grant_permission_pattern_from_file(
        &self,
        rule: conway_core::permission_pattern::PatternRule,
        scope: conway_core::agent::PermissionScope,
        granting_agent: conway_core::ids::AgentId,
        origin_path: std::path::PathBuf,
    ) {
        self.rt.permission_broker().remember_pattern(
            rule,
            scope,
            granting_agent,
            conway_core::permission_pattern::PatternOrigin::File(origin_path),
        );
    }

    /// Installs a DENY rule loaded from a permissions file at
    /// `origin_path`. Unlike the allow-side methods above, there is no
    /// trust precondition here at all -- `deny` applies immediately,
    /// trusted or not, from any file (D4 §3, board item
    /// 01KYT8SGX32CP56PRJNG72V2W5).
    pub fn grant_deny_pattern(
        &self,
        rule: conway_core::permission_pattern::PatternRule,
        origin_path: std::path::PathBuf,
    ) {
        self.rt.permission_broker().remember_deny_pattern(
            rule,
            conway_core::permission_pattern::PatternOrigin::File(origin_path),
        );
    }

    /// Installs a PROMPT rule loaded from a permissions file at
    /// `origin_path` (board item 01KYTP1D3XWEZPW4AKPH54FNB3). Mirrors
    /// [`Self::grant_deny_pattern`] exactly, including its "no trust
    /// precondition" reasoning: `prompt`, like `deny`, only ever narrows
    /// (forces an extra ask rather than skipping one), never grants, so it
    /// applies immediately, trusted or not, from any file
    /// (extension-architecture.md §5.5 stage 1).
    pub fn grant_prompt_pattern(
        &self,
        rule: conway_core::permission_pattern::PatternRule,
        origin_path: std::path::PathBuf,
    ) {
        self.rt.permission_broker().remember_prompt_pattern(
            rule,
            conway_core::permission_pattern::PatternOrigin::File(origin_path),
        );
    }

    /// F12: the `render_kind` a registered tool declares for itself, by
    /// name, or `None` if no plugin registered that tool. The structured-rule
    /// registration check (`validate_rule_registration`) uses this to refuse
    /// a `command_prefix` rule against a `Structured` tool -- a rule that can
    /// never reliably match. Reads the already-resolved tool the same way
    /// `ToolRunner::execute_one` does; no new resolution path.
    pub fn tool_render_kind(
        &self,
        name: &conway_core::ids::ToolName,
    ) -> Option<conway_core::ports::RenderKind> {
        self.rt.tool_render_kind(name)
    }

    /// F12: validates a parsed [`Rule`] against the registered tools, the
    /// single registration check the structured form needs. Returns a typed
    /// [`RuleRegistrationError`] for a rule this loader will refuse to
    /// install silently rather than store inert -- the mirror of the
    /// `68ea9b1` `read:*`-matched-nothing bug. Today the only check is:
    /// `when: command_prefix` paired with a `select: tools([t])` whose
    /// resolved `render_kind` is `Structured` (a JSON dump whose token
    /// boundaries the operator cannot predict). A `tools` pattern naming an
    /// UNKNOWN tool is NOT a registration error here -- the broker simply
    /// never matches it, and an unknown tool can be registered later in the
    /// same session; refusing it at load time would be a load-order hazard.
    fn validate_rule_registration(
        &self,
        rule: &conway_core::permission_pattern::Rule,
    ) -> Option<conway_core::permission_pattern::RuleRegistrationError> {
        use conway_core::permission_pattern::{RuleRegistrationReason, Select, When};
        let tool = match (&rule.select, &rule.when) {
            (Select::Tools(ts), When::CommandPrefix(_)) if ts.len() == 1 => ts[0].as_str(),
            _ => return None,
        };
        match self.tool_render_kind(&conway_core::ids::ToolName::new(tool)) {
            Some(conway_core::ports::RenderKind::Structured) => {
                Some(conway_core::permission_pattern::RuleRegistrationError {
                    rule: rule.clone(),
                    reason: RuleRegistrationReason::CommandPrefixOnStructuredTool,
                })
            }
            // `ShellCommand` is exactly the tool `command_prefix` was
            // designed for; `None` (unknown tool) is left for the broker to
            // never match, not a registration error (load-order hazard).
            _ => None,
        }
    }

    /// F12: installs a parsed ALLOW [`Rule`] from a permissions file at
    /// `origin_path`. The flat form desugars to a `Rule` too, so this is the
    /// single install path for allow rules from config. Trust was already
    /// confirmed by the caller (`load_permission_files`); this method does
    /// not re-check it. A [`When::PathsUnder`] prefix that cannot be
    /// canonicalized is dropped by the broker (fail closed); the caller
    /// surfaces that as a notice via the install's `bool` return when it
    /// matters (today nothing reads it here, since the registration check
    /// already rejected the structurally-invalid ones).
    fn install_allow_rule(
        &self,
        rule: conway_core::permission_pattern::Rule,
        scope: conway_core::agent::PermissionScope,
        granting_agent: conway_core::ids::AgentId,
        origin_path: std::path::PathBuf,
    ) {
        self.rt.permission_broker().remember_pattern_rule(
            rule,
            scope,
            granting_agent,
            conway_core::permission_pattern::PatternOrigin::File(origin_path),
        );
    }

    /// F12: installs a parsed DENY [`Rule`] from a permissions file at
    /// `origin_path`. No trust precondition (D4 §3).
    fn install_deny_rule(
        &self,
        rule: conway_core::permission_pattern::Rule,
        origin_path: std::path::PathBuf,
    ) {
        self.rt.permission_broker().remember_deny_rule(
            rule,
            conway_core::permission_pattern::PatternOrigin::File(origin_path),
        );
    }

    /// F12: installs a parsed PROMPT [`Rule`] from a permissions file at
    /// `origin_path`. No trust precondition (extension-architecture.md §5.5
    /// stage 1).
    fn install_prompt_rule(
        &self,
        rule: conway_core::permission_pattern::Rule,
        origin_path: std::path::PathBuf,
    ) {
        self.rt.permission_broker().remember_prompt_rule(
            rule,
            conway_core::permission_pattern::PatternOrigin::File(origin_path),
        );
    }

    /// Every active PROMPT rule, paired with its origin -- the prompt
    /// half's own review list, mirroring
    /// [`Self::active_deny_permission_patterns`].
    pub fn active_prompt_permission_patterns(
        &self,
    ) -> Vec<(
        conway_core::permission_pattern::PatternRule,
        conway_core::permission_pattern::PatternOrigin,
    )> {
        self.rt.permission_broker().active_prompt_patterns()
    }

    /// Every active pattern ALLOW grant, paired with its origin, for a
    /// review list. A rule set nobody can inspect -- or whose provenance
    /// nobody can tell -- is a trap.
    pub fn active_permission_patterns(
        &self,
    ) -> Vec<(
        conway_core::permission_pattern::PatternRule,
        conway_core::permission_pattern::PatternOrigin,
    )> {
        self.rt.permission_broker().active_patterns()
    }

    /// Every active DENY rule, paired with its origin -- the deny half's
    /// own review list.
    pub fn active_deny_permission_patterns(
        &self,
    ) -> Vec<(
        conway_core::permission_pattern::PatternRule,
        conway_core::permission_pattern::PatternOrigin,
    )> {
        self.rt.permission_broker().active_deny_patterns()
    }

    /// Drops every pattern ALLOW grant and cached `AllowAlways`, returning
    /// the session to asking (V2b). Deliberately leaves `deny` rules in
    /// force -- see `PermissionBroker::revoke_all_grants`'s own doc.
    pub fn revoke_permission_grants(&self) {
        self.rt.permission_broker().revoke_all_grants();
    }

    /// Revokes exactly ONE pattern ALLOW grant (board item
    /// 01KYND4WGHSZXW5YQ6ZWHCDDNN), addressed by the same `(rule, origin)`
    /// pair [`Self::active_permission_patterns`] already hands the review
    /// surface -- see `PermissionBroker::revoke_pattern`'s own doc for why
    /// that pair, not a position, is the identity.
    ///
    /// **Revocation never fails open.** The broker's in-memory grant is
    /// dropped FIRST, unconditionally, before any file I/O is attempted --
    /// so even if persistence below fails, this session has already
    /// stopped honoring the rule. [`RevokeOutcome`] reports the
    /// persistence half honestly instead of folding a failed write into a
    /// blanket "done" -- the failure mode this constraint exists to
    /// forbid is the REVERSE: claiming success while the rule survives on
    /// disk.
    ///
    /// `origin`'s two shapes get different treatment entirely:
    /// - [`conway_core::permission_pattern::PatternOrigin::Interactive`][]:
    ///   no file exists for this grant (an operator's "always allow", or a
    ///   rule installed with no permissions file behind it at all --
    ///   a test harness, an embedder's own code). Revoking it must not
    ///   CREATE one, so nothing is written; see
    ///   [`RevokeOutcome::RevokedNoFile`].
    /// - [`conway_core::permission_pattern::PatternOrigin::File`][]: the
    ///   rule's wire form is removed from THAT EXACT file's `allow` list --
    ///   never a different file (guessing via, say, "the first configured
    ///   project path" is exactly the project-vs-global mixup this method
    ///   exists to avoid) -- by read-modify-write, tmp-then-rename
    ///   (`tui/history.rs`'s T8 precedent: chosen over `config/trust.rs`'s
    ///   tmp-then-rename-PLUS-chmod-0600 precedent because a permissions
    ///   file is not a new secrets-adjacent file needing that extra
    ///   hardening -- it is the operator's own existing rules list,
    ///   already created at ordinary permissions by whatever wrote the
    ///   grant into it in the first place).
    ///
    ///   A file that cannot be READ or PARSED is a hard PERSIST FAILURE,
    ///   not treated as empty the way loading tolerates a corrupt file:
    ///   unlike *appending* a new rule (safe to treat corrupt content as
    ///   blank, since a corrupt file was already authorizing nothing per
    ///   [`conway_core::permission_pattern::parse_rules`]'s fail-closed
    ///   posture), blindly overwriting content this method could not
    ///   actually parse would silently discard whatever real rules --
    ///   allow OR deny -- that file held. If the rule's wire form is
    ///   simply absent from an otherwise-valid file (removed by hand
    ///   already, or a stale row), nothing is written at all -- the goal
    ///   state ("this file no longer grants this rule") is already true.
    ///
    ///   If that file is a TRUSTED PROJECT file (never the global file,
    ///   which is trusted by authorship and never gated on a digest at
    ///   all -- `Self::load_permission_files`'s own doc), the rewrite
    ///   changes its bytes, which changes its content digest --
    ///   SILENTLY DE-TRUSTING it per `crate::config::trust`'s own design,
    ///   and taking every OTHER allow rule in that file down with it at
    ///   the next restart. Because this rewrite is itself the direct
    ///   result of an explicit operator action (selecting "revoke" in
    ///   `/settings`) narrowing authority they already trusted -- never
    ///   automatic, never a side effect of anything else -- re-recording
    ///   trust for the new, strictly narrower bytes is the correct call,
    ///   not a loophole: it is exactly the "explicit, on-purpose"
    ///   trust-write D4 §5/§9 requires, just triggered from a different
    ///   command than `/trust permissions`. A failure to re-trust does
    ///   NOT undo the revoke -- the rule is still gone either way -- it
    ///   only means the file's OTHER rules degrade until `/trust
    ///   permissions` is run again, surfaced via
    ///   [`RevokeOutcome::RevokedAndPersisted`]'s `retrust_warning`.
    ///
    /// `deny` rules have no counterpart to this method at all: they are
    /// not addressable through `active_permission_patterns()` (only
    /// `active_deny_permission_patterns()` lists them), and `/settings`
    /// never renders one as a selectable row -- so there is no path that
    /// reaches here with a deny rule's identity in the first place. This
    /// mirrors `PermissionBroker::revoke_all_grants`'s own deliberate
    /// choice to leave `deny` untouched: a `deny` rule narrows rather than
    /// grants, most come from a file the operator does not control (or
    /// reviewed less carefully than their own `allow` list), and it is a
    /// safety rule -- offering a one-keystroke way to remove one would be
    /// the wrong shape for a rule whose entire job is to be hard to evade,
    /// even by the operator's own accidental keypress.
    pub fn revoke_permission_pattern(
        &self,
        env: &std::collections::HashMap<String, String>,
        rule: &conway_core::permission_pattern::PatternRule,
        origin: &conway_core::permission_pattern::PatternOrigin,
    ) -> RevokeOutcome {
        if !self.rt.permission_broker().revoke_pattern(rule, origin) {
            return RevokeOutcome::NotFound;
        }

        let path = match origin {
            conway_core::permission_pattern::PatternOrigin::Interactive => {
                return RevokeOutcome::RevokedNoFile;
            }
            conway_core::permission_pattern::PatternOrigin::File(path) => path,
        };

        match Self::rewrite_permission_file_removing(path, rule) {
            Err(e) => RevokeOutcome::RevokedButPersistFailed {
                error: e.to_string(),
            },
            Ok(()) => {
                let global_path = crate::config::discovery::xdg_config_path(env).and_then(
                    |settings| settings.parent().map(|dir| dir.join("permissions.json")),
                );
                let is_global = global_path.as_deref() == Some(path.as_path());
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

    /// Removes `rule`'s wire form from `path`'s `allow` list, tmp-then-
    /// rename -- see [`Self::revoke_permission_pattern`]'s own doc for the
    /// full reasoning (why a parse failure is a hard error here, unlike the
    /// append path; why no chmod hardening).
    fn rewrite_permission_file_removing(
        path: &std::path::Path,
        rule: &conway_core::permission_pattern::PatternRule,
    ) -> std::io::Result<()> {
        let contents = std::fs::read_to_string(path)?;
        let mut file: conway_core::permission_pattern::PermissionFile =
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
            // Nothing to remove -- the goal state already holds. No write,
            // so there is nothing that could de-trust the file either.
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

    /// Loads permissions files project-first then global
    /// (`crate::config::discovery::permission_file_paths`) and installs
    /// their rules -- **the real production startup seam**, board item
    /// 01KYT8SGX32CP56PRJNG72V2W5. `conway-cli`'s TUI (`tui::app::App::new`)
    /// calls this directly; so does
    /// `crates/conway/tests/permission_trust_seam.rs`, which is how that
    /// item's acceptance test drives the actual code path rather than a
    /// hand-written fixture.
    ///
    /// The asymmetry (see `conway_core::permission_pattern`'s module doc
    /// and `crate::config::trust`'s own doc for the full reasoning):
    /// - `deny` rules install from EVERY file, unconditionally.
    /// - `allow` rules from the GLOBAL file install unconditionally too --
    ///   trusted by authorship, the operator's own file.
    /// - `allow` rules from a PROJECT file install ONLY when
    ///   `crate::config::trust::TrustStore::is_trusted` confirms an
    ///   explicit, recorded trust decision matching the file's CURRENT
    ///   bytes; otherwise they are skipped and a human-readable notice is
    ///   returned (never an error -- see this codebase's existing
    ///   "every permissions-file failure is silent and narrowing" posture).
    pub fn load_permission_files(
        &self,
        cwd: &std::path::Path,
        env: &std::collections::HashMap<String, String>,
        granting_agent: conway_core::ids::AgentId,
    ) -> PermissionLoadReport {
        let paths = crate::config::discovery::permission_file_paths(cwd, env);
        let global_path = crate::config::discovery::xdg_config_path(env)
            .and_then(|settings| settings.parent().map(|dir| dir.join("permissions.json")));
        let trust_store = crate::config::trust::TrustStore::load(env);
        let mut notices = Vec::new();
        let mut registration_errors = Vec::new();

        for path in &paths {
            let Ok(contents) = std::fs::read_to_string(path) else {
                continue;
            };

            // Deny applies unconditionally, from every scope, regardless
            // of trust -- D4 §3. F12: this now also covers structured
            // `then: deny` rules from the `rules` array (`parse_deny_rules`
            // returns the union of flat `deny` and structured `then: deny`).
            for rule in conway_core::permission_pattern::parse_deny_rules(&contents) {
                if let Some(err) = self.validate_rule_registration(&rule) {
                    registration_errors.push(err);
                    continue;
                }
                self.install_deny_rule(rule, path.clone());
            }

            // F12: prompt rules apply unconditionally too (narrowing, D4 §3
            // extended to `prompt` -- extension-architecture.md §5.5 stage
            // 1). The flat form has no prompt syntax, so these come
            // entirely from the structured `rules` array.
            for rule in conway_core::permission_pattern::parse_prompt_rules(&contents) {
                if let Some(err) = self.validate_rule_registration(&rule) {
                    registration_errors.push(err);
                    continue;
                }
                self.install_prompt_rule(rule, path.clone());
            }

            let is_global = global_path.as_deref() == Some(path.as_path());
            let trusted = is_global || trust_store.is_trusted(path, &contents);
            let allow_rules = conway_core::permission_pattern::parse_rules(&contents);
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
                if let Some(err) = self.validate_rule_registration(&rule) {
                    registration_errors.push(err);
                    continue;
                }
                self.install_allow_rule(
                    rule,
                    conway_core::agent::PermissionScope::Session,
                    granting_agent,
                    path.clone(),
                );
            }
        }

        PermissionLoadReport {
            paths,
            notices,
            registration_errors,
        }
    }

    /// Records an explicit trust decision for `path`'s CURRENT bytes on
    /// disk (`crate::config::trust::TrustStore::trust`) and immediately
    /// installs its `allow` rules for this running session -- so trusting
    /// takes effect now, not only on the next restart. Returns the number
    /// of allow rules installed.
    ///
    /// This is the ONLY path that writes a trust record, and it is only
    /// ever invoked by an explicit operator action (the TUI's `/trust
    /// permissions`) -- never automatically, never as a side effect of
    /// [`Self::load_permission_files`] (board item
    /// 01KYT8SGX32CP56PRJNG72V2W5, D4 §5/§9: no startup prompt, no silent
    /// self-trust).
    pub fn trust_permission_file(
        &self,
        env: &std::collections::HashMap<String, String>,
        path: &std::path::Path,
        granting_agent: conway_core::ids::AgentId,
    ) -> std::io::Result<usize> {
        crate::config::trust::TrustStore::trust(env, path)?;
        let contents = std::fs::read_to_string(path)?;
        let rules = conway_core::permission_pattern::parse_rules(&contents);
        let mut count = 0;
        for rule in rules {
            if self.validate_rule_registration(&rule).is_some() {
                // A registration error means the rule was never going to
                // match; do not count it as installed. `trust_permission_file`
                // is operator-triggered (`/trust permissions`), so the
                // operator will have already seen the registration errors from
                // the prior `load_permission_files` -- re-trusting does not
                // silently swallow them, it just does not re-report them here.
                continue;
            }
            self.install_allow_rule(
                rule,
                conway_core::agent::PermissionScope::Session,
                granting_agent,
                path.to_path_buf(),
            );
            count += 1;
        }
        Ok(count)
    }

    pub fn config(&self) -> &ConwayConfig {
        &self.config
    }

    /// The local model-metadata map (`[models.metadata_path]`), loaded ONCE
    /// by `ConwayBuilder::build` and kept here so every consumer reads the
    /// SAME parse of the SAME file instead of each re-reading it from disk
    /// on its own (T3 follow-up: `conway-cli`'s `App::new` previously
    /// re-read `[models.metadata_path]` itself, a second code path that
    /// happened to agree with the builder's only because both implement the
    /// identical "missing file -> empty map" fallback -- a duplication that
    /// could silently drift if either side's load logic ever changed alone).
    /// Empty (never an error) when the builder found no metadata file, or
    /// found one that named no models -- mirrors
    /// `config::model_metadata::load`'s own "missing is expected" contract.
    pub fn model_metadata(&self) -> &ModelMetadata {
        &self.model_metadata
    }

    /// Creates a new session and starts its root agent.
    ///
    /// `spec`'s `None`/empty fields are resolved from `self.config` here, at
    /// call time rather than at builder time, so one `Conway` can serve
    /// differently-configured sessions (`SessionSpec::default()` itself is
    /// config-agnostic; see that type's own doc).
    ///
    /// **Reconciliation (disclosed):** the binding implementation notes
    /// describe this method's sequence as "build `SessionMeta` ->
    /// `store.create` -> `Runtime::start_root`", but the already-committed
    /// `Runtime::start_root` (WI-082) already builds the `SessionMeta` and
    /// calls `SessionStore::create` internally. Calling `store.create` again
    /// here with the same id would double-create the session (an error
    /// against both `FakeStore` and `JsonlSessionStore`, which reject a
    /// duplicate id). Instead, this method generates the `SessionId` itself
    /// and passes it through `RootSpec::session`, so `start_root`'s own
    /// internal `store.create` call is the single, authoritative creation --
    /// this still satisfies "creates a session via `SessionStore::create`",
    /// it just avoids invoking that call a second time.
    ///
    /// **Caller-chosen id (WI-119):** `spec.id` -- when `Some` -- is passed
    /// through unchanged instead of the freshly minted `SessionId` below.
    /// `RootSpec::session` (WI-082) already supports this at the runtime
    /// layer; this is the facade-side wiring to reach it. An id already
    /// present in the store surfaces as `start_root`'s own
    /// `SessionStore::create` failure --
    /// `Err(ConwayError::Runtime(RuntimeError::Store(StoreError::AlreadyExists
    /// { .. })))`, propagated unchanged through the `?` below -- typed and
    /// distinct from every other failure this method can produce, not a
    /// generic error.
    ///
    /// **Disclosed gap:** `RootSpec` (`conway-runtime`, WI-082) has no field
    /// for `SessionSpec::labels` or for `config.limits.max_parallel_tools`,
    /// so neither reaches the created session/agent through this method --
    /// out of this item's file scope to add.
    pub async fn new_session(&self, spec: SessionSpec) -> Result<SessionHandle> {
        let role = spec
            .role
            .unwrap_or_else(|| self.config.default_role.clone());
        let cwd = spec.cwd.unwrap_or_else(|| self.config.cwd.clone());
        let budget = spec.budget.unwrap_or_else(|| self.default_budget());
        let agent_def = spec.agent_def.map(AgentDefRef);

        let session = spec.id.unwrap_or_default();
        let root_spec = RootSpec {
            session: Some(session),
            agent_def,
            role: Some(role),
            tools: spec.tools,
            budget,
            cwd,
            // Board item 01KYTMH9JX21CGSE2Y6E2KP8SJ: this `Conway`'s own
            // confinement root (`ConwayBuilder::with_root`), if the operator
            // set one -- `None` (unconfined) otherwise. `SessionSpec` has no
            // per-session override for this: unlike `cwd`/`role`/`model`,
            // root confinement is a whole-invocation setting an operator
            // opts into once, not something a caller varies per session.
            root: self.root.clone(),
            prompt: None,
            keep_alive: spec.keep_alive,
            model: spec.model,
        };
        let root = self.rt.start_root(root_spec).await?;

        Ok(SessionHandle::new(
            self.rt.clone(),
            session,
            root,
            self.store.clone(),
        ))
    }

    /// `config.limits` resolved into a `Budget`. `max_tool_calls` has no
    /// facade config counterpart and is always `None`.
    fn default_budget(&self) -> Budget {
        let limits = &self.config.limits;
        Budget {
            max_steps: limits.max_steps,
            deadline: if limits.deadline_secs == 0 {
                None
            } else {
                Some(Utc::now() + ChronoDuration::seconds(limits.deadline_secs as i64))
            },
            max_tokens: if limits.max_tokens == 0 {
                None
            } else {
                Some(limits.max_tokens)
            },
            max_tool_calls: None,
        }
    }

    /// The "why did this model run, and why not the others" report for
    /// `role`, projected through the concrete `DeclarativeRouter` this
    /// `Conway` compiled itself.
    ///
    /// When the builder instead received an injected `Router`
    /// (`ConwayBuilder::with_router`), there is no concrete
    /// `DeclarativeRouter` to project through -- `conway_routing::RoutingExplain`
    /// is defined over that concrete type, not the `Router` trait object --
    /// so this returns a degraded, empty report (`entries: vec![]`,
    /// `headroom_tokens: 0`), mirroring `RoutingExplain::explain`'s own
    /// fallback for an unrecognized role.
    pub fn explain_routing(&self, role: &RoleAlias) -> ExplainReport {
        let req = RouteRequest {
            role: role.clone(),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 0,
            agent_id: AgentId::new(),
        };
        match &self.router_explain {
            Some(router) => RoutingExplain::new(router).explain(&req),
            None => ExplainReport {
                role: role.clone(),
                pin: None,
                est_tokens: 0,
                required: RequiredCaps::default(),
                headroom_tokens: 0,
                entries: Vec::new(),
                generated_at: Utc::now(),
            },
        }
    }

    /// Reattaches to a persisted session (WI-103), now as a DRIVABLE handle
    /// (WI-119).
    ///
    /// **Resolved (WI-119):** this method's previous doc disclosed a real
    /// gap -- `conway-runtime` exposed only `start_root`, which cannot be
    /// repurposed for resume (it unconditionally `store.create`s, which
    /// every committed `SessionStore` rejects for an id that already has a
    /// persisted session). WI-118 closed that gap by adding
    /// `Runtime::resume_root(ResumeSpec)`: it reads the existing
    /// `SessionMeta` via `store.meta` (no `store.create`), re-registers
    /// `meta.agent_id` into `Runtime`'s `agents` map and `AgentTree` through
    /// the same `launch_agent` path `start_root` uses, and gates the
    /// resumed `AgentLoop`'s first iteration behind a `ResumeGate` so it
    /// idles until this handle's own first `SessionHandle::prompt` call --
    /// never racing the (already-completed) persisted transcript. This
    /// method now calls it directly, which resolves both criteria the
    /// pre-WI-118 doc could not satisfy:
    /// - `prompt()` after resume: `Runtime::prompt` now finds `agent` in
    ///   `Runtime.agents` (registered by `resume_root` below), so it appends
    ///   and wakes the gated loop instead of returning `AgentNotFound`.
    /// - `tree()`: `resume_root` attaches the resumed root to `AgentTree`,
    ///   so `SessionHandle::tree()` (`self.rt.tree()`, WI-101, unchanged)
    ///   now reflects it. **Still disclosed, not silently dropped:**
    ///   `resume_root`'s own doc is explicit that it re-attaches only the
    ///   resumed *root* -- past fork/spawn children are not re-attached as
    ///   live `AgentTree` nodes (their tasks are gone; a live-looking node
    ///   with nothing to ever finish it would misrepresent their status
    ///   worse than omitting them). Their history remains fully readable via
    ///   `transcript`/`context_report_at`, just not via `tree()`.
    ///
    /// Every property the old, store-only implementation already delivered
    /// is preserved: `id()`/`root()` still read from the persisted
    /// `SessionMeta` (`resume_root` returns exactly `meta.agent_id`, never a
    /// freshly minted id); `transcript(root)` still reads purely through
    /// `SessionStore`, unaffected by live registration; a truncated trailing
    /// line is still repaired transparently by `JsonlSessionStore` on first
    /// file access (the same `store.meta` call `resume_root` makes
    /// internally), so `resume` still succeeds on such a session without any
    /// special-casing here. The warning-forwarding gap this method's
    /// previous doc disclosed (`SessionStore`'s ports carry no "a repair
    /// just happened" signal to surface as `Event::Error{fatal: false}`)
    /// still stands, for the same reason -- `conway-session`'s repair path
    /// only `tracing::warn!`s, with nothing threaded back through any
    /// `Result`.
    ///
    /// `agent_def`/`role`/`cwd` are all left `None` in the `ResumeSpec`
    /// below, so `resume_root` falls back to the persisted `SessionMeta`'s
    /// own values -- this method has no override surface for them (matching
    /// `resume`'s existing binding signature, which takes only `sid`); a
    /// caller that needs an override can add one to `ResumeSpec` through a
    /// future item without breaking this one's contract.
    ///
    /// **Error-shape preservation (disclosed):** `resume_root`'s own error
    /// for an unknown/missing session is `RuntimeError::Store` (its internal
    /// `store.meta` lookup, converted via that type's own `#[from]
    /// StoreError`) -- a plain `?` here would surface it as
    /// `ConwayError::Runtime(RuntimeError::Store(_))`, one layer deeper than
    /// this method returned pre-WI-119 (`ConwayError::Store(_)` directly, from
    /// this method's own former `store.meta` call). `resume`'s existing test
    /// suite asserts the flat shape, and nothing about resuming a session
    /// makes "the store doesn't have it" a *runtime* concern rather than a
    /// *store* one -- so this unwraps `RuntimeError::Store` back to
    /// `ConwayError::Store` explicitly, keeping every other `RuntimeError`
    /// variant (e.g. a future `resume_root` failure mode) under
    /// `ConwayError::Runtime` unchanged.
    pub async fn resume(&self, sid: SessionId) -> Result<SessionHandle> {
        let agent = self
            .rt
            .resume_root(ResumeSpec {
                session: sid,
                agent_def: None,
                role: None,
                tools: None,
                budget: self.default_budget(),
                cwd: None,
            })
            .await
            .map_err(|err| match err {
                RuntimeError::Store(inner) => ConwayError::Store(inner),
                other => ConwayError::Runtime(other),
            })?;
        Ok(SessionHandle::new(
            self.rt.clone(),
            sid,
            agent,
            self.store.clone(),
        ))
    }

    /// Enumerates persisted sessions via `SessionStore::list`, returned
    /// unmodified -- no facade-side re-filtering, re-ordering, or paging
    /// beyond what `filter` itself already expresses.
    pub async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionMeta>> {
        Ok(self.store.list(filter).await?)
    }

    /// A session's own local record count -- `SessionStore::head`, the same
    /// value [`Conway::fork_from`]'s own bounds check compares `at` against.
    ///
    /// Distinct from [`SessionHandle::transcript`](crate::SessionHandle::transcript)'s
    /// length: `transcript` returns the *effective, ancestry-resolved* view
    /// (inherited prefix + this session's own records), which overcounts the
    /// local head for any session that is itself a fork child. Callers that
    /// need "this session's current head, as `fork_from` itself sees it" --
    /// e.g. `conway-cli`'s `--fork-from <ref>` with no `@seq`, which must
    /// compute "fork this branch at its current head" -- need this method,
    /// not `transcript().len()`.
    pub async fn session_head(&self, sid: SessionId) -> Result<LogSeq> {
        Ok(self.store.head(&sid).await?)
    }

    /// Forks a *stored* session at an arbitrary point, offline -- no live
    /// parent agent is involved, and `SessionStore::fork`'s O(1)-by-
    /// reference contract (architecture §5.1/§8, D-11's local-unit `at_seq`)
    /// means zero parent records are copied.
    ///
    /// Distinct from [`SessionHandle::fork`](crate::SessionHandle::fork),
    /// which forks a *live* agent at its current head through
    /// `SubagentHost` -- this item's binding notes name that contrast as
    /// "the most likely point of confusion in the public API": both
    /// ultimately call `SessionStore::fork`, but only `SessionHandle::fork`
    /// goes through the runtime's subagent machinery (and so also spawns a
    /// live agent task); this method only creates the child's session file.
    ///
    /// Reuses [`ForkSpec`] (WI-102) rather than a parallel type, per the
    /// binding notes. `directive`/`cache_hint`/`result_contract` still have
    /// no session-level counterpart -- `conway_core::log::SessionMeta`
    /// carries none of them, and there is no live child turn here to attach
    /// a `LogRecord::ForkDirective` to (the child session is *created* with
    /// zero records, store-side, exactly as before) -- so only `agent_def`
    /// and `role` are consulted for the persisted `SessionMeta`, as
    /// overrides onto the parent's own values.
    ///
    /// **Live registration (WI-119):** after the store-side fork below, this
    /// method now also calls `Runtime::resume_root` over the freshly created
    /// child session -- the same mechanism [`Conway::resume`] uses -- so the
    /// returned handle is DRIVABLE: `prompt` on it succeeds (verified by
    /// `fork_from_returns_a_drivable_child_whose_prompt_succeeds`).
    /// `resume_root`'s `ResumeGate` (WI-118) means the child idles until
    /// *this* handle's own first `prompt` call, exactly like a resumed root
    /// -- it does not run a turn as a side effect of `fork_from` itself.
    /// `spec.tools`/`spec.budget` -- otherwise unused by the store-side fork
    /// -- ARE consulted here: they configure the live agent's `AgentSpec`
    /// (`ResumeSpec.tools`/`.budget`), the same role `ForkSpec` plays for
    /// [`SessionHandle::fork`](crate::SessionHandle::fork)'s live path.
    /// `agent_def`/`role`/`cwd` are left `None` in the `ResumeSpec` --
    /// `resume_root` falls back to `child_meta`'s own already-resolved
    /// values (set from `spec`/`parent_meta` just above), so there is no
    /// need to re-derive them a second time.
    ///
    /// **Inherited prefix, resolved (WI-119 gap closed):** this criterion
    /// also asks for "the child's context contains the inherited prefix" --
    /// previously disclosed here as NOT satisfied, since `Runtime::
    /// resume_root` (WI-118) always constructed its `AgentLoop` with
    /// `inherited: None`, correct only for a genuine root (whose own session
    /// records ARE its complete history), not for a fork child (whose own
    /// records are, by the zero-copy contract this method preserves, empty
    /// or a small tail). `resume_root` (`conway-runtime`) now detects a
    /// fork-child session via its persisted `SessionMeta::origin` and
    /// resolves the parent's prefix at `origin.at_seq` through
    /// `conway_session::TranscriptResolver::resolve_prefix` (made `pub` for
    /// this) -- the exact primitive `subagent.rs`'s live-fork path already
    /// bottoms out on, so there is one shared implementation of the D-11
    /// ancestry walk, not two. This works for `fork_from`'s arbitrary,
    /// possibly-earlier `at` (unlike substituting `subagent.rs`'s own
    /// current-head-only fork path, which was ruled out for exactly that
    /// reason) because it resolves directly against `(parent, at_seq)`
    /// rather than reusing `subagent.rs`'s "resolve the freshly-forked
    /// child" shortcut. No change was needed in this method itself: it
    /// already called `resume_root`, which now does the right thing.
    ///
    /// **Sibling-tool note (disclosed):** `resume_root`'s own doc covers the
    /// case a resumed fork child has since accumulated its own turns (the
    /// resolved prefix excludes them, so `AgentLoop`'s separate own-records
    /// read is never double-counted) -- see that method's doc for the full
    /// mechanism.
    ///
    /// **Defense-in-depth bounds check (disclosed):** `SessionStore::fork`'s
    /// own committed implementation (`conway-session`'s `fork_impl`) already
    /// rejects `at > head` with `StoreError::SeqOutOfRange{ requested, head
    /// }` -- but `conway_core::fakes::FakeStore` (a `SessionStore` impl this
    /// crate depends on but does not own; out of this item's file scope to
    /// change) does not enforce that bound. Rather than let this method's
    /// behavior depend on which `SessionStore` backs a given `Conway`, the
    /// bound is checked here too, against the same error shape, so the
    /// criterion holds under every `SessionStore` implementation.
    ///
    /// **Shared helper (disclosed refactor):** the `store.fork` ->
    /// `rt.resume_root` sequence below used to live inline here. It now
    /// delegates to `crate::fork_child::fork_child` -- which only this
    /// method calls, since board item B2 moved the `/ask` fork-ask flow
    /// (`SessionHandle::ask`) onto the runtime's own attach path
    /// (`SubagentHost::start`) so ephemeral `/ask` children attach as proper
    /// fork children of the asker. A session created through this method is
    /// never catalog-hidden: `fork_child` fixes `SessionMeta::ephemeral` to
    /// `false`. See that module's doc for the full history.
    pub async fn fork_from(
        &self,
        sid: SessionId,
        at: LogSeq,
        spec: ForkSpec,
    ) -> Result<SessionHandle> {
        let parent_meta = self.store.meta(&sid).await?;
        let head = self.store.head(&sid).await?;
        if at.0 > head.0 {
            return Err(ConwayError::Store(StoreError::SeqOutOfRange {
                requested: at,
                head,
            }));
        }

        crate::fork_child::fork_child(
            &self.rt,
            &self.store,
            sid,
            parent_meta,
            at,
            crate::fork_child::ForkChildRequest {
                agent_def: spec.agent_def,
                role: spec.role,
                tools: spec.tools,
                budget: spec.budget,
            },
        )
        .await
    }

    /// Promotes an ephemeral `/ask`-style agent to persistent (B3 — the
    /// `/ask` modal's `[f]` "keep" fate), atomically performing ALL THREE
    /// of: the durable session-header rewrite, the live-tree flag flip, and
    /// the `Event::AgentPromoted` emission that tells UIs to update. After
    /// B2, promotion is a flag flip ONLY — no re-parenting, no record
    /// rewriting beyond the header's `ephemeral` bit (P-2: the child's
    /// entire transcript, origin, and provenance are preserved verbatim).
    ///
    /// **Failure ordering (binding): header first, then tree, then
    /// event.** The store is the source of truth, so the durable flip
    /// (`SessionStore::set_ephemeral`) runs first; if it fails — unknown
    /// session, non-ephemeral session, I/O error — this method returns
    /// `Err` having touched NOTHING else: no tree flip, no event, so the
    /// three views can never split-brain. The tree flip and the event are
    /// then performed together by `Runtime::promote_agent` (flip strictly
    /// before emission), and both are infallible once the guard below has
    /// passed: `AgentTree` never detaches nodes, so an agent present in
    /// the snapshot stays flippable for this runtime's lifetime. The
    /// reverse ordering (tree/event first) was rejected precisely because
    /// a subsequent durable failure would then leave the live views
    /// claiming "persistent" while the header still says ephemeral —
    /// including making the session wrongly purge-eligible via
    /// `SessionStore::remove`.
    ///
    /// **Facade-layer live check (guard-matrix boundary):** `agent` must
    /// be present in `Runtime::tree()` — promotion is a LIVE operation
    /// (the modal acts on a child it is looking at), and this is also what
    /// resolves `agent` to its owning session without a store scan. The
    /// check lives here, not in the store, for the same reason B1's
    /// `remove` guard matrix documents its own (inverse) live check at the
    /// facade layer: the store has no view of the runtime's tree. The
    /// store-level `set_ephemeral` primitive itself imposes no liveness
    /// requirement and would also work on a cold session; this facade
    /// method deliberately does not expose that. The snapshot read and the
    /// later flip cannot race stale: nodes are never detached from
    /// `AgentTree`, so presence cannot go stale between the two.
    ///
    /// P-1: promote is a lifecycle operation on an existing agent, NOT a
    /// new subagent primitive — no fork, no spawn, no new session.
    ///
    /// Errors: `ConwayError::Runtime(RuntimeError::AgentNotFound)` when
    /// `agent` is not in the live tree; `ConwayError::Store(
    /// StoreError::NotPromotable)` when the agent's session is not
    /// ephemeral (a double promote, or a non-`/ask` session); other
    /// `StoreError`s propagated unchanged.
    ///
    /// Returns the promoted agent's `SessionId` (unchanged by the promote
    /// — the flip touches no ids), so the caller can immediately e.g.
    /// focus or resume the now-persistent session.
    pub async fn promote(&self, agent: AgentId) -> Result<SessionId> {
        // Facade-layer live check + agent -> session resolution in one
        // read (see the doc above for why the check lives at this layer).
        let snapshot = self.rt.tree();
        let node = snapshot
            .nodes
            .iter()
            .find(|n| n.agent_id == agent)
            .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound { agent }))?;
        let session = node.session;

        // Step 1 (durable, source of truth): the guarded header rewrite.
        // On ANY failure here nothing else has happened — see the
        // failure-ordering paragraph in this method's doc.
        self.store.set_ephemeral(&session, false).await?;

        // Steps 2+3 (live): tree flip, then the event — strictly in that
        // order inside `Runtime::promote_agent` (see its doc).
        self.rt.promote_agent(agent)?;
        Ok(session)
    }

    /// Merges an ephemeral `/ask` child's turns into its parent's log,
    /// verbatim, then purges the child (B4 — the `/ask` modal's "pull in"
    /// fate, the semantic opposite of [`Conway::promote`]'s "keep": instead
    /// of the child becoming a session in its own right, its question and
    /// answer become part of the parent's own history and the child ceases
    /// to exist).
    ///
    /// **The merge set (binding, from the B2 review):** post-B2, BOTH
    /// facade `/ask` children (`SessionHandle::ask`) and `conway_ask` tool
    /// children carry their question as a `LogRecord::ForkDirective { by:
    /// parent }` head record, NOT a `UserTurn`. The merge set is exactly:
    /// - the child's `ForkDirective` head record, materialized as a
    ///   `UserTurn` (text = the directive's text, `ts` preserved) re-stamped
    ///   `Provenance::MergedAsk { from: child_session }` — so the merge
    ///   origin stays explicit and inspectable (P-2/GP-10) even after the
    ///   child's own session file is purged;
    /// - the child's `Assistant` records, copied VERBATIM — real `model`,
    ///   `route_reason`, `usage`, `stop`, `content`, `ts` all pass through
    ///   untouched (P-10: these are untrusted model-produced fields; this
    ///   method never fabricates synthetic values for them);
    /// - any genuine `UserTurn` records in the child (defensive: older or
    ///   other ask shapes), copied verbatim except `prov` re-stamped to
    ///   `MergedAsk`.
    ///
    /// `ContextReportRecord`/`AgentResultRecord`/tool records are NOT
    /// merged as top-level records — tool calls persist inside the
    /// `Assistant` records' content blocks (the `conway_ask` item-f
    /// precedent), and the child's context reports/results describe the
    /// child's own (now-purged) session, not the parent's.
    ///
    /// **Sequencing:** merged records are appended to the parent's log via
    /// `SessionStore::append`, which re-sequences them to the parent's head
    /// (`JsonlSessionStore`'s `assign_seq`; `FakeStore` has parity) — this
    /// method deliberately does NOT hand-assign seqs. The placeholder seq on
    /// the materialized `UserTurn` never reaches disk.
    ///
    /// **Guard matrix and failure ordering (binding):** every refusal runs
    /// BEFORE the parent's log is mutated, so a refused pull leaves no
    /// partial merge behind:
    /// 1. `child` must be present in `Runtime::tree()` (else
    ///    `RuntimeError::AgentNotFound`) — the same facade-layer live check
    ///    [`Conway::promote`] documents, and also how `child` resolves to
    ///    its owning session and parent without a store scan. Tree nodes
    ///    are never detached, so the snapshot taken here cannot go stale
    ///    before the store calls below.
    /// 2. The child's parent (from the tree) must be LIVE — present in the
    ///    tree with a non-terminal `AgentStatus` — else
    ///    `RuntimeError::AgentNotLive`. A finished parent will never run
    ///    another turn, so merging into its log would write records nothing
    ///    ever reads. No wake is needed for a live parent: `agent_loop`
    ///    re-reads its full context from the store every turn
    ///    (agent_loop.rs), so the merged records are simply part of the
    ///    parent's next turn.
    /// 3. B1's `remove` guards, mirrored as pre-checks so they fail before
    ///    any parent mutation: the child's session must be ephemeral (else
    ///    `StoreError::NotRemovable`) and must have NO children of its own,
    ///    ephemeral ones included (else `NotRemovable`).
    ///
    /// Only then are the merged records appended and `SessionStore::remove`
    /// called for the child — whose own guard matrix re-runs authoritatively
    /// under the store's lifecycle lock, so a concurrent fork of the child
    /// between the pre-check and the purge is still refused (that race
    /// leaves the appended records in the parent AND the child unpurged —
    /// disclosed as the one non-atomic seam in this operation; a crash in
    /// the same window has the same shape. Recovery is a store-level
    /// `SessionStore::remove` of the child, NOT a `pull_in` retry: the
    /// child is still ephemeral and childless, so `remove`'s guards pass —
    /// whereas re-calling `pull_in` would append the whole merge set a
    /// SECOND time before purging, duplicating the question and answer in
    /// the parent's log).
    ///
    /// The child's tree node is NOT detached (`AgentTree` never detaches —
    /// the same invariant [`Conway::promote`] relies on), so the tree keeps
    /// a provenance record that the ask happened (P-2) even though the
    /// session behind it is gone.
    ///
    /// P-1: pull-in is a lifecycle operation on two existing agents' logs,
    /// NOT a new subagent primitive — no fork, no spawn, no new session is
    /// created here.
    pub async fn pull_in(&self, child: AgentId) -> Result<()> {
        // 1+2. Live-tree resolution and the parent liveness guard, from one
        // snapshot (nodes never detach, so this cannot race stale).
        let snapshot = self.rt.tree();
        let child_node = snapshot
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound { agent: child }))?;
        let child_session = child_node.session;
        let parent_agent = child_node.parent.ok_or(ConwayError::Store(
            StoreError::NotRemovable {
                session: child_session,
                reason: "pull_in: the child has no parent in the live tree to merge into".into(),
            },
        ))?;
        let parent_node = snapshot
            .nodes
            .iter()
            .find(|n| n.agent_id == parent_agent)
            .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound {
                agent: parent_agent,
            }))?;
        if matches!(
            parent_node.status,
            AgentStatus::Finished | AgentStatus::Failed | AgentStatus::Cancelled
        ) {
            return Err(ConwayError::Runtime(RuntimeError::AgentNotLive {
                agent: parent_agent,
            }));
        }
        let parent_session = parent_node.session;

        // The child must be terminal: merging a still-running child would
        // miss its trailing records and then purge the session under a
        // live agent loop (whose next append would fail `NotFound`).
        // Terminal status is absorbing — a Finished/Failed/Cancelled child
        // can never append again — so this snapshot check cannot go stale
        // (cycle-5 B4 review, significant finding 1).
        if !matches!(
            child_node.status,
            AgentStatus::Finished | AgentStatus::Failed | AgentStatus::Cancelled
        ) {
            return Err(ConwayError::Store(StoreError::NotRemovable {
                session: child_session,
                reason: "child is still running (pull_in merges only completed asks)".into(),
            }));
        }

        // 3. B1's remove guards, mirrored as pre-checks so a refused pull
        // never leaves a partial merge in the parent's log (see the doc
        // above). `remove` re-checks both authoritatively under its
        // lifecycle lock before anything is deleted.
        let child_meta = self.store.meta(&child_session).await?;
        if !child_meta.ephemeral {
            return Err(ConwayError::Store(StoreError::NotRemovable {
                session: child_session,
                reason: "session is not ephemeral (pull_in merges only ephemeral /ask children)"
                    .into(),
            }));
        }
        let grandchildren = self
            .store
            .list(SessionFilter {
                parent: Some(child_session),
                include_ephemeral: true,
                ..SessionFilter::default()
            })
            .await?;
        if !grandchildren.is_empty() {
            return Err(ConwayError::Store(StoreError::NotRemovable {
                session: child_session,
                reason: "session has children (pull_in would orphan their provenance)".into(),
            }));
        }

        // The merge set, per the AMENDMENT in this method's doc.
        let head = self.store.head(&child_session).await?;
        let records = self
            .store
            .read(&child_session, SeqRange::new(LogSeq::ZERO, Some(head)))
            .await?;
        let mut merged = Vec::new();
        for record in records {
            match record {
                LogRecord::ForkDirective { ts, text, .. } => {
                    merged.push(LogRecord::UserTurn {
                        // Placeholder only — the store re-sequences on
                        // append; this value never reaches disk.
                        seq: LogSeq::ZERO,
                        ts,
                        text,
                        prov: Provenance::MergedAsk {
                            from: child_session,
                        },
                    });
                }
                // VERBATIM (P-10): real model/route_reason/usage/stop/ts —
                // passed through, never fabricated.
                assistant @ LogRecord::Assistant { .. } => merged.push(assistant),
                LogRecord::UserTurn { seq, ts, text, .. } => merged.push(LogRecord::UserTurn {
                    seq,
                    ts,
                    text,
                    prov: Provenance::MergedAsk {
                        from: child_session,
                    },
                }),
                // Everything else (tool records, context reports, agent
                // results, system notes, the header) is not part of the
                // merge set — see the doc above.
                _ => {}
            }
        }

        // Append to the parent's log; the store re-sequences each record to
        // the parent's head (see the doc above).
        for record in merged {
            self.store.append(&parent_session, record).await?;
        }

        // Purge the child. B1's guards re-run authoritatively here; the
        // pre-checks above only exist for failure ordering.
        self.store.remove(&child_session).await?;
        Ok(())
    }

    /// Purges an ephemeral `/ask` child outright, WITHOUT merging its turns
    /// anywhere (B5 — the `/ask` modal's `[esc]` "discard" fate, and the
    /// forced fate when the TUI quits with the modal open). The semantic
    /// opposite of [`Conway::pull_in`]: the user has explicitly chosen to
    /// throw the answer away, which is the single sanctioned exception to
    /// mandatory provenance retention (P-2/GP-10 — discard only ever happens
    /// via this explicit choice, never silently).
    ///
    /// **Guard matrix (mirrors the facade-layer checks [`Conway::promote`]
    /// and [`Conway::pull_in`] document, with the store's B1 guards
    /// authoritative at the end):**
    /// 1. `agent` must be present in `Runtime::tree()` (else
    ///    `RuntimeError::AgentNotFound`) — purge is a LIVE operation here
    ///    (the modal discards a child it is looking at), the same
    ///    facade-layer live check the store's own `remove` doc assigns to
    ///    this layer, and also how `agent` resolves to its owning session
    ///    without a store scan. Tree nodes are never detached, so the
    ///    snapshot cannot go stale before the store call below.
    /// 2. The child must be TERMINAL (`Finished`/`Failed`/`Cancelled`),
    ///    else `StoreError::NotRemovable` — purging under a still-running
    ///    agent loop would orphan its next append (the same still-running
    ///    guard `pull_in` carries; terminal status is absorbing, so this
    ///    snapshot check cannot race stale either). The modal only offers
    ///    the discard fate after the child's turn has ended, but this is a
    ///    public facade op and guards itself.
    /// 3. B1's `SessionStore::remove` guards then run authoritatively under
    ///    the store's lifecycle lock: ephemeral-only (`NotRemovable`
    ///    otherwise — a promoted child can no longer be discarded; the two
    ///    fates are exclusive) and no children of its own.
    ///
    /// The child's tree node is NOT detached (`AgentTree` never detaches),
    /// so the tree keeps a provenance record that the ask happened even
    /// though the session behind it is gone — same invariant
    /// [`Conway::pull_in`] documents.
    ///
    /// P-1: purge is a lifecycle operation on an existing agent's session,
    /// NOT a new subagent primitive.
    pub async fn purge(&self, agent: AgentId) -> Result<()> {
        // 1. Live-tree resolution (see the doc above).
        let snapshot = self.rt.tree();
        let node = snapshot
            .nodes
            .iter()
            .find(|n| n.agent_id == agent)
            .ok_or(ConwayError::Runtime(RuntimeError::AgentNotFound { agent }))?;
        let session = node.session;

        // 2. Terminal-only (see the doc above; same reasoning as
        // `pull_in`'s still-running guard).
        if !matches!(
            node.status,
            AgentStatus::Finished | AgentStatus::Failed | AgentStatus::Cancelled
        ) {
            return Err(ConwayError::Store(StoreError::NotRemovable {
                session,
                reason: "agent is still running (purge discards only completed asks)".into(),
            }));
        }

        // 3. B1's guards (ephemeral-only, no children) run authoritatively
        // under the store's lifecycle lock.
        self.store.remove(&session).await?;
        Ok(())
    }

    /// Crash-residue sweep (B5): purges leftover ephemeral sessions created
    /// by the MODAL `/ask` path (`SessionHandle::ask`,
    /// `AskOrigin::ModalAsk`) whose agent is NOT live in this runtime's
    /// tree. Runs once at TUI startup, where nothing is live yet — a modal
    /// ask child left behind by a crashed/killed TUI process has no modal
    /// that will ever show its answer, so no user will ever make the
    /// fork/pull-in/discard choice for it; leaving it would accumulate
    /// unreachable scratchpad sessions.
    ///
    /// **`AskOrigin::ToolAsk` sessions are NEVER swept** (the whole reason
    /// the tag exists — see `conway_core::log::AskOrigin`'s doc): a
    /// `conway_ask` tool child's transcript is referenced by an
    /// `EphemeralSessionRef` artifact in the calling agent's persisted
    /// `ToolOutput`, and purging it would leave that artifact dangling
    /// (P-2). Untagged (`None`) ephemeral sessions — every header written
    /// before the tag existed — are likewise never swept.
    ///
    /// **Not-live caution (the same one B1's `remove` guard matrix assigns
    /// to the facade layer):** a session whose agent IS live in
    /// `Runtime::tree()` is skipped, not purged — so although this is
    /// intended for startup (empty tree, everything eligible is residue),
    /// calling it later is still safe: it can never purge a session out
    /// from under a live agent loop.
    ///
    /// **Cross-process liveness (S1 follow-up):** BEFORE reaping, the sweep
    /// asks the store for its cross-process liveness marker
    /// ([`SessionStore::live_owner`]). If a marker is present AND its
    /// `heartbeat` is within [`SWEEP_LIVE_THRESHOLD`] (60s) of now, ANOTHER
    /// process is actively using this store directory, so the sweep returns
    /// `Ok(0)` immediately — it defers ENTIRELY rather than risk purging
    /// that process's open modal-ask child (which is not in THIS runtime's
    /// tree and so would otherwise look like residue). The owning process's
    /// periodic [`Conway::heartbeat_live_owner`] keeps the marker fresh;
    /// when it exits cleanly ([`Conway::clear_live_owner`]) or crashes (the
    /// marker goes stale), the NEXT startup's sweep reaps the residue. This
    /// closes the "two TUIs on one store dir" gap scoped by S1 without a
    /// `kill(0)` pid check: freshness + clean-shutdown removal cover crash
    /// recovery and pid-reuse (a dead process stops heartbeating). The
    /// caller publishes its OWN marker AFTER the sweep (see `tui::run`), so
    /// a sweep never sees its own marker. Full multi-instance coordination
    /// (e.g. a second process sweeping its OWN prior residue while the
    /// first is live) is out of scope — the store-level marker means a
    /// second process defers ALL sweeping while another is live.
    ///
    /// Best-effort per session: a session whose `remove` fails (e.g. it has
    /// since acquired children) is skipped and counting continues — the
    /// sweep is janitorial, and a leftover simply stays for the next
    /// startup's sweep. Returns the number of sessions purged.
    pub async fn sweep_stale_modal_asks(&self) -> Result<usize> {
        // S1 follow-up: if another process is actively using this store, defer
        // entirely. The caller publishes its OWN marker only AFTER the sweep,
        // so a marker read here is necessarily someone else's. A `live_owner`
        // error is treated as "no owner" (reap) — the sweep is best-effort and
        // must never block startup, and a store that cannot report liveness
        // behaves as a cold store (the same as one with no marker).
        if let Some(owner) = self.store.live_owner().await.unwrap_or(None) {
            if Utc::now().signed_duration_since(owner.heartbeat) <= SWEEP_LIVE_THRESHOLD {
                return Ok(0);
            }
        }
        let sessions = self
            .store
            .list(SessionFilter {
                include_ephemeral: true,
                ..SessionFilter::default()
            })
            .await?;
        let snapshot = self.rt.tree();
        let mut purged = 0;
        for meta in sessions {
            if meta.ask_origin != Some(conway_core::log::AskOrigin::ModalAsk) {
                continue;
            }
            // Never purge a live agent's session out from under it (see the
            // doc above).
            if snapshot.nodes.iter().any(|n| n.agent_id == meta.agent_id) {
                continue;
            }
            if self.store.remove(&meta.id).await.is_ok() {
                purged += 1;
            }
        }
        Ok(purged)
    }

    /// Publishes or refreshes THIS process's store liveness marker — the
    /// cross-process "I am using this store directory" signal consulted by
    /// [`sweep_stale_modal_asks`](Self::sweep_stale_modal_asks) (S1 follow-up).
    /// `pid` is the owning process's OS pid (diagnostic). The TUI calls this
    /// at startup AFTER the sweep and then a heartbeat task calls it on an
    /// interval, so a second process starting against the same store sees a
    /// fresh marker and defers its sweep rather than reaping this process's
    /// open modal-ask child. Best-effort: a failure here is non-fatal (the
    /// marker is a cache, not a durable record) and the caller should log
    /// and continue.
    pub async fn heartbeat_live_owner(&self, pid: u32) -> Result<()> {
        Ok(self.store.touch_live_owner(pid).await?)
    }

    /// Removes THIS process's store liveness marker — called on clean TUI
    /// shutdown so a subsequent cold start reaps residue immediately instead
    /// of waiting for the marker to go stale. Best-effort and non-fatal: a
    /// missing marker (cleared already, or never written) is success.
    pub async fn clear_live_owner(&self) -> Result<()> {
        Ok(self.store.clear_live_owner().await?)
    }

    /// Classifies a natural-language `/fork`/`/spawn` request into an
    /// [`AgentIntent`] (C1) by running an EPHEMERAL one-turn classification
    /// session under the declarative `intent` role, then purging that
    /// session before returning — on every exit path. The full design
    /// (session shape, prompt-prefix system prompt, the unconfigured-role
    /// passthrough fallback, the P-10 reply-validation policy, and every
    /// disclosed residual) lives in the `intent` module's doc; this is
    /// the one-method delegation this item is scoped to add here.
    ///
    /// `parent` is the caller's current live agent (the TUI's focused
    /// session root): the intent session attaches under it as an ephemeral
    /// spawn child for the few moments it exists. `default_recipe` is the
    /// CALLER's command default (`Fork` when the user typed `/fork`,
    /// `Spawn` for `/spawn`): every degraded path — unconfigured role,
    /// unparseable reply, invalid recipe, empty prompt — returns a verbatim
    /// passthrough `AgentIntent` carrying THIS recipe, the raw `text`, and
    /// no agent def, so a classifier failure can never break the command.
    pub async fn classify_agent_intent(
        &self,
        parent: AgentId,
        default_recipe: SubagentMode,
        text: &str,
    ) -> Result<AgentIntent> {
        crate::intent::classify(&self.rt, &self.store, &self.config, parent, default_recipe, text)
            .await
    }
}

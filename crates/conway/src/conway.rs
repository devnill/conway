//! `Conway`: the live, assembled facade over one `conway-runtime::Runtime`
//!. Constructed exclusively via `crate::builder::ConwayBuilder::build`.

use std::sync::Arc;

use chrono::{Duration as ChronoDuration, Utc};
use conway_core::agent::{AgentDefRef, Budget, SubagentMode};
use conway_core::capabilities::RequiredCaps;
use conway_core::error::{RuntimeError, StoreError};
use conway_core::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SessionId};
use conway_core::log::{LogRecord, SessionFilter, SessionMeta};
use conway_core::ports::{RoutingExplainer, SessionStore};
use conway_core::routing::{ExplainReport, MinimalRouter, RouteRequest};
use conway_runtime::runtime::{ResumeSpec, RootSpec, Runtime};

use crate::config::model_metadata::ModelMetadata;
use crate::config::{ConfigWarning, ConwayConfig};
use crate::error::{FacadeError, Result};
use crate::intent::AgentIntent;
use crate::permissions::{
    PermissionLoadReport, RevokeOutcome, TrustPermissionReport, TrustPreview,
};
use crate::session_handle::{SessionHandle, SessionSpec};
use crate::subagent_spec::ForkSpec;

/// The event name [`Conway::active_deny_capable_hook_rules`]/
/// [`Conway::revoke_hook_rule`] use to identify a `pre_tool_use` row --
/// exactly `conway_core::hook::HookEvent::name`'s own wire spelling for that
/// event, and `crate::config::schema::HookEntry::event`'s expected value.
/// `conway_runtime::hook_dispatch` does not export a sibling constant for
/// this event (it is dispatched by the permission broker, a different
/// module -- see that module's own "Why this is not on `PermissionBroker`"
/// doc), so this crate names it once here rather than repeating the string
/// literal at both this constant's call sites.
const PRE_TOOL_USE_EVENT: &str = "pre_tool_use";

/// [`HookRuleView::origin`]'s value for every row [`Conway::
/// active_deny_capable_hook_rules`] returns. A hook rule, unlike a pattern
/// ALLOW/DENY/PROMPT rule, has no per-rule [`conway_core::permission_pattern
/// ::PatternOrigin`] to report: `[hooks].rules` is a single array that
/// replaces WHOLESALE per config layer (`crate::config::merge`'s own module
/// doc, "arrays and scalars replace wholesale"), never a union of several
/// files' entries the way a `permissions.json` grant's provenance is -- so
/// there is exactly one place every hook rule can ever have come from: the
/// final merged config. Reporting a specific layer name (default/user/
/// project/env/CLI) would need knowing which layer's `[hooks]` table
/// actually won, which nothing downstream of `config::merge::load` still
/// tracks once the merge is done; this label says what IS known, honestly,
/// rather than fabricating a per-rule path no mechanism here can attribute.
const HOOK_ORIGIN_LABEL: &str = "settings.json (merged config)";

/// One row [`Conway::active_deny_capable_hook_rules`] hands the `/settings`
/// review surface -- a hook-backed
/// rule that can currently deny a call, addressed for revocation by its
/// `(event, id)` pair via [`Conway::revoke_hook_rule`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookRuleView {
    /// The rule's operator-chosen `HookEntry::id`.
    pub id: String,
    /// The event this rule fires on -- always `PRE_TOOL_USE_EVENT` or
    /// [`conway_runtime::hook_dispatch::PROMPT_SUBMITTED`], the only two
    /// events this list ever reports (see [`Conway::
    /// active_deny_capable_hook_rules`]'s own doc for why).
    pub event: String,
    /// The rule's `HookEntry::match_tool`. `None` fires this rule for every call
    /// this event dispatches; `Some(pattern)` narrows to calls whose tool
    /// name satisfies `pattern` (exact match, or a `*`-glob) -- only ever
    /// meaningful for `pre_tool_use` (`prompt_submitted`'s payload carries
    /// no tool name at all, so `merge::validate` refuses to load a config
    /// pairing `match` with it in the first place).
    pub match_tool: Option<String>,
    /// Always `HOOK_ORIGIN_LABEL` today -- see that constant's own doc
    /// for why a hook rule has no finer-grained provenance to report.
    pub origin: String,
}

/// The live, assembled facade: one `Runtime`, its resolved config, and --
/// when the builder had a real answer to "why" (it compiled its own router,
/// or a `RouterFactory` supplied one
///) -- the `RoutingExplainer` `explain_routing`
/// projects through.
///
/// Cheap to `Clone`: every field is an `Arc`.
#[derive(Clone)]
pub struct Conway {
    rt: Arc<Runtime>,
    config: Arc<ConwayConfig>,
    // Also cloned into every `SessionHandle` this `Conway` mints
    // (`new_session`), which needs it for `SessionHandle::transcript`'s
    // ancestry walk.
    store: Arc<dyn SessionStore>,
    router_explain: Option<Arc<dyn RoutingExplainer>>,
    warnings: Arc<Vec<ConfigWarning>>,
    // T3 follow-up: the local model-metadata map `ConwayBuilder::build`
    // already loads (`[models.metadata_path]`, step 2) to construct the
    // `CapabilityIndex` -- kept here too so `Self::model_metadata` can hand
    // the SAME loaded map back out. Before this field existed, every
    // consumer of that file (the TUI's `App::new`, ~app.rs) re-read and
    // re-parsed it from disk on its own, a second code path that agreed
    // with the builder's only by coincidence.
    model_metadata: Arc<ModelMetadata>,
    /// Set via
    /// `ConwayBuilder::with_root` -- see that method's own doc for the
    /// default (`None`, unconfined, unchanged) and the operator-facing
    /// contract. Consulted once per [`Self::new_session`] call, exactly like
    /// `self.config.cwd`.
    root: Option<std::path::PathBuf>,
    /// A build-time snapshot of every installed plugin's
    /// `Plugin::status_contributions()` (board item `01M03VKQ738DTGHHK2C4RWXC0E`).
    /// Collected in `ConwayBuilder::build` BEFORE the plugins are moved into
    /// `RuntimeDeps` -- so this is a snapshot taken at session-open, before any
    /// `status/1` notifications have arrived (typically empty). The LIVE
    /// surface a future TUI status-line render path will poll is the
    /// `Plugin::status_contributions` trait method itself (a polled snapshot of
    /// the session's per-key store); this field is the build-time record the
    /// facade can hand back WITHOUT re-reaching the (consumed) plugins --
    /// honest about being a snapshot, not a live view.
    plugin_status_contributions: Vec<conway_core::ports::PluginStatusContribution>,
}

impl Conway {
    // `Conway::new` is a crate-internal constructor called from exactly one
    // site (`ConwayBuilder::build`); its argument list grew to eight when the
    // facade began carrying the plugin status contributions the
    // `status.declare/1` wire point collects (board item
    // `01M03VKQ738DTGHHK2C4RWXC0E`). Clippy's default ceiling of seven is a
    // smell worth a note, not a refactor worth bundling unrelated fields into
    // a throwaway struct that would only obscure the single call site.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        rt: Arc<Runtime>,
        config: ConwayConfig,
        store: Arc<dyn SessionStore>,
        router_explain: Option<Arc<dyn RoutingExplainer>>,
        warnings: Vec<ConfigWarning>,
        model_metadata: ModelMetadata,
        root: Option<std::path::PathBuf>,
        plugin_status_contributions: Vec<conway_core::ports::PluginStatusContribution>,
    ) -> Self {
        Self {
            rt,
            config: Arc::new(config),
            store,
            router_explain,
            warnings: Arc::new(warnings),
            model_metadata: Arc::new(model_metadata),
            root,
            plugin_status_contributions,
        }
    }

    /// A build-time snapshot of every installed plugin's status contributions
    /// (board item `01M03VKQ738DTGHHK2C4RWXC0E`). See the field's own doc for
    /// why this is a snapshot (collected at session-open, before any
    /// `status/1` notifications arrive -- typically empty) rather than a live
    /// view; the live surface a render path will poll is the
    /// `Plugin::status_contributions` trait method. Exposed now so the wire
    /// half has a reachable facade consumer, scoped honestly to what it is.
    pub fn plugin_status_contributions(&self) -> &[conway_core::ports::PluginStatusContribution] {
        &self.plugin_status_contributions
    }

    /// Non-fatal warnings, from two sources: `config::load` (headroom-vs-
    /// context-window, a stale `[tui]` section -- empty when this `Conway`
    /// was built via `ConwayBuilder::from_parts`, which bypasses `load`
    /// entirely) and `ConwayBuilder::build` itself (a `PluginManifest::
    /// optional` dependency absent from the final installed plugin set --
    /// see `WarningCode::OptionalPluginDependencyMissing`'s own doc). Both
    /// share this one accessor rather than a second, since both are the
    /// same shape: a non-fatal, named, operator-facing notice.
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
    /// a permissions file, so the review surface can tell the two apart.
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
    /// `origin_path`.
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

    /// Installs a structured ALLOW grant (e.g. a
    /// [`conway_core::permission_pattern::When::ArgsMatch`] field-pinning
    /// rule) approved through the interactive gate -- origin `Interactive`.
    /// This is the structured-[`conway_core::permission_pattern::Rule`]
    /// sibling of [`Self::grant_permission_pattern`]: that one takes the flat
    /// [`conway_core::permission_pattern::PatternRule`] form; this one takes
    /// the full [`conway_core::permission_pattern::Rule`], which is the only
    /// way to install an `ArgsMatch` grant (the flat language cannot express
    /// it -- see
    /// [`conway_core::permission_pattern::Rule::args_match_allow_rule`]).
    /// Returns `false` only if the broker dropped the rule at install (a
    /// `PathsUnder` rule whose prefix could not be canonicalized); an
    /// `ArgsMatch` rule is never dropped, so this is `true` for the only
    /// constructor that builds one. The `base` passed to the broker is the
    /// confinement root -- unused for `ArgsMatch` (which carries no paths)
    /// but required by `remember_pattern_rule`'s signature.
    pub fn grant_permission_rule(
        &self,
        rule: conway_core::permission_pattern::Rule,
        scope: conway_core::agent::PermissionScope,
        granting_agent: conway_core::ids::AgentId,
    ) -> bool {
        let base = self
            .root
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new("."));
        self.rt.permission_broker().remember_pattern_rule(
            rule,
            scope,
            granting_agent,
            conway_core::permission_pattern::PatternOrigin::Interactive,
            base,
        )
    }

    /// Installs a DENY rule loaded from a permissions file at
    /// `origin_path`. Unlike the allow-side methods above, there is no
    /// trust precondition here at all -- `deny` applies immediately,
    /// trusted or not, from any file (D4 §3
    ///).
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
    /// `origin_path`. Mirrors
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

    /// B1: the `PathArgs` a registered tool declares for itself, by name, or
    /// `None` if no plugin registered that tool. The structured-rule
    /// registration check (`validate_rule_registration`) uses this to surface
    /// a typed `PathsUnderOnUnconfinedTool` error when a `paths_under`
    /// deny/prompt rule is paired with a tool whose `PathArgs` is not `Named`
    /// (`Unconfinable` such as `bash`, or `None`) -- a `paths_under` predicate
    /// can never confine such a tool, so a `then: deny/prompt` rule selecting
    /// it is silently inert (fail-OPEN). Reads the already-resolved tool the
    /// same way `tool_render_kind` does; no new resolution path.
    pub fn tool_path_args(
        &self,
        name: &conway_core::ids::ToolName,
    ) -> Option<conway_core::ports::PathArgs> {
        self.rt.tool_path_args(name)
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

    /// The structured half of the deny review list: every active deny
    /// [`conway_core::permission_pattern::Rule`] the flat form cannot
    /// express, paired with its origin. Deny rules install from ANY file,
    /// trusted or not (D4 §3), so an operator debugging "why does this keep
    /// refusing" needs to see them -- a rule set nobody can inspect is a
    /// trap. Read-only: deny rules are deliberately not revocable from the
    /// review surface (see [`Self::revoke_permission_pattern`]'s own doc).
    pub fn active_structured_deny_rules(
        &self,
    ) -> Vec<(
        conway_core::permission_pattern::Rule,
        conway_core::permission_pattern::PatternOrigin,
    )> {
        self.rt.permission_broker().active_structured_deny_rules()
    }

    /// The structured half of the ALLOW review list (A2): every
    /// active allow [`conway_core::permission_pattern::Rule`] the flat form
    /// cannot express (`paths_under`, `categories`, `category_in`, multiple
    /// tools), paired with its origin and the
    /// [`crate::GrantScope`] it was granted at --
    /// mirroring [`Self::active_permission_patterns`], which drops these
    /// rules by construction (`Rule::to_pattern_rule` returns `None` for
    /// them). The scope rides along because an allow rule is the one rule
    /// kind that IS scoped: a review surface that hid it would
    /// misrepresent how much of the agent tree a grant covers. The
    /// `(rule, origin)` pair is also exactly the identity
    /// [`Self::revoke_structured_allow_rule`] addresses, so a row built
    /// from this list can name itself back for revocation.
    ///
    /// Returns [`conway_core::agent::GrantScope`], not
    /// `conway_runtime::permission::GrantScope` -- the facade's own module
    /// doc denies exposing `conway-runtime` types publicly (Stage 2b,
    /// board item `01KZVYZM7BZRQ54RRB8P814KV9`), so the runtime's internal
    /// scope is converted here, at the boundary, via the `From` impl in
    /// `crates/conway-runtime/src/permission.rs`.
    pub fn active_structured_allow_rules(
        &self,
    ) -> Vec<(
        conway_core::permission_pattern::Rule,
        conway_core::permission_pattern::PatternOrigin,
        conway_core::agent::GrantScope,
    )> {
        self.rt
            .permission_broker()
            .active_structured_allow_rules()
            .into_iter()
            .map(|(rule, origin, scope)| (rule, origin, scope.into()))
            .collect()
    }

    /// The structured half of the prompt review list: every active prompt
    /// [`conway_core::permission_pattern::Rule`] the flat form cannot
    /// express, paired with its origin -- mirroring
    /// [`Self::active_structured_deny_rules`]. Like deny, prompt rules
    /// install from any file unconditionally (they only ever narrow), so
    /// their inspection surface matters for exactly the same reason.
    pub fn active_structured_prompt_rules(
        &self,
    ) -> Vec<(
        conway_core::permission_pattern::Rule,
        conway_core::permission_pattern::PatternOrigin,
    )> {
        self.rt.permission_broker().active_structured_prompt_rules()
    }

    /// Every currently-installed DENY-CAPABLE hook-backed rule -- every `pre_tool_use` hook, then every
    /// `prompt_submitted` subscriber, in that order.
    ///
    /// **Scoped to the two DENY-CAPABLE events, deliberately, not every
    /// dispatched event.** `PHILOSOPHY.md` §5's own guarantee ("[a hook]
    /// appears wherever other permission rules appear, and it is
    /// individually revocable") is about PERMISSION rules -- a rule that
    /// can say no to a call. `pre_tool_use` (narrows a tool call) and
    /// `prompt_submitted` (narrows a submitted prompt,
    /// `conway_runtime::hook_dispatch`'s own module doc) are the only two
    /// events [`conway_core::hook::HookPermissionVerdict`] is ever read
    /// for; every other configured event -- `post_tool_use`,
    /// `session_starting`, `child_spawned`, `request_assembled`,
    /// `child_reported` -- is observation-only and cannot silently widen
    /// what this session allows by staying enabled, so admitting them here
    /// would blur this list's own contract ("a rule that can currently
    /// block a call") with a DIFFERENT, still-open concern: that an
    /// observation hook still runs an arbitrary command with the
    /// operator's own privileges is real, but it is not a permission-rule
    /// visibility gap, and a general hook-inventory surface for that
    /// concern is not this item's to build.
    ///
    /// A hook installs here regardless of whether its script currently
    /// resolves -- see [`conway_runtime::permission::PermissionBroker::
    /// active_pre_tool_use_hooks`]'s own doc: a broken/missing script is
    /// not detected until invocation, so it is never silently absent from
    /// this list.
    pub fn active_deny_capable_hook_rules(&self) -> Vec<HookRuleView> {
        let mut rows: Vec<HookRuleView> = self
            .rt
            .pre_tool_use_hooks()
            .into_iter()
            .map(|hook| HookRuleView {
                id: hook.id,
                event: PRE_TOOL_USE_EVENT.to_string(),
                match_tool: hook.matcher,
                origin: HOOK_ORIGIN_LABEL.to_string(),
            })
            .collect();
        rows.extend(
            self.rt
                .observation_hooks()
                .remove(conway_runtime::hook_dispatch::PROMPT_SUBMITTED)
                .unwrap_or_default()
                .into_iter()
                .map(|hook| HookRuleView {
                    id: hook.id,
                    event: conway_runtime::hook_dispatch::PROMPT_SUBMITTED.to_string(),
                    match_tool: hook.matcher,
                    origin: HOOK_ORIGIN_LABEL.to_string(),
                }),
        );
        rows
    }

    /// Revokes exactly ONE hook-backed rule , addressed by its `(event, id)` pair --
    /// the same identity [`Self::active_deny_capable_hook_rules`] hands the
    /// review surface. `event` must be exactly `"pre_tool_use"` or
    /// `"prompt_submitted"`; any other value (including an observation-only
    /// event, which this review surface never lists in the first place)
    /// returns `false` without touching anything.
    ///
    /// **Session-scoped only, never persisted -- matching every other
    /// `/settings` toggle** (`conway_cli::tui::view::settings`'s own module
    /// doc: "Conway's config load ... has no writer anywhere outside test
    /// fixtures ... persisting a runtime toggle would mean inventing one").
    /// Unlike a pattern ALLOW grant, a hook rule has no `PatternOrigin` at
    /// all -- it is never addressable to a specific file the way a
    /// `permissions.json` rule is (`[hooks].rules` replaces wholesale per
    /// config layer, `crate::config::merge`'s own module doc: "arrays and
    /// scalars replace wholesale") -- so there is no file-rewrite branch to
    /// choose between here the way [`Self::revoke_permission_pattern`] has;
    /// this always takes the `PatternOrigin::Interactive`-shaped path:
    /// drop it from the broker/dispatcher's in-memory list, nothing else.
    ///
    /// Returns `true` if a rule with that `(event, id)` was actually
    /// installed and is now gone; `false` if it was already absent (the
    /// same "was already gone" outcome the other revoke actions surface as
    /// a notice rather than an error).
    pub fn revoke_hook_rule(&self, event: &str, id: &str) -> bool {
        match event {
            PRE_TOOL_USE_EVENT => {
                let mut hooks = self.rt.pre_tool_use_hooks();
                let before = hooks.len();
                hooks.retain(|hook| hook.id != id);
                let changed = hooks.len() != before;
                if changed {
                    self.rt.set_pre_tool_use_hooks(hooks);
                }
                changed
            }
            conway_runtime::hook_dispatch::PROMPT_SUBMITTED => {
                let mut hooks = self.rt.observation_hooks();
                let changed = match hooks.get_mut(conway_runtime::hook_dispatch::PROMPT_SUBMITTED) {
                    Some(subscribers) => {
                        let before = subscribers.len();
                        subscribers.retain(|hook| hook.id != id);
                        subscribers.len() != before
                    }
                    None => false,
                };
                if changed {
                    self.rt.set_observation_hooks(hooks);
                }
                changed
            }
            _ => false,
        }
    }

    /// Drops every pattern ALLOW grant and cached `AllowAlways`, returning
    /// the session to asking (V2b). Deliberately leaves `deny` rules in
    /// force -- see `PermissionBroker::revoke_all_grants`'s own doc.
    pub fn revoke_permission_grants(&self) {
        self.rt.permission_broker().revoke_all_grants();
    }

    /// Revokes exactly ONE pattern ALLOW grant , addressed by the same `(rule, origin)`
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
            // A4: a plugin-contributed allow rule is refused at admission
            // (`remember_pattern_rule` rejects `PatternOrigin::Plugin`), so
            // this arm is unreachable from the review surface -- a
            // `Plugin`-origin allow rule can never be installed, and a
            // `Plugin`-origin deny/prompt rule is not revocable through
            // this surface at all. Treat it like `Interactive` (no file to
            // rewrite): the broker's in-memory grant was already dropped by
            // `revoke_pattern` above, so there is nothing left to persist.
            conway_core::permission_pattern::PatternOrigin::Plugin => {
                return RevokeOutcome::RevokedNoFile;
            }
        };

        crate::permissions::persist_revoke_outcome(
            env,
            path,
            crate::permissions::rewrite_permission_file_removing(path, rule),
        )
    }

    /// Revokes exactly ONE STRUCTURED allow rule (A2) -- the
    /// counterpart to [`Self::revoke_permission_pattern`] for the rules the
    /// flat form cannot express. Addressed by the same `(rule, origin)`
    /// pair [`Self::active_structured_allow_rules`] hands the review
    /// surface, matched by full
    /// [`conway_core::permission_pattern::Rule`] equality
    /// (`PermissionBroker::revoke_pattern_rule`) rather than the flat
    /// `to_pattern_rule()` projection -- which is the whole point:
    /// `revoke_permission_pattern`'s key collapses every structured rule
    /// to `None`, so it returns [`RevokeOutcome::NotFound`] for one even
    /// while the rule is installed and authorizing calls.
    ///
    /// Every guarantee of [`Self::revoke_permission_pattern`] carries over
    /// unchanged -- see that method's own doc for the full reasoning:
    /// revocation never fails open (the broker's in-memory grant is dropped
    /// FIRST, before any file I/O); an `Interactive` origin touches no file
    /// ([`RevokeOutcome::RevokedNoFile`]); a `File` origin is rewritten
    /// read-modify-write, tmp-then-rename, removing the rule from that
    /// exact file's structured `rules` array (never the flat `allow` list
    /// -- a structured rule's wire form lives in `rules`; an unparseable
    /// file is a hard persist failure, never blindly overwritten); and a
    /// rewritten TRUSTED PROJECT file is re-trusted for its new, strictly
    /// narrower bytes, the same explicit-action narrowing exception D4
    /// §5/§9 admits for the flat path.
    ///
    /// Takes [`conway_core::agent::GrantScope`], not
    /// `conway_runtime::permission::GrantScope` -- see
    /// [`Self::active_structured_allow_rules`]'s doc for why; converted
    /// back to the runtime's own type here, at the boundary, before
    /// addressing `PermissionBroker::revoke_pattern_rule`.
    pub fn revoke_structured_allow_rule(
        &self,
        env: &std::collections::HashMap<String, String>,
        rule: &conway_core::permission_pattern::Rule,
        origin: &conway_core::permission_pattern::PatternOrigin,
        scope: &conway_core::agent::GrantScope,
    ) -> RevokeOutcome {
        let rt_scope: conway_runtime::permission::GrantScope = (*scope).into();
        if !self
            .rt
            .permission_broker()
            .revoke_pattern_rule(rule, origin, &rt_scope)
        {
            return RevokeOutcome::NotFound;
        }

        let path = match origin {
            conway_core::permission_pattern::PatternOrigin::Interactive => {
                return RevokeOutcome::RevokedNoFile;
            }
            conway_core::permission_pattern::PatternOrigin::File(path) => path,
            // A4: see `revoke_permission_pattern`'s own `Plugin` arm -- a
            // plugin allow rule is refused at admission, so this arm is
            // unreachable; treat it like `Interactive` (no file to rewrite).
            conway_core::permission_pattern::PatternOrigin::Plugin => {
                return RevokeOutcome::RevokedNoFile;
            }
        };

        crate::permissions::persist_revoke_outcome(
            env,
            path,
            crate::permissions::rewrite_permission_file_removing_structured(path, rule),
        )
    }

    /// Loads permissions files project-first then global
    /// (`crate::config::discovery::permission_file_paths`) and installs
    /// their rules -- **the real production startup seam**
    ///. `conway-cli`'s TUI (`tui::app::App::new`)
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
    ///   "every permissions-file failure is silent and narrowing" posture,
    ///   which still holds for content that is not valid JSON at all, or
    ///   that names a recognized key with the wrong shape).
    ///
    /// **One deliberate exception to that silent posture** -- a file naming a top-level key this schema
    /// does not recognize -- `"denys"` for `"deny"`, or any other typo -- is
    /// checked FIRST, via `conway_core::permission_pattern::
    /// permission_file_unknown_field_error`, before any rule from that file is
    /// parsed. When it fires, the file contributes NOTHING (not even its
    /// correctly-spelled rules) and a message naming the offending key is
    /// pushed to [`PermissionLoadReport::parse_errors`] -- LOUD, unlike every
    /// other condition here. This is the one case where silence would be the
    /// defect rather than the safety margin: `deny`'s whole point is that it
    /// always applies regardless of trust, so a `deny` rule that silently never
    /// installs because of a typo is exactly the fail-open outcome the
    /// asymmetry above exists to rule out -- an unenforceable narrowing rule
    /// denies rather than silently matching nothing.
    ///
    /// `scope` is the [`crate::PermissionScope`] every installed ALLOW rule is
    /// remembered at (`deny`/`prompt` rules are unscoped by design -- they
    /// only ever narrow, so they apply to every requester). The TUI passes
    /// `Session`; an embedder that loads a file on behalf of ONE agent (or
    /// one subtree) passes `Agent`/`AgentSubtree` with the corresponding
    /// `granting_agent`, so a file's rules never silently cover more of
    /// the agent tree than the embedder intended -- the same
    /// least-privilege choice the permission prompt's `s` key offers
    /// interactively.
    pub fn load_permission_files(
        &self,
        cwd: &std::path::Path,
        env: &std::collections::HashMap<String, String>,
        scope: conway_core::agent::PermissionScope,
        granting_agent: conway_core::ids::AgentId,
    ) -> PermissionLoadReport {
        crate::permissions::load_permission_files(&self.rt, cwd, env, scope, granting_agent)
    }

    /// Reads `path`'s current bytes and reports whether trusting it now
    /// would be a first trust, a re-trust of a file that changed since it
    /// was last trusted, or a no-op re-confirmation of an already-current
    /// trust record -- WITHOUT recording anything. This is the read-only
    /// half of a trust decision: `conway-cli`'s `/trust permissions` calls
    /// this FIRST, shows the operator the result, and only calls
    /// [`Self::trust_permission_file`] after an explicit confirm -- so the
    /// operator is shown what they are about to trust before, not after,
    /// deciding (board item, split from `01KZHVFCN6ZEAXV7K5JHRQN1YB`'s
    /// `(kind, id, digest)`/plugin-subject generalisation, which this does
    /// not pre-empt).
    ///
    /// Returns [`TrustPreview`]'s `contents` field as the file's CURRENT
    /// bytes only -- never a diff against what a prior trust decision
    /// covered. `crate::config::trust::TrustStore` retains only a digest of
    /// a prior decision, never its content (see that module's own doc), so
    /// there is nothing to diff against even when `status` reports
    /// [`crate::config::trust::TrustStatus::Changed`]; see
    /// [`TrustPreview`]'s own doc for the full reasoning and where that
    /// limit is stated to the operator.
    ///
    /// Returns `Err` only for the ordinary `std::io::Error` an unreadable
    /// `path` produces -- unlike [`Self::trust_permission_file`], there is
    /// no unrecognized-top-level-key check here: this function never
    /// writes anything, so there is nothing a bad file would silently
    /// bless.
    pub fn preview_trust_target(
        &self,
        env: &std::collections::HashMap<String, String>,
        path: &std::path::Path,
    ) -> std::io::Result<TrustPreview> {
        crate::permissions::preview_trust_target(env, path)
    }

    /// Records an explicit trust decision for `path`'s CURRENT bytes on
    /// disk (`crate::config::trust::TrustStore::trust`) and immediately
    /// installs its `allow` rules for this running session -- so trusting
    /// takes effect now, not only on the next restart. Returns a
    /// [`TrustPermissionReport`] carrying the number of allow rules
    /// ACTUALLY installed by the broker AND any typed registration errors
    /// for rules the broker refused to install (B3: a `paths_under` prefix
    /// that fails to canonicalize). `scope` is the [`crate::PermissionScope`] the
    /// rules are remembered at, exactly as in
    /// [`Self::load_permission_files`].
    ///
    /// This is the ONLY path that writes a trust record, and it is only
    /// ever invoked by an explicit operator action (the TUI's `/trust
    /// permissions`) -- never automatically, never as a side effect of
    /// [`Self::load_permission_files`] (///, D4 §5/§9: no startup prompt, no silent
    /// self-trust).
    ///
    /// Returns `Err` -- WITHOUT writing a trust record and WITHOUT
    /// installing any rule -- for two reasons: the ordinary `std::io::Error`
    /// an unreadable `path` or a failed `TrustStore::trust` write produces,
    /// and, checked first, `path` naming an unrecognized top-level key (board
    /// item; see
    /// [`conway_core::permission_pattern::permission_file_unknown_field_error`]'s
    /// own doc). The second case exists because a typo'd file's rules were
    /// never going to install anyway -- recording trust for it first would
    /// bless content that silently enforces nothing on every subsequent
    /// load, exactly the failure this whole closes, just moved
    /// to the trust-recording path instead of the load path. Both `Err`
    /// cases carry a message naming `path` and, for the unknown-key case,
    /// the offending field.
    pub fn trust_permission_file(
        &self,
        env: &std::collections::HashMap<String, String>,
        path: &std::path::Path,
        scope: conway_core::agent::PermissionScope,
        granting_agent: conway_core::ids::AgentId,
    ) -> std::io::Result<TrustPermissionReport> {
        crate::permissions::trust_permission_file(
            &self.rt,
            env,
            path,
            scope,
            granting_agent,
            &self.config.cwd,
        )
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
    /// `Runtime::start_root` already builds the `SessionMeta` and
    /// calls `SessionStore::create` internally. Calling `store.create` again
    /// here with the same id would double-create the session (an error
    /// against both `FakeStore` and `JsonlSessionStore`, which reject a
    /// duplicate id). Instead, this method generates the `SessionId` itself
    /// and passes it through `RootSpec::session`, so `start_root`'s own
    /// internal `store.create` call is the single, authoritative creation --
    /// this still satisfies "creates a session via `SessionStore::create`",
    /// it just avoids invoking that call a second time.
    ///
    /// **Caller-chosen id:** `spec.id` -- when `Some` -- is passed
    /// through unchanged instead of the freshly minted `SessionId` below.
    /// `RootSpec::session` already supports this at the runtime
    /// layer; this is the facade-side wiring to reach it. An id already
    /// present in the store surfaces as `start_root`'s own
    /// `SessionStore::create` failure --
    /// `Err(FacadeError::Runtime(RuntimeError::Store(StoreError::AlreadyExists
    /// { .. })))`, propagated unchanged through the `?` below -- typed and
    /// distinct from every other failure this method can produce, not a
    /// generic error.
    ///
    /// **`SessionSpec::labels` reaches the created session** (board item
    /// `01M0989GZ0PQAW0TN7APY1PHYW`): passed straight through to
    /// `RootSpec::labels`, which `start_root` stamps onto the new session's
    /// `SessionMeta.labels` -- see that field's own doc for why fork/spawn
    /// children deliberately do NOT inherit it. `config.limits.
    /// max_parallel_tools` remains a disclosed gap: `RootSpec` still has no
    /// field for it, so it does not reach the created session/agent through
    /// this method -- out of this item's file scope to add.
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
            // this `Conway`'s own
            // confinement root (`ConwayBuilder::with_root`), if the operator
            // set one -- `None` (unconfined) otherwise. `SessionSpec` has no
            // per-session override for this: unlike `cwd`/`role`/`model`,
            // root confinement is a whole-invocation setting an operator
            // opts into once, not something a caller varies per session.
            root: self.root.clone(),
            prompt: None,
            keep_alive: spec.keep_alive,
            model: spec.model,
            system_prompt_override: spec.system_prompt_override,
            // `SessionSpec::result_contract`'s own doc has the
            // call-site-wins-over-agent-def precedence; `RootSpec::
            // result_contract`'s own doc has the enforcement mechanism.
            result_contract: spec.result_contract,
            labels: spec.labels,
        };
        let root = self.rt.start_root(root_spec).await?;

        Ok(SessionHandle::new(
            self.rt.clone(),
            session,
            root,
            self.store.clone(),
        ))
    }

    /// `config.limits` resolved into a `Budget`. Every dimension follows the
    /// same convention: a `0` in config means "no ceiling" and maps to
    /// `None`, since `Budget`'s own optionality is what the runtime gates on.
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
            max_tool_calls: if limits.max_tool_calls == 0 {
                None
            } else {
                Some(limits.max_tool_calls)
            },
        }
    }

    /// The "why did this model run, and why not the others" report for
    /// `role`, projected through the concrete `DeclarativeRouter` this
    /// `Conway` compiled itself.
    ///
    /// When the builder instead received an injected `Router`
    /// (`ConwayBuilder::with_router`) with no `RouterFactory`-supplied
    /// explainer either, there is no `RoutingExplainer` to project through
    /// at all -- `router_explain` is `None`. This used to fall back to a
    /// fabricated-empty report (`entries: vec![]`), which `conway routes
    /// explain` then misread as "unknown role" for a perfectly valid one
    /// (a silent inversion of what the surface claimed
    ///).
    /// It now falls back to `conway_core::routing::MinimalRouter`,
    /// projected over this `Conway`'s own resolved `RoutingConfig` -- an
    /// honestly degenerate answer (no capability filtering, no health
    /// filtering, one entry per configured chain candidate) rather than an
    /// empty one.
    pub fn explain_routing(&self, role: &RoleAlias) -> ExplainReport {
        let req = RouteRequest {
            role: role.clone(),
            pin: None,
            required: RequiredCaps::default(),
            est_tokens: 0,
            agent_id: AgentId::new(),
        };
        match &self.router_explain {
            Some(explainer) => explainer.explain(&req),
            None => {
                let routing_config =
                    self.config
                        .routing()
                        .unwrap_or_else(|_| conway_core::routing::RoutingConfig {
                            roles: std::collections::BTreeMap::new(),
                            health: conway_core::routing::HealthConfig::default(),
                            default_headroom_tokens:
                                conway_core::capabilities::DEFAULT_HEADROOM_TOKENS,
                        });
                MinimalRouter::new(routing_config).explain(&req)
            }
        }
    }

    /// Reattaches to a persisted session, now as a DRIVABLE handle.
    ///
    /// **Resolved:** this method's previous doc disclosed a real
    /// gap -- `conway-runtime` exposed only `start_root`, which cannot be
    /// repurposed for resume (it unconditionally `store.create`s, which
    /// every committed `SessionStore` rejects for an id that already has a
    /// persisted session). A later change closed that gap by adding
    /// `Runtime::resume_root(ResumeSpec)`: it reads the existing
    /// `SessionMeta` via `store.meta` (no `store.create`), re-registers
    /// `meta.agent_id` into `Runtime`'s `agents` map and `AgentTree` through
    /// the same `launch_agent` path `start_root` uses, and gates the
    /// resumed `AgentLoop`'s first iteration behind a `ResumeGate` so it
    /// idles until this handle's own first `SessionHandle::prompt` call --
    /// never racing the (already-completed) persisted transcript. This
    /// method now calls it directly, which resolves both criteria the
    /// earlier doc could not satisfy:
    /// - `prompt()` after resume: `Runtime::prompt` now finds `agent` in
    ///   `Runtime.agents` (registered by `resume_root` below), so it appends
    ///   and wakes the gated loop instead of returning `AgentNotFound`.
    /// - `tree()`: `resume_root` attaches the resumed root to `AgentTree`,
    ///   so `SessionHandle::tree()` (`self.rt.tree()`, unchanged)
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
    /// `FacadeError::Runtime(RuntimeError::Store(_))`, one layer deeper than
    /// this method returned earlier (`FacadeError::Store(_)` directly, from
    /// this method's own former `store.meta` call). `resume`'s existing test
    /// suite asserts the flat shape, and nothing about resuming a session
    /// makes "the store doesn't have it" a *runtime* concern rather than a
    /// *store* one -- so this unwraps `RuntimeError::Store` back to
    /// `FacadeError::Store` explicitly, keeping every other `RuntimeError`
    /// variant (e.g. a future `resume_root` failure mode) under
    /// `FacadeError::Runtime` unchanged.
    pub async fn resume(&self, sid: SessionId) -> Result<SessionHandle> {
        self.resume_with(sid, None, None).await
    }

    /// [`Self::resume`], with an optional role and/or pinned-model override
    /// applied to the resumed agent -- the mechanism `conway-cli`'s
    /// `--model`/`--role-override`, combined with `--resume`, build on
    /// (INTENT.md §5c: "changing model mid-session is ordinary, and stays
    /// cheap"). `role: None, model: None` behaves exactly like
    /// [`Self::resume`] (indeed, `resume` is defined in terms of this
    /// method) -- neither override reaches `ResumeSpec`, so the resumed
    /// agent's role/pin resolve purely from the persisted `SessionMeta`/
    /// `agent_def`, unchanged.
    ///
    /// **Selection survives; only rendering does not (§5c).** The resumed
    /// agent's history is the SAME persisted log every other resume reads --
    /// nothing about which records are selected changes here. Only the
    /// RENDERING of that history -- the bytes the newly-pinned model (or the
    /// model the new role's chain resolves to) receives on its very next
    /// turn -- can differ. A role/model this session's persisted transcript
    /// does not fit is never silently trimmed or silently served under the
    /// OLD model instead: it surfaces as the same loud
    /// `RoutingError::ContextTooLarge` refusal an ordinary turn's admission
    /// gate already produces (`conway_runtime::agent_loop`'s router-facing
    /// `too_large` construction), naming what did not fit -- this method
    /// performs no fallback of its own.
    ///
    /// **Not a live, same-process switch.** Like [`Self::resume`], this
    /// re-registers the session's root agent as a freshly-launched
    /// `AgentLoop` task -- it cannot reach into an ALREADY-running task's own
    /// `AgentSpec` (fixed for that task's entire lifetime; there is no
    /// mutation channel for it) and has no effect on one still live in this
    /// same `Runtime`. `conway-cli`'s live, uninterrupted mid-conversation
    /// `/model`/`/role` instead fork a fresh interactive child under the new
    /// role/pin (`ForkSpec::role`/`ForkSpec::model`) and retarget input to
    /// it -- see that command's own doc for why: forking, unlike this
    /// method, is a mechanism that already reaches a LIVE session.
    pub async fn resume_with(
        &self,
        sid: SessionId,
        role: Option<RoleAlias>,
        model: Option<ModelRef>,
    ) -> Result<SessionHandle> {
        let agent = self
            .rt
            .resume_root(ResumeSpec {
                session: sid,
                agent_def: None,
                role,
                model,
                tools: None,
                budget: self.default_budget(),
                cwd: None,
                // `resume`/`resume_with` take no per-call spec to source a
                // contract from, so this is always `None`, exactly as before
                // `ResumeSpec::result_contract` existed. See that field's
                // own doc for the caller that CAN supply `Some`
                // (`Conway::fork_from`, via `ForkSpec::result_contract`).
                result_contract: None,
                // `resume`/`resume_with` take no per-call spec to source a
                // keep-alive flag from, so this is always `false`, exactly
                // as before `ResumeSpec::keep_alive` existed (preserving
                // `resume`'s existing one-shot behavior). See that field's
                // own doc for the caller that CAN supply `true`
                // (`Conway::fork_from`, via `ForkSpec::keep_alive`).
                keep_alive: false,
            })
            .await
            .map_err(|err| match err {
                RuntimeError::Store(inner) => FacadeError::Store(inner),
                other => FacadeError::Runtime(other),
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

    /// Appends a `LogRecord::ContextMask` to `sid`'s own log, masking (or,
    /// with `excluded: false`, un-masking) `target_seq` -- the host-side
    /// half of [`conway_core::ports::CommandOutcome::MaskRecord`]
    /// (`conway_plugin_history`'s `/conway.history.mask` command; board
    /// item 01KZY8QRAVVVKCRBZ6HAEGW3GG, "`/checkout` and a reachable
    /// `ContextMask`"), and the first production call site the enum
    /// variant construction guard (`crates/conway/tests/
    /// enum_variant_construction_guard.rs`) recognizes for it.
    ///
    /// **An ordinary append, not a mutation.** Like every other
    /// `LogRecord`, this is a plain `SessionStore::append` -- `target_seq`'s
    /// own stored bytes are never touched (`LogRecord::ContextMask`'s own
    /// doc). Reversible by construction: appending a second mask for the
    /// same `target_seq` with the opposite `excluded` value is the whole
    /// undo mechanism; there is no separate "unmask" record shape.
    ///
    /// **`seq`/`ts` are placeholders the store overwrites.** Mirrors every
    /// other record built ahead of an `append` call in this crate (e.g.
    /// [`Self::fork_from`]'s own child-creation path) -- `seq` becomes
    /// whatever `SessionStore::append` actually assigns (returned here),
    /// and `ts` is stamped at the moment of the call, not validated against
    /// anything.
    ///
    /// **Bounds, disclosed:** this performs no check that `target_seq` is
    /// actually `<= session_head(sid)` -- `SessionStore::append` itself
    /// enforces no such bound (unlike `Self::fork_from`'s explicit `at >
    /// head` check), so masking a seq that does not exist yet succeeds
    /// today and simply has no visible effect until a record with that seq
    /// is appended. Tightening this is a disclosed follow-up, not a defect
    /// this item's own acceptance criteria require closing (`CommandOutcome::
    /// MaskRecord::target_seq`'s own doc names the identical limit).
    pub async fn mask_record(
        &self,
        sid: SessionId,
        target_seq: LogSeq,
        excluded: bool,
    ) -> Result<LogSeq> {
        Ok(self
            .store
            .append(
                &sid,
                LogRecord::ContextMask {
                    seq: LogSeq(0),
                    ts: Utc::now(),
                    target_seq,
                    excluded,
                },
            )
            .await?)
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
    /// Reuses [`ForkSpec`] rather than a parallel type, per the
    /// binding notes. `directive` still has no session-level counterpart --
    /// `conway_core::log::SessionMeta` carries none, and there is no live
    /// child turn here to attach a `LogRecord::ForkDirective` to (the child
    /// session is *created* with zero records, store-side, exactly as
    /// before) -- so only `agent_def` and `role` are consulted for the
    /// persisted `SessionMeta`, as overrides onto the parent's own values.
    ///
    /// **`result_contract` IS honored (board item
    /// `01M03FQDF33AZ8G258516EDWQD`, closing a real gap disclosed by an
    /// earlier item):** unlike `directive`, a result contract is not
    /// session-metadata at all -- it lives on the live agent's own
    /// `AgentSpec` and is enforced at natural-completion time
    /// (`conway_runtime::agent_loop::AgentLoop::run_inner`), the exact
    /// mechanism [`SessionHandle::fork`](crate::SessionHandle::fork)'s live
    /// path already exercises via `SubagentSpec::result_contract`. Before
    /// this item, this method silently dropped `spec.result_contract`
    /// instead: `crate::fork_child::fork_child` built a `conway_runtime::
    /// runtime::ResumeSpec` with no field to carry it, so a caller that set
    /// [`ForkSpec::result_contract`] and called `fork_from` got a child with
    /// no contract, with nothing failing. `ResumeSpec::result_contract` (its
    /// own doc has the gap's full history) now closes that: `spec.
    /// result_contract` is passed straight through to `fork_child`'s
    /// `ForkChildRequest`, which threads it into the `ResumeSpec` this
    /// method's live registration below already builds.
    ///
    /// **`keep_alive` IS honored too (board item
    /// `01M03KZXR1KF77YRAW4W4GE6KK`, the second sibling of the same bug):**
    /// `ForkSpec::keep_alive(true)` threads through `ForkChildRequest` into
    /// `ResumeSpec::keep_alive` and onward into the live
    /// `AgentSpec::keep_alive`, so a forked child idles for its next prompt
    /// after each completed turn instead of terminating -- the same
    /// mechanism [`SessionHandle::fork`](crate::SessionHandle::fork)'s live
    /// path already exercises via `SubagentSpec::keep_alive`. Before this
    /// item, the flag was silently dropped (`resume_root` hardcoded
    /// `false`), so a caller that set `keep_alive(true)` got a one-shot
    /// child with no error.
    ///
    /// **`plugin_config` IS honored too (same item, the third sibling):**
    /// `ForkSpec::plugin_config` threads through `ForkChildRequest` into
    /// `fork_child`'s re-validation --
    /// [`conway_runtime::runtime::Runtime::narrow_plugin_config_for_fork`]
    /// narrows the parent's persisted config by the requested override
    /// against the currently installed plugins' rules, refusing any widening,
    /// and the re-validated value is PERSISTED onto the child's
    /// `SessionMeta::plugin_config` (so a subsequent `resume` re-derives the
    /// same narrowed value). Before this item, the request was silently
    /// dropped (`fork_child` always inherited `parent_meta.plugin_config`),
    /// so a child forked with a narrowed `conway.fs` root silently got the
    /// parent's -- a confinement boundary quietly failing to narrow.
    ///
    /// **Live registration:** after the store-side fork below, this
    /// method now also calls `Runtime::resume_root` over the freshly created
    /// child session -- the same mechanism [`Conway::resume`] uses -- so the
    /// returned handle is DRIVABLE: `prompt` on it succeeds (verified by
    /// `fork_from_returns_a_drivable_child_whose_prompt_succeeds`).
    /// `resume_root`'s `ResumeGate` means the child idles until
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
    /// **Inherited prefix, resolved (gap closed):** this criterion
    /// also asks for "the child's context contains the inherited prefix" --
    /// previously disclosed here as NOT satisfied, since `Runtime::
    /// resume_root` always constructed its `AgentLoop` with
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
    /// }` -- but `conway_testkit::FakeStore` (a `SessionStore` impl this
    /// crate depends on but does not own; out of this item's file scope to
    /// change) does not enforce that bound. Rather than let this method's
    /// behavior depend on which `SessionStore` backs a given `Conway`, the
    /// bound is checked here too, against the same error shape, so the
    /// criterion holds under every `SessionStore` implementation.
    ///
    /// **Shared helper (disclosed refactor):** the `store.fork` ->
    /// `rt.resume_root` sequence below used to live inline here. It now
    /// delegates to `crate::fork_child::fork_child` -- which only this
    /// method calls, since B2 moved the `/ask` fork-ask flow
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
            return Err(FacadeError::Store(StoreError::SeqOutOfRange {
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
                result_contract: spec.result_contract,
                keep_alive: spec.keep_alive,
                plugin_config: spec.plugin_config,
            },
        )
        .await
    }

    /// Promotes an ephemeral `/ask`-style agent to persistent (B3 — the
    /// `/ask` modal's `[f]` "keep" fate), atomically performing ALL THREE
    /// of: the durable session-header rewrite, the live-tree flag flip, and
    /// the `Event::AgentPromoted` emission that tells UIs to update. After
    /// B2, promotion is a flag flip ONLY — no re-parenting, no record
    /// rewriting beyond the header's `ephemeral` bit (the child's entire
    /// transcript, origin, and provenance are preserved verbatim).
    ///
    /// **Stage 2c (board item `01KZVZ0ASR4CRFG822YWEAW30K`):** the full
    /// mechanism — the failure ordering (header first, then tree, then
    /// event, so the three views can never split-brain), the live-tree
    /// resolution, and the exact error taxonomy — now lives on
    /// [`conway_runtime::runtime::Runtime::promote`], beside `AgentTree`;
    /// see that method's own doc for the complete reasoning. This method
    /// is a thin, unchanged-behavior delegation that flattens
    /// `RuntimeError::Store` back to this facade's own `FacadeError::
    /// Store` at the boundary (see `Self::resume`'s doc for why that
    /// unwrap is not a shortcut but a preserved contract).
    ///
    /// Promote is a lifecycle operation on an existing agent, NOT a new
    /// subagent primitive — no fork, no spawn, no new session.
    ///
    /// Errors: `FacadeError::Runtime(RuntimeError::AgentNotFound)` when
    /// `agent` is not in the live tree; `FacadeError::Store(
    /// StoreError::NotPromotable)` when the agent's session is not
    /// ephemeral (a double promote, or a non-`/ask` session); other
    /// `StoreError`s propagated unchanged.
    ///
    /// Returns the promoted agent's `SessionId` (unchanged by the promote
    /// — the flip touches no ids), so the caller can immediately e.g.
    /// focus or resume the now-persistent session.
    pub async fn promote(&self, agent: AgentId) -> Result<SessionId> {
        // Stage 2c (board item `01KZVZ0ASR4CRFG822YWEAW30K`): the full
        // sequence (durable header rewrite, then the live tree flip and
        // event) now lives on `Runtime::promote`, beside `AgentTree` -- see
        // that method's own doc. The unwrap below mirrors `Self::resume`'s
        // own "error-shape preservation" precedent: `RuntimeError::Store`
        // flattens back to this facade's own `FacadeError::Store` rather
        // than nesting one layer deeper, matching what this method
        // returned before the relocation and what
        // `crates/conway/tests/promote.rs` already asserts.
        self.rt.promote(agent).await.map_err(|err| match err {
            RuntimeError::Store(inner) => FacadeError::Store(inner),
            other => FacadeError::Runtime(other),
        })
    }

    /// Merges an ephemeral `/ask` child's turns into its parent's log,
    /// verbatim, then purges the child (B4 — the `/ask` modal's "pull in"
    /// fate, the semantic opposite of [`Conway::promote`]'s "keep": instead
    /// of the child becoming a session in its own right, its question and
    /// answer become part of the parent's own history and the child ceases
    /// to exist).
    ///
    /// **Stage 2c (board item `01KZVZ0ASR4CRFG822YWEAW30K`):** the full
    /// mechanism — the merge set, the sequencing, and the guard matrix —
    /// now lives on [`conway_runtime::runtime::Runtime::pull_in`], beside
    /// `AgentTree`; see that method's own doc for the complete reasoning.
    /// This method is a thin, unchanged-behavior delegation with the same
    /// `RuntimeError::Store` -> `FacadeError::Store` flattening
    /// [`Self::promote`]'s own doc explains.
    ///
    /// **That flattening is what tells a caller whether anything happened**
    /// (board item `01M0TNBACHQSAMMJ3TY14S47MX`). The merge is several
    /// appends and the store cannot roll them back, so a failure part-way
    /// leaves records behind. Every failure that mutated NOTHING — the
    /// whole guard matrix, plus a merge whose very first append failed —
    /// arrives here as [`FacadeError::Store`]; a failure that already
    /// mutated the parent's log arrives as [`FacadeError::Runtime`] wrapping
    /// `RuntimeError::PullInIncomplete`, which carries how many of how many
    /// records landed. Retrying after the latter would DUPLICATE whatever
    /// already merged, so the two must not be treated alike.
    ///
    /// Pull-in is a lifecycle operation on two existing agents' logs, NOT
    /// a new subagent primitive — no fork, no spawn, no new session is
    /// created here.
    pub async fn pull_in(&self, child: AgentId) -> Result<()> {
        self.rt.pull_in(child).await.map_err(|err| match err {
            RuntimeError::Store(inner) => FacadeError::Store(inner),
            other => FacadeError::Runtime(other),
        })
    }

    /// Purges an ephemeral `/ask` child outright, WITHOUT merging its turns
    /// anywhere (B5 — the `/ask` modal's `[esc]` "discard" fate, and the
    /// forced fate when the TUI quits with the modal open). The semantic
    /// opposite of [`Conway::pull_in`]: the user has explicitly chosen to
    /// throw the answer away, which is the single sanctioned exception to
    /// mandatory provenance retention (discard only ever happens
    /// via this explicit choice, never silently).
    ///
    /// **Stage 2c (board item `01KZVZ0ASR4CRFG822YWEAW30K`):** the full
    /// guard matrix now lives on
    /// [`conway_runtime::runtime::Runtime::purge`], beside `AgentTree`;
    /// see that method's own doc for the complete reasoning. This method
    /// is a thin, unchanged-behavior delegation with the same
    /// `RuntimeError::Store` -> `FacadeError::Store` flattening
    /// [`Self::promote`]'s own doc explains.
    ///
    /// Purge is a lifecycle operation on an existing agent's session,
    /// NOT a new subagent primitive.
    pub async fn purge(&self, agent: AgentId) -> Result<()> {
        self.rt.purge(agent).await.map_err(|err| match err {
            RuntimeError::Store(inner) => FacadeError::Store(inner),
            other => FacadeError::Runtime(other),
        })
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
    /// `ToolOutput`, and purging it would leave that artifact dangling so
    /// provenance survives it. Untagged (`None`) ephemeral sessions — every
    /// header written before the tag existed — are likewise never swept.
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
    /// `heartbeat` is within `live_threshold` of now, ANOTHER
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
    /// **`live_threshold` is caller-supplied, on purpose (Stage 2a).** This
    /// used to be a facade-owned constant, `60s = 4× the TUI's 15s
    /// heartbeat interval` — a presentation detail (a specific renderer's
    /// refresh cadence) hardcoded into engine configuration, which is
    /// exactly the defect the rest of Stage 2a moves `TuiSection` and its
    /// siblings out of this crate to fix. A caller with its own heartbeat
    /// cadence (or none at all) is the only party positioned to know what
    /// "fresh" means for its own marker; `conway-cli`'s TUI, the one
    /// production caller, derives its own value from its own 15s heartbeat
    /// interval rather than reading it back off this crate (see
    /// `crates/conway-cli/src/tui/mod.rs`).
    ///
    /// Best-effort per session: a session whose `remove` fails (e.g. it has
    /// since acquired children) is skipped and counting continues — the
    /// sweep is janitorial, and a leftover simply stays for the next
    /// startup's sweep. Returns the number of sessions purged.
    pub async fn sweep_stale_modal_asks(&self, live_threshold: ChronoDuration) -> Result<usize> {
        // S1 follow-up: if another process is actively using this store, defer
        // entirely. The caller publishes its OWN marker only AFTER the sweep,
        // so a marker read here is necessarily someone else's. A `live_owner`
        // error is treated as "no owner" (reap) — the sweep is best-effort and
        // must never block startup, and a store that cannot report liveness
        // behaves as a cold store (the same as one with no marker).
        if let Some(owner) = self.store.live_owner().await.unwrap_or(None) {
            if Utc::now().signed_duration_since(owner.heartbeat) <= live_threshold {
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
    /// passthrough fallback, the untrusted-reply validation policy, and every
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
        crate::intent::classify(&self.rt, &self.config, parent, default_recipe, text).await
    }
}

//! The Claude Code plugin directory compatibility tier's install mechanism
//! for the CLI binary (board item `01M0VR89FB1F3Q4FQ8852K2A5E`): every
//! `[plugins].claude_compat[]` entry in `settings.json` names a directory
//! already on the operator's own machine; this module reads it
//! (`conway_plugin_claude::discover`, no network access anywhere in that
//! call) and attaches every `.mcp.json` server declaration it translated,
//! through the exact same `conway_plugin_mcp::McpPlugin::discover` ->
//! `ConwayBuilder::with_plugin` path `mcp_plugins::install` already uses
//! for an operator-authored `[plugins].mcp[]` entry.
//!
//! **A fourth, sibling choke point** -- `first_party_plugins`'s closed
//! candidate set, `subprocess_plugins`'s conway-wire host, `mcp_plugins`'s
//! JSON-RPC client, and this module's own directory-read translation layer
//! all resolve independently from the same `ConwayBuilder`, in
//! `main.rs::build_conway`.
//!
//! **Board item `01M0XBZNBPXEESX8VNTJDKNG0J`: the hook half was wired here
//! too.** Until that item, only the MCP half of what a Claude Code
//! plugin directory can declare was ever appended into the `ConwayBuilder`
//! this module hands `build()` -- `conway_plugin_claude::
//! ClaudeCompatReport::hook_registrations()` (board item
//! `01M0X1FCQ80C9ET97HENXSAW2K`) already produced real, dispatchable
//! `[hooks].rules[]`-shaped registrations, but nothing appended them into a
//! `HooksConfig` before `build()` read one: an operator naming a directory
//! in `[plugins].claude_compat[]` got its MCP servers running and its
//! hooks reported, never dispatching -- the built-but-unreachable defect
//! `DESIGN-plugin-dependencies.md` §1 names as this tree's recurring
//! disease. That item's own fix, though, forged the wiring rather than
//! using a real seam: no dedicated builder-level injection existed for a
//! discovered `[hooks].rules[]`-shaped entry the way `with_plugin` already
//! existed for an MCP server, so it added `ConwayBuilder::config_mut` -- a
//! caller-side write into the WHOLE config -- and spliced the translated
//! rules into `hooks.rules` through it, indistinguishable from an entry the
//! operator had typed into `settings.json` themselves.
//!
//! **Board item `01M129QW0GV90QTQS6B3BY3DAR` closes that gap for real.**
//! `conway_core::ports::Plugin::hooks()` is the seam that should have
//! existed instead: a plugin declares its hooks the SAME way it declares
//! its tools, on the SAME `with_plugin` surface an MCP server already uses
//! (immediately below, in the SAME loop) -- no second, config-shaped
//! channel. `install`'s loop below now does both halves through ONE
//! mechanism: attach every `.mcp.json` server via `McpPlugin::discover` ->
//! `with_plugin` (unchanged), then wrap every mapped `hooks/hooks.json`
//! rule's own `HookRegistration` as a `ClaudeCompatHooksPlugin` and
//! attach THAT via the identical `with_plugin` call (`hooks_plugin`).
//! `ConwayBuilder::config_mut` is REMOVED (its one caller was this branch);
//! see `ConwayBuilder::build`'s own doc for how a plugin's `hooks()`
//! reaches `PermissionBroker::decide` at the same tier a config-declared
//! rule always has, and for the namespacing that now makes a
//! plugin-registered hook's id distinguishable from an operator-authored
//! one (this item's own provenance decision). `conway_plugin_claude::
//! ClaudeCompatReport::unsupported` is still read separately, by
//! `tui::app::startup` (for the `/plugin` listing's own honesty requirement
//! -- acceptance 5) -- this module's job stays "make a translated
//! declaration real," not "report on everything found."
//!
//! **Guard rail, deliberate: a translated hook's `on_failure` is left at
//! `conway_core::hook::HookOnFailure`'s own default, `Deny`, never set
//! explicitly by this module.** `conway_plugin_claude::HookRegistration`
//! carries no `on_failure` field of its own (that policy is
//! `PluginHookRule`/`conway::config::schema::HookEntry`-only, and
//! `conway_plugin_claude` never depends on `conway` -- see that crate's own
//! module doc), so `to_plugin_hook_rule` constructs every attached
//! [`PluginHookRule`] via `Default::default()` for exactly that one field.
//! This is the SAME
//! posture every existing `[hooks].rules[]` entry with no explicit
//! `on_failure` already has (board item `01M0X1AH44SNMK5TZ507K30QNP`): this
//! layer must not silently pick a foreign plugin's own failure posture on
//! the operator's behalf, and fail-closed is the one choice that never
//! WIDENS what an outage does. See `install`'s own test,
//! `a_translated_pre_tool_use_hook_carries_on_failure_deny`, which pins it
//! directly against a real translated registration rather than only
//! asserting it in prose.
//!
//! **Guard rail, deliberate: deny-capable hooks are called out, by name, on
//! stderr -- distinct from observation-only ones, and unconditionally.** A
//! translated `pre_tool_use` OR `prompt_submitted` rule is a real
//! permission consequence of naming a directory in `settings.json`: the
//! former can deny a real tool call, the latter can deny every prompt the
//! operator types (`conway_runtime::hook_dispatch::PROMPT_SUBMITTED`,
//! dispatched via `HookDispatcher::dispatch_deny_only`) -- the identical
//! authority an operator-authored `[hooks].rules[]` entry already has.
//! `install` reports that distinction itself, via `conway_cli::diag::warn`
//! (unconditional stderr, "reserve this for something an operator would
//! act on" -- that function's own doc) for every registration whose event
//! is in [`conway::DENY_CAPABLE_EVENTS`], and `diag::info` (verbose-only,
//! routine progress) for every other, observation-only one -- never one
//! undifferentiated "hooks registered" line, and never a second,
//! independently-drifting classification of which events those are (see
//! `report_hook_registrations`'s own doc: board item
//! `01M0XRD8VMWD273W0W51T8ECCM` fixed exactly that drift once already).
//! Both calls happen inside `build_conway`, before the TUI ever puts the
//! terminal into raw/alternate-screen mode (`main.rs`'s own comment on why
//! a stray stderr write after that point lands on top of the drawn UI), so
//! this reaches the operator's real scrollback on every dispatch target,
//! TUI included.
//!
//! **The payload-shape caveat this module does not, and must not, weaken.**
//! `conway_plugin_claude::hooks`'s own module doc states it in full:
//! "dispatches" is not the same claim as "behaves identically to running
//! under real Claude Code" -- a translated hook script still reads
//! `tool_name`/`tool_input` on stdin, while conway's dispatcher sends its
//! own `HookInvocation`/`HookEvent` shape. Wiring dispatch (this item) makes
//! the registration REAL; it does not, and cannot, repair that mismatch --
//! `docs/plugins/claude-compat.md` states the same limitation for the
//! operator, and nothing here claims otherwise.
//!
//! **Trust, stated where the capability is defined**, the same disclosure
//! `subprocess_plugins`/`mcp_plugins` each carry: everything a
//! `[plugins].claude_compat[]` entry's directory declares runs, or is read,
//! with the operator's own privileges and no sandboxing --
//! `conway::config::schema::PluginsConfig::claude_compat`'s own doc has the
//! full disclosure.
//!
//! **Board item `01M0XRCAFD7DD7N64RNRM3P8W9`: the command half is reachable
//! now too -- through a SEPARATE seam from `install`, not through it.**
//! `install`, above, is `ConwayBuilder`-shaped: it attaches every MCP server
//! and appends every mapped hook rule into the ONE `ConwayBuilder` `main.rs`
//! carries through `build_conway`. `conway::plugin::Plugin::commands()` has
//! no equivalent consumer inside the facade at all -- `ConwayBuilder::build`
//! never reads it (grep `crates/conway/src/builder.rs` for `.commands()`:
//! nothing). The ONLY reader is `conway_cli::tui::commands::
//! CommandRegistry::build`, called from TWO places, and NEITHER one takes
//! its `&[Arc<dyn Plugin>]` from the built `Conway`/`Runtime` (which retains
//! no such accessor -- `first_party_plugins::installed_plugins`'s own doc
//! states the identical constraint for the first-party bundle): both
//! `tui::app::App::new` and `commands::plugin::run` take it as a plain
//! parameter that `first_party_plugins::installed_plugins` RE-DERIVES from
//! `conway.config()`, independently of whatever `ConwayBuilder::
//! with_plugin` calls happened at build time. A `[plugins].claude_compat[]`
//! entry's own translated commands were invisible to that re-derivation
//! entirely -- not merely unregistered by `install` (this item's own
//! starting defect), but unreachable by CONSTRUCTION even if `install` had
//! called `with_plugin` for them, since `installed_plugins` never looks at
//! `ConwayBuilder`'s attached plugins at all. [`command_plugins`], below, is
//! this crate's other half: called from `first_party_plugins::
//! installed_plugins` (not from `install`), it re-derives every
//! `[plugins].claude_compat[]` entry's own `ClaudeCompatReport::
//! command_registrations()` the SAME way `installed_plugins` already
//! re-derives the first-party bundle -- one more source feeding the SAME
//! list, read by the SAME two consumers, with no change to either of them.
//! Attaching a commands-only `Plugin` to the `ConwayBuilder` inside
//! `install` instead was considered and rejected: nothing in `build()` ever
//! reads `Plugin::commands()` (confirmed above), so it would add a
//! duplicate-manifest-id risk against every other installed plugin for zero
//! behavioral effect -- the real reader lives entirely on the CLI side of
//! `build()`, so this crate's fix lives there too.

use std::sync::Arc;

use conway::config::schema::ConwayConfig;
use conway::config::{ConfigWarning, WarningCode};
use conway::plugin::{Command, Plugin, PluginHookRule, PluginManifest, Tool};
use conway::{ConwayBuilder, FacadeError, DENY_CAPABLE_EVENTS};
use conway_plugin_claude::HookRegistration;
use conway_plugin_mcp::McpPlugin;

use crate::diag;

/// Converts one translated [`HookRegistration`] into a real
/// [`PluginHookRule`] -- field for field, per `HookRegistration`'s own doc
/// ("mirrors `HookEntry`'s five fields exactly, deliberately NOT that
/// literal type"). `on_failure` is left at `PluginHookRule::on_failure`'s
/// field type's own default (`HookOnFailure::Deny`) -- see this module's
/// own top doc for why that is deliberate, not an oversight: this is a
/// TRANSLATION layer for a foreign format that carries no such field at
/// all, unlike a plugin authoring its own hook directly (see
/// `PluginHookRule::on_failure`'s own doc for that distinction).
///
/// `id` is left BARE -- `Plugin::hooks`'s own doc: the host, not this
/// translation, namespaces it with the declaring plugin's manifest id
/// before it ever reaches dispatch. `HookRegistration::id` already carries
/// ITS OWN `claude_compat:<report_id>:<event>:<index>` namespacing (see
/// that field's own doc on `ClaudeCompatReport::hook_registrations`), so
/// the id that ultimately dispatches is namespaced twice over -- once by
/// this crate's own translation (to keep two rules from the SAME directory
/// from colliding), once by the generic `Plugin::hooks()` seam (to keep two
/// DIFFERENT plugins, or a plugin and an operator, from colliding) --
/// harmless, and each layer solves a different collision.
fn to_plugin_hook_rule(registration: HookRegistration) -> PluginHookRule {
    PluginHookRule {
        id: registration.id,
        event: registration.event.to_string(),
        match_tool: registration.match_tool,
        command: registration.command,
        timeout_ms: registration.timeout_ms,
        enabled: registration.enabled,
        on_failure: Default::default(),
        // Carried straight through -- `HookRegistration::spawn_only`'s own
        // doc: `true` for exactly the one translated event that needs it
        // (`SubagentStart` -> `child_spawned`), `false` for every other
        // mapped event, unchanged from before this field existed.
        spawn_only: registration.spawn_only,
    }
}

/// A `[plugins].claude_compat[]` entry's own translated `hooks/hooks.json`
/// rules, wrapped as a real `conway::plugin::Plugin` -- the seam
/// `Plugin::hooks()` (board item `01M129QW0GV90QTQS6B3BY3DAR`) gives
/// [`install`], below, instead of the removed `ConwayBuilder::config_mut`
/// escape hatch. Mirrors `ClaudeCompatCommandsPlugin`'s own shape exactly:
/// carries no tools and no host-capability requirements, its only job is
/// handing back the translations [`hooks_plugin`] already resolved.
struct ClaudeCompatHooksPlugin {
    /// [`conway_plugin_claude::ClaudeCompatReport::id`] -- the SAME
    /// manifest-derived identity `ClaudeCompatCommandsPlugin::id` uses for
    /// the command half, NOT the config entry's own
    /// `ClaudeCompatPluginEntry::id` (the two are allowed to differ). This
    /// is what `ConwayBuilder::build` namespaces every rule in
    /// [`Self::hooks`] with.
    id: String,
    hooks: Vec<PluginHookRule>,
}

impl Plugin for ClaudeCompatHooksPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            version: "0.0.0".to_string(),
            tools: Vec::new(),
            required_host_caps: Vec::new(),
            optional_host_caps: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn hooks(&self) -> Vec<PluginHookRule> {
        self.hooks.clone()
    }
}

/// Builds this entry's own translated `hooks/hooks.json` rules as a real,
/// installable [`ClaudeCompatHooksPlugin`] -- separated from [`install`]'s
/// own loop so it is directly testable, the same "wiring-only, translation
/// tested at its own crate" split [`command_plugins`] already establishes
/// for the command half.
fn hooks_plugin(id: String, registrations: Vec<HookRegistration>) -> Arc<dyn Plugin> {
    let hooks: Vec<PluginHookRule> = registrations.into_iter().map(to_plugin_hook_rule).collect();
    Arc::new(ClaudeCompatHooksPlugin { id, hooks }) as Arc<dyn Plugin>
}

/// Reports, on stderr, which of `registrations` -- all already known to
/// belong to `entry_id` -- can deny a real tool call or a submitted
/// prompt, and which are observation-only, per this module's own
/// "distinguish, don't just say 'hooks registered'" guard rail.
///
/// **Classifies against [`conway::DENY_CAPABLE_EVENTS`], not a
/// re-declared list of its own** (board item `01M0XRD8VMWD273W0W51T8ECCM`):
/// this module used to hardcode a single-event `DENY_CAPABLE_EVENT =
/// "pre_tool_use"` constant, silently missing `prompt_submitted` --
/// conway's OTHER deny-capable, fail-closed event
/// (`conway_runtime::hook_dispatch::PROMPT_SUBMITTED`, dispatched via
/// `HookDispatcher::dispatch_deny_only`). A translated `UserPromptSubmit`
/// rule went unreported, on the unconditional channel, as a result. See
/// [`conway::DENY_CAPABLE_EVENTS`]'s own doc for why that constant, not a
/// second copy of the pair, is what every consumer of "is this event
/// deny-capable" should read from now.
///
/// A true no-op when `registrations` is empty (neither call below ever
/// fires).
fn report_hook_registrations(entry_id: &str, registrations: &[HookRegistration]) {
    let (deny_capable, observation_only) = classify_hook_registrations(registrations);
    if !deny_capable.is_empty() {
        diag::warn(format!(
            "[plugins].claude_compat entry '{entry_id}' registered {} hook(s) that CAN DENY a \
             real tool call or a submitted prompt ({}): {}",
            deny_capable.len(),
            DENY_CAPABLE_EVENTS.join(", "),
            deny_capable.join(", ")
        ));
    }
    if !observation_only.is_empty() {
        diag::info(format!(
            "[plugins].claude_compat entry '{entry_id}' registered {} observation-only hook(s): \
             {}",
            observation_only.len(),
            observation_only.join(", ")
        ));
    }
}

/// The pure split [`report_hook_registrations`] reports on -- pulled out
/// so the classification itself (which bucket a translated event lands in,
/// and therefore which channel, unconditional `diag::warn` or
/// verbose-gated `diag::info`, it reaches) is checkable directly, without
/// capturing real stderr. Order-preserving within each bucket; every id in
/// `registrations` appears in exactly one of the two returned lists.
fn classify_hook_registrations(registrations: &[HookRegistration]) -> (Vec<&str>, Vec<&str>) {
    let deny_capable = registrations
        .iter()
        .filter(|r| DENY_CAPABLE_EVENTS.contains(&r.event))
        .map(|r| r.id.as_str())
        .collect();
    let observation_only = registrations
        .iter()
        .filter(|r| !DENY_CAPABLE_EVENTS.contains(&r.event))
        .map(|r| r.id.as_str())
        .collect();
    (deny_capable, observation_only)
}

/// Discovers and attaches every `[plugins].claude_compat[]` entry's own
/// `.mcp.json` server declarations, in list order, then per-server order
/// within a directory -- then attaches every mapped `hooks/hooks.json`
/// rule's own [`HookRegistration`], wrapped as a `ClaudeCompatHooksPlugin`
/// (`hooks_plugin`), into the SAME builder via [`ConwayBuilder::
/// with_plugin`] (this module's own top doc). A discovery failure reading
/// the DIRECTORY itself -- the directory missing, a malformed
/// `.claude-plugin/plugin.json`/`.mcp.json` (`conway_plugin_claude::
/// ClaudeCompatError`) -- fails the WHOLE call as [`FacadeError::Build`],
/// naming the offending entry's own `id`, mirroring `subprocess_plugins::
/// install`/`mcp_plugins::install`'s own "an unresolvable entry fails the
/// whole build" posture for the same reason: an operator who named a
/// directory in `settings.json` and got nothing for it, silently, is
/// exactly the rung-1 lie CONTRIBUTING's declaration rule exists to
/// prevent. Attaching translated hook rules never itself fails this call --
/// `HookRegistration` construction is infallible (`conway_plugin_claude::
/// hooks::HookTranslation::registration`'s own doc); any defect in the
/// RESULT (an empty bare id, a namespaced id collision, an invalid `match`)
/// surfaces later, at `ConwayBuilder::build`'s own re-validation, exactly
/// like an operator-authored `[hooks].rules[]` entry with the same defect
/// would.
///
/// **A translated MCP server itself failing discovery is DIFFERENT --
/// board item `01M1AMSDE035HAG23TE6XPEF9R`, the blast-radius fix.** Before
/// this item, an `McpPlugin::discover` failure for even ONE server (spawn
/// failure, a missing runtime, a first-launch build that never finishes,
/// an upstream bug -- see `docs/plugins/claude-compat.md`'s own "reading a
/// directory you already have" for the operator report that surfaced this)
/// failed this WHOLE call exactly like a directory-read failure did, which
/// meant conway refused to start at ALL over a single third-party plugin's
/// own subprocess dying -- and `/plugin`, the one place that entry could be
/// removed from, was unreachable precisely because the process that would
/// show it never started. **Ruling: degrade and announce, never fail
/// closed, for this ONE failure mode.** An MCP server contributes tools
/// ONLY (`conway_plugin_mcp::McpPlugin`'s own `Plugin` impl carries no
/// `hooks`/`permission_evaluator` override -- confirmed by reading that
/// crate, not assumed), so a server that never came up narrows what the
/// model can call; it does not silently drop or misapply a permission
/// rule, which is the one thing P-13 ("deny and prompt rules fail closed,
/// never silently open") actually forbids. **P-13 does NOT apply here for
/// exactly that reason** -- it protects a rule from matching the wrong
/// targets or silently vanishing, and a tool-only server that never loads
/// contributes zero rules either way, matching or not. Contrast the SAME
/// directory's `hooks/hooks.json` rules, discovered independently by
/// `conway_plugin_claude::discover` above (a pure, local file read that
/// never spawns anything -- see that crate's own doc): those keep the
/// existing hard-fail posture (via the `discover` call above, unchanged),
/// because a mapped hook IS a permission-relevant rule and P-13 DOES apply
/// there. A dead MCP server in one entry never blocks that SAME entry's
/// hooks or commands from attaching, nor any OTHER entry's anything --
/// each server's own failure is caught and reported individually, in list
/// order, exactly like `report_hook_registrations`'s own per-entry
/// disclosure below.
///
/// **Considered and rejected: fail closed with a reachable escape
/// (`--without-plugin <id>`/`--safe-mode`).** Weaker than degrading:
/// it only helps an operator who already knows the flag exists, which is
/// not the operator who hit this (the ideate report: first plugin
/// install, first restart, no flag in mind at all -- the operator
/// recovered only by hand-editing `settings.json`'s `plugins` section,
/// having to know the file existed, its schema, and which key to cut).
/// Kept in mind, not implemented alongside: nothing here removes the
/// operator's ability to fix `settings.json` directly, so a future
/// `--safe-mode` remains addable without reopening this ruling.
///
/// **Considered and rejected: keeping the hard failure but naming
/// `~/.conway/settings.json`/`[plugins].claude_compat` in the error.**
/// Materially better than the ORIGINAL message (which named neither), but
/// still requires reaching a shell to edit a file conway itself already
/// has the config, the entry id, and a working command (`/plugin
/// uninstall`) to fix from INSIDE the running session -- degrading is a
/// strictly better recovery than a better-worded dead end.
///
/// **Every degraded server is reported on TWO channels**, the identical
/// "a host with no reason to read `Conway::warnings()` still sees it"
/// shape `WarningCode::OptionalPluginDependencyMissing`/
/// `OptionalHostCapabilityMissing` already establish one layer up in
/// `conway::builder`: `diag::warn` (unconditional stderr, reaching every
/// dispatch target before the TUI ever enters raw/alternate-screen mode --
/// this module's own top doc), and a [`ConfigWarning`] pushed via
/// [`ConwayBuilder::with_warning`], which reaches `Conway::warnings()` --
/// already rendered, generically, by BOTH `main.rs`'s own non-interactive
/// loop AND `tui::app::App::new`'s transcript (existing wiring, unmodified
/// by this item: any `ConfigWarning` landing on that accessor was already
/// surfaced by both before this `WarningCode` variant existed). The
/// message itself names the entry, the specific server, the underlying
/// `McpPluginError`, and the one live recovery: `/plugin uninstall
/// <entry-id>` -- reachable ONLY because this entry's degrade let the
/// session start at all.
///
/// **What this item does NOT change, disclosed rather than silently
/// widened:** `[plugins].mcp[]` (`mcp_plugins::install`) and
/// `[plugins].install` (`first_party_plugins::install`) keep their
/// existing hard-fail posture, untouched by this function. An
/// operator-authored `[plugins].mcp[]` entry is the identical wire
/// protocol and the identical "tools only" contribution shape an argument
/// for the SAME ruling could be made for, but extending it there is a
/// SEPARATE, undone widening this item does not make on its own account --
/// flagged, not silently applied, so a reviewer does not assume symmetry
/// that is not actually shipped. `[plugins].install` is a CLOSED,
/// compiled-in candidate set this binary tests directly; a first-party
/// plugin failing to install is conway's own defect, not a third party's,
/// and degrading there would hide exactly the kind of regression CI should
/// catch loud.
pub async fn install(builder: ConwayBuilder) -> conway::Result<ConwayBuilder> {
    let entries = builder.config().plugins.claude_compat.clone();
    let mut builder = builder;
    for entry in entries {
        let report =
            conway_plugin_claude::discover(&entry.dir).map_err(|err| FacadeError::Build {
                message: format!("[plugins].claude_compat entry '{}': {err}", entry.id),
            })?;

        // Board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`: `ConwayBuilder::
        // with_extra_skill_dir`/`with_extra_agent_dir` already existed,
        // already documented as "the seam a Claude Code compat layer...
        // calls to hand a plugin's own directories to a real build," and
        // simply had no caller until now. A missing `skills/`/`agents/`
        // subdirectory is not an error -- `load_skill_defs_from_roots`/
        // `load_agent_defs_from_roots`'s own lenient path already treats an
        // unreadable non-primary root as "zero entries," so this is always
        // safe to add, whether or not the entry's own directory happens to
        // contain either subdirectory. `crates/conway/src/skills.rs`/
        // `agents.rs`'s own lenient loaders (this item's other sibling
        // change) are what make a REAL Claude Code `SKILL.md`/agent file
        // found there actually translate.
        builder = builder
            .with_extra_skill_dir(entry.dir.join("skills"))
            .with_extra_agent_dir(entry.dir.join("agents"));

        // Computed BEFORE the `mcp_servers` loop below moves that field out
        // of `report` -- `hook_registrations()` takes `&self`, which a
        // partially-moved `report` could no longer satisfy.
        let registrations = report.hook_registrations();
        for server in report.mcp_servers {
            let server_name = server.name.clone();
            let spec = server.into_spec(entry.timeout_ms);
            match McpPlugin::discover(spec).await {
                Ok(plugin) => {
                    builder = builder.with_plugin(Arc::new(plugin));
                }
                Err(err) => {
                    // Degrade and announce (this function's own top doc,
                    // "the blast-radius fix"): never fail the whole build
                    // over one dead MCP server -- report it, on both
                    // channels, and keep going with everything else this
                    // entry (and every other entry) declares.
                    let message = format!(
                        "[plugins].claude_compat entry '{}': mcp server '{server_name}' failed \
                         to start ({err}) -- starting WITHOUT this server's tools. Run \
                         `/plugin uninstall {}` to remove the entry, or fix/remove it in \
                         settings.json's [plugins].claude_compat list.",
                        entry.id, entry.id
                    );
                    diag::warn(&message);
                    builder = builder.with_warning(ConfigWarning {
                        code: WarningCode::McpServerFailed,
                        message,
                    });
                }
            }
        }

        if !registrations.is_empty() {
            report_hook_registrations(&entry.id, &registrations);
            builder = builder.with_plugin(hooks_plugin(report.id.clone(), registrations));
        }
    }
    Ok(builder)
}

/// A `[plugins].claude_compat[]` entry's own translated `commands/*.md`
/// files, wrapped as a real `conway::plugin::Plugin` -- the ONLY shape
/// that reaches `conway_cli::tui::commands::CommandRegistry::build`
/// (`Plugin::commands()` is the one method it reads; see this module's own
/// top doc, "the command half is reachable now too"). Carries no tools and
/// no host-capability requirements: its single job is handing back the
/// `Ready` translations [`command_plugins`] already resolved.
///
/// **Board item `01M1DG5TTF6NHW2RXJRZ8ZPE7K`: also carries every `Ready`
/// `skills/<name>/SKILL.md` translation.** A translated skill is
/// registered through the IDENTICAL bare-name + host-namespacing scheme a
/// `commands/*.md` file already uses (`conway_plugin_claude::skills`'s own
/// module doc: this crate reuses `commands::ClaudeCommand` itself) -- so
/// the two kinds fold into the SAME `commands` list here rather than a
/// second `Plugin`, and `CommandRegistry::build`'s own duplicate-name
/// check (unmodified) is what an operator sees, named, if a plugin's own
/// `commands/foo.md` and `skills/foo/SKILL.md` ever collide on the same
/// bare name.
struct ClaudeCompatCommandsPlugin {
    /// [`conway_plugin_claude::ClaudeCompatReport::id`] -- the
    /// manifest-derived identity, NOT the config entry's own
    /// `ClaudeCompatPluginEntry::id` (the two are allowed to differ; see
    /// `ClaudeCompatReport::hook_registrations`'s own doc for the identical
    /// choice made for hook-id namespacing). `CommandRegistry::build`
    /// prefixes every bare command name here with THIS id.
    id: String,
    commands: Vec<Arc<dyn Command>>,
}

impl Plugin for ClaudeCompatCommandsPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: self.id.clone(),
            version: "0.0.0".to_string(),
            tools: Vec::new(),
            required_host_caps: Vec::new(),
            optional_host_caps: Vec::new(),
            requires: Vec::new(),
            optional: Vec::new(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        self.commands.clone()
    }
}

/// Re-derives every `[plugins].claude_compat[]` entry's own translated
/// `commands/*.md` files as ready-to-fold-in `Arc<dyn conway::plugin::
/// Plugin>`s -- the commands-only counterpart to [`install`]'s own MCP/hook
/// wiring, read from a config value rather than a live `ConwayBuilder`
/// because its ONE caller, `first_party_plugins::installed_plugins`, is
/// itself a read-only re-derivation from `conway.config()` (see that
/// function's own doc, and this module's own top doc for why the command
/// surface needs a SECOND seam rather than reusing [`install`]).
///
/// An entry with no `Ready` command translations contributes no `Plugin` at
/// all -- an installed `Plugin` with an empty `commands()` would be a
/// behavioral no-op either way, so this skips constructing one rather than
/// padding the registry with vacuous entries.
///
/// **Discovery failure here mirrors [`install`]'s own posture, not a softer
/// one.** [`install`] already proved every configured entry's directory
/// resolves, at `ConwayBuilder::build` time, before this ever runs -- a
/// failure here (the directory changed on disk since) is the same kind of
/// since-startup config drift `install` itself would refuse to paper over,
/// so this returns a named [`FacadeError::Build`] identically, rather than
/// silently dropping the entry's commands.
pub fn command_plugins(config: &ConwayConfig) -> conway::Result<Vec<Arc<dyn Plugin>>> {
    let mut plugins: Vec<Arc<dyn Plugin>> = Vec::new();
    for entry in &config.plugins.claude_compat {
        let report =
            conway_plugin_claude::discover(&entry.dir).map_err(|err| FacadeError::Build {
                message: format!("[plugins].claude_compat entry '{}': {err}", entry.id),
            })?;
        let mut commands = report.command_registrations();
        commands.extend(report.skill_registrations());
        if commands.is_empty() {
            continue;
        }
        plugins.push(Arc::new(ClaudeCompatCommandsPlugin {
            id: report.id.clone(),
            commands,
        }) as Arc<dyn Plugin>);
    }
    Ok(plugins)
}

#[cfg(test)]
mod tests {
    //! **Wiring-only, exactly like `subprocess_plugins`/`mcp_plugins`'s own
    //! disclosure.** `conway_plugin_claude`'s own translation logic is
    //! covered by its own crate's test suite; what is local and checkable
    //! HERE is only that an empty entry list is a true no-op, and that a
    //! directory naming an entry which fails to discover fails the whole
    //! build, naming the entry -- P-13, checked directly rather than only
    //! asserted in prose.
    use super::*;

    fn minimal_config() -> ConwayConfig {
        use std::collections::BTreeMap;

        use conway::config::schema::{
            AgentsConfig, HealthSection, HooksConfig, LimitsConfig, ModelsConfig,
            PermissionsConfig, PluginsConfig, RoleEntry, RoutingSection, SessionConfig,
            ToolsConfig,
        };
        use conway_core::ids::RoleAlias;

        let mut roles = BTreeMap::new();
        roles.insert(
            "default".to_string(),
            RoleEntry {
                chain: vec![],
                headroom_tokens: None,
                ..Default::default()
            },
        );
        ConwayConfig {
            default_role: RoleAlias::new("default"),
            cwd: std::path::PathBuf::from("."),
            session: SessionConfig::default(),
            limits: LimitsConfig::default(),
            permissions: PermissionsConfig::default(),
            backends: BTreeMap::new(),
            routing: RoutingSection::default(),
            roles,
            health: HealthSection::default(),
            agents: AgentsConfig::default(),
            models: ModelsConfig::default(),
            tools: ToolsConfig::default(),
            plugins: PluginsConfig::default(),
            hooks: HooksConfig::default(),
        }
    }

    #[tokio::test]
    async fn an_empty_claude_compat_list_is_a_true_no_op() {
        let builder = ConwayBuilder::from_parts(minimal_config());
        let result = install(builder).await;
        assert!(
            result.is_ok(),
            "an empty [plugins].claude_compat list must never fail"
        );
    }

    #[tokio::test]
    async fn a_nonexistent_directory_fails_the_whole_build_naming_the_entry() {
        use conway::config::schema::ClaudeCompatPluginEntry;

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "acme-tools".to_string(),
            dir: std::path::PathBuf::from("/does/not/exist/at/all"),
            timeout_ms: 5_000,
        });
        let builder = ConwayBuilder::from_parts(config);
        // `ConwayBuilder` does not implement `Debug`, so `expect_err`/
        // `unwrap_err` (both bound on `T: Debug`) are unavailable here --
        // matched explicitly instead, mirroring `conway/tests/builder.rs`'s
        // own `expect_build_err` helper for the identical reason.
        let err = match install(builder).await {
            Ok(_) => panic!("a nonexistent claude_compat directory must fail the whole build"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(
            message.contains("acme-tools"),
            "the failing entry's own id must be named: {message}"
        );
    }

    // ---- MCP server blast-radius fix (board item `01M1AMSDE035HAG23TE6XPEF9R`) ----

    /// Writes a fixture "MCP server" that dies before completing the
    /// `initialize` handshake -- the ideate report's exact shape ("session
    /// died: closed stdout (EOF) mid-session"), reproduced with a trivial
    /// script rather than the real ideate binary. Mirrors
    /// `conway-plugin-mcp/tests/common::write_script`'s own convention
    /// (plain Python 3, no interpreter prepended, `#!` shebang) -- that
    /// helper lives in a sibling crate's OWN `tests/common/mod.rs`, not
    /// reused across a crate boundary, so this is a fresh, minimal copy of
    /// the same shape rather than a new inter-crate test dependency.
    fn write_dying_mcp_server(dir: &std::path::Path, name: &str) {
        let path = dir.join(name);
        std::fs::write(&path, "#!/usr/bin/env python3\nimport sys\nsys.exit(1)\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// **The headline claim this item exists to prove.** Before this
    /// item's fix, `install`'s own MCP-discovery loop propagated
    /// `McpPlugin::discover`'s `Err` straight out via `?`, exactly like the
    /// directory-read failure `a_nonexistent_directory_fails_the_whole_
    /// build_naming_the_entry` (immediately above) still does -- so a
    /// single dead MCP server failed the WHOLE `install` call, which is
    /// what `build_conway` propagates straight to `main`'s own top-level
    /// `Err` branch, refusing to start conway at all. This test pins the
    /// fix: the SAME fixture that used to produce that `Err` now produces
    /// `Ok`, with the failure visible on `Conway::warnings()`'s own
    /// pre-`build()` mirror instead of aborting the call.
    #[tokio::test]
    async fn a_dead_mcp_server_degrades_instead_of_failing_the_whole_build() {
        use conway::config::schema::ClaudeCompatPluginEntry;

        let dir = tempfile::tempdir().expect("tempdir");
        write_dying_mcp_server(dir.path(), "dies.py");
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"ideate":{"command":"${CLAUDE_PLUGIN_ROOT}/dies.py"}}}"#,
        )
        .unwrap();

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "ideate".to_string(),
            dir: dir.path().to_path_buf(),
            timeout_ms: 5_000,
        });
        let builder = ConwayBuilder::from_parts(config);
        let builder = install(builder)
            .await
            .expect("a dead MCP server must degrade, never fail the whole build");

        assert!(
            builder.plugins().is_empty(),
            "the dead server's own plugin must never attach: {}",
            builder.plugins().len()
        );
        let warning = builder
            .warnings()
            .iter()
            .find(|w| w.code == WarningCode::McpServerFailed)
            .expect("the degraded server must be reported on Conway::warnings()");
        assert!(
            warning.message.contains("ideate"),
            "the failing entry's own id must be named: {}",
            warning.message
        );
        assert!(
            warning.message.contains("/plugin uninstall ideate"),
            "the ONE live recovery -- removing the entry from inside the session -- must be \
             named, not just the failure: {}",
            warning.message
        );
    }

    /// Two servers in the SAME `.mcp.json`, each failing for its own
    /// reason: BOTH must degrade independently, neither one's failure
    /// swallowing or masking the other's -- proves degrading one server
    /// never takes down its own sibling, the narrowest possible blast
    /// radius. (Not "one dead, one healthy": a genuine success path is
    /// already covered by this crate's OWN `.mcp.json` translation tests
    /// and by `conway-plugin-mcp`'s end-to-end suite; this test's whole
    /// point is isolation between two independent failures, not a happy
    /// path.)
    #[tokio::test]
    async fn two_independently_dead_mcp_servers_in_the_same_directory_both_degrade() {
        use conway::config::schema::ClaudeCompatPluginEntry;

        let dir = tempfile::tempdir().expect("tempdir");
        write_dying_mcp_server(dir.path(), "dies.py");
        // `true` -- a real, always-spawnable command -- is deliberately NOT
        // a healthy MCP server (it never speaks the handshake either), so
        // this asserts the SAME "reported, not attached" outcome for BOTH
        // servers rather than claiming a genuine success path: the point
        // of this test is that the two servers' OWN failures are isolated
        // from each other, not that one of them fully succeeds.
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{
                "dies":{"command":"${CLAUDE_PLUGIN_ROOT}/dies.py"},
                "also-fails":{"command":"true"}
            }}"#,
        )
        .unwrap();

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "acme-tools".to_string(),
            dir: dir.path().to_path_buf(),
            timeout_ms: 5_000,
        });
        let builder = ConwayBuilder::from_parts(config);
        let builder = install(builder)
            .await
            .expect("two independently-dead servers must still degrade, not fail the build");

        assert_eq!(
            builder
                .warnings()
                .iter()
                .filter(|w| w.code == conway::config::WarningCode::McpServerFailed)
                .count(),
            2,
            "both servers' own failures must be reported individually: {:?}",
            builder.warnings()
        );
    }

    // ---- hook-dispatch wiring (board item `01M0XBZNBPXEESX8VNTJDKNG0J`,
    //      re-wired onto `Plugin::hooks()` by board item
    //      `01M129QW0GV90QTQS6B3BY3DAR`) ----

    use conway::config::schema::ClaudeCompatPluginEntry;
    use conway::plugin::HookOnFailure;

    /// Writes `<dir>/hooks/hooks.json` with the given raw JSON contents --
    /// the identical fixture shape `conway_plugin_claude::hooks`'s own tests
    /// use, inlined here rather than shared across crates.
    fn write_hooks_json(dir: &std::path::Path, contents: &str) {
        std::fs::create_dir_all(dir.join("hooks")).unwrap();
        std::fs::write(dir.join("hooks").join("hooks.json"), contents).unwrap();
    }

    fn config_with_claude_compat_entry(dir: &std::path::Path) -> ConwayConfig {
        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "acme-tools".to_string(),
            dir: dir.to_path_buf(),
            timeout_ms: 5_000,
        });
        config
    }

    /// Every [`PluginHookRule`] every plugin `install` attached returns,
    /// flattened -- the post-`install`, pre-`build()` inspection point this
    /// module's own tests now use instead of the removed
    /// `builder.config().hooks.rules` (that field no longer receives a
    /// plugin-registered hook at all -- see [`ConwayBuilder::plugins`]'s own
    /// doc). `ConwayBuilder::build`'s own namespacing (host-prefixing each
    /// `id` with its declaring plugin's manifest id) has NOT run yet at this
    /// point -- these are the BARE ids `install` handed to `with_plugin`,
    /// exactly the ones asserted below.
    fn hook_rules(builder: &ConwayBuilder) -> Vec<PluginHookRule> {
        builder.plugins().iter().flat_map(|p| p.hooks()).collect()
    }

    /// **The headline claim this item exists to prove**: a `PreToolUse`
    /// rule in a directory's own `hooks/hooks.json` is not merely reported
    /// -- it is attached, real and dispatchable, as a
    /// [`ClaudeCompatHooksPlugin`] on the SAME builder `install` hands back,
    /// ready for `ConwayBuilder::build` to fold into its own dispatch lists.
    /// `crates/conway-cli/tests/hook_runner_wiring.rs` is the sibling
    /// end-to-end proof that an appended `pre_tool_use` rule actually denies
    /// a real tool call through the compiled binary; this test pins the
    /// wiring step that makes that reachable at all.
    #[tokio::test]
    async fn a_mapped_pre_tool_use_hook_is_appended_as_a_dispatchable_rule() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo pre"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = hook_rules(&builder);
        assert_eq!(rules.len(), 1, "exactly one mapped rule: {rules:?}");
        let rule = &rules[0];
        assert_eq!(rule.event, "pre_tool_use");
        assert_eq!(rule.match_tool.as_deref(), Some("Bash"));
        assert_eq!(rule.command[0], "/bin/sh");
        assert_eq!(rule.command[1], "-c");
        assert!(rule.command[2].contains("echo pre"));
        assert!(rule.enabled);
        assert!(
            rule.id.contains("claude_compat:"),
            "a translated rule's OWN id must still carry its own \
             conway_plugin_claude-assigned namespacing (ConwayBuilder::build applies a SECOND, \
             host-level namespace on top of this one): {}",
            rule.id
        );
    }

    /// **Guard rail, pinned directly**: a translated hook never sets
    /// `on_failure` itself -- it is left at [`PluginHookRule::on_failure`]'s
    /// field type's own default, `HookOnFailure::Deny`, the same
    /// fail-closed posture every existing `[hooks].rules[]` entry with no
    /// explicit `on_failure` already has. This module must never silently
    /// choose a foreign plugin's own failure posture on the operator's
    /// behalf.
    #[tokio::test]
    async fn a_translated_pre_tool_use_hook_carries_on_failure_deny() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo pre"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = hook_rules(&builder);
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].on_failure,
            HookOnFailure::Deny,
            "a translated rule must default to Deny, never widen an operator-unreviewed outage \
             posture"
        );
    }

    /// A mapped, but non-`pre_tool_use`, event (`SessionStart` ->
    /// `session_starting`) is appended exactly like a `pre_tool_use` one --
    /// dispatch wiring does not discriminate by event, only the operator-
    /// visible reporting (`report_hook_registrations`) does.
    #[tokio::test]
    async fn a_mapped_session_starting_hook_is_also_appended() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"echo start"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = hook_rules(&builder);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].event, "session_starting");
    }

    /// Board item `01M129Y98V4C1050QBPPMY37X0`: `to_plugin_hook_rule` carries
    /// `HookRegistration::spawn_only` through to the real, dispatchable
    /// `PluginHookRule` unchanged -- `true` for a translated `SubagentStart`
    /// rule, `false` for a translated `SubagentStop` one (checked in the
    /// SAME fixture, not two separate tests, so this cannot pass against a
    /// version of `to_plugin_hook_rule` that hardcodes one value for every
    /// rule regardless of what it translated). The dispatch-level proof that
    /// a `spawn_only: true` rule actually narrows to a real `Spawn` (not
    /// merely that the bit survives this wiring step) is
    /// `crates/conway-plugin-claude/tests/hooks_dispatch.rs`'s own
    /// `a_translated_subagent_start_hooks_json_rule_fires_for_a_spawn_but_not_for_a_fork`
    /// -- this test is wiring-only, mirroring this module's own stated
    /// scope (see this module's `tests` doc comment).
    #[tokio::test]
    async fn a_translated_subagent_start_hook_carries_spawn_only_and_subagent_stop_does_not() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{
                "SubagentStart":[{"hooks":[{"type":"command","command":"echo start"}]}],
                "SubagentStop":[{"hooks":[{"type":"command","command":"echo stop"}]}]
            }}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = hook_rules(&builder);
        assert_eq!(
            rules.len(),
            2,
            "both mapped rules must be appended: {rules:?}"
        );

        let start_rule = rules
            .iter()
            .find(|r| r.event == "child_spawned")
            .expect("SubagentStart maps to child_spawned");
        assert!(
            start_rule.spawn_only,
            "a translated SubagentStart rule must narrow to Spawn-mode children only"
        );

        let stop_rule = rules
            .iter()
            .find(|r| r.event == "child_reported")
            .expect("SubagentStop maps to child_reported");
        assert!(
            !stop_rule.spawn_only,
            "child_reported has no \"mode\" field -- spawn_only must stay false"
        );
    }

    /// An `Unmapped` rule (no conway counterpart -- `Stop` here) contributes
    /// no [`PluginHookRule`] at all, and therefore no attached plugin --
    /// `hook_registrations()` already filters these out (they are named in
    /// `ClaudeCompatReport::unsupported` instead, read by a different
    /// module, per this file's own top doc).
    #[tokio::test]
    async fn an_unmapped_hook_appends_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        assert!(
            builder.plugins().is_empty(),
            "an unmapped event must attach nothing: {}",
            builder.plugins().len()
        );
    }

    /// A directory declaring both a deny-capable (`PreToolUse`) and an
    /// observation-only (`SessionStart`) rule appends BOTH -- proving
    /// `report_hook_registrations`'s deny/observation split (stderr-only) is
    /// purely a reporting distinction, never a filter on what actually gets
    /// wired.
    #[tokio::test]
    async fn a_directory_with_both_deny_capable_and_observation_only_hooks_appends_both() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{
                "PreToolUse":[{"hooks":[{"type":"command","command":"echo pre"}]}],
                "SessionStart":[{"hooks":[{"type":"command","command":"echo start"}]}]
            }}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = hook_rules(&builder);
        assert_eq!(
            rules.len(),
            2,
            "both mapped rules must be appended: {rules:?}"
        );
        assert!(rules.iter().any(|r| r.event == "pre_tool_use"));
        assert!(rules.iter().any(|r| r.event == "session_starting"));
    }

    /// Two `[plugins].claude_compat[]` entries, each declaring its own
    /// mapped hook, each attach their OWN [`ClaudeCompatHooksPlugin`] --
    /// `install`'s loop accumulates across entries (two separate
    /// `with_plugin` calls) rather than each entry silently overwriting the
    /// last.
    #[tokio::test]
    async fn two_claude_compat_entries_accumulate_into_the_same_hooks_config() {
        // Each directory gets its OWN `.claude-plugin/plugin.json` `name` --
        // `HookRegistration::id` is namespaced by that manifest-derived
        // `ClaudeCompatReport::id`, not by the config entry's own `id`
        // (`ClaudeCompatPluginEntry::id`'s own doc: the two are allowed to
        // differ). Naming both here makes the id-namespacing assertion
        // below check something real rather than a random tempdir name.
        let dir_a = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir_a.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir_a.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"entry-a"}"#,
        )
        .unwrap();
        write_hooks_json(
            dir_a.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo a"}]}]}}"#,
        );
        let dir_b = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir_b.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir_b.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"entry-b"}"#,
        )
        .unwrap();
        write_hooks_json(
            dir_b.path(),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"echo b"}]}]}}"#,
        );

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "entry-a".to_string(),
            dir: dir_a.path().to_path_buf(),
            timeout_ms: 5_000,
        });
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "entry-b".to_string(),
            dir: dir_b.path().to_path_buf(),
            timeout_ms: 5_000,
        });

        let builder = ConwayBuilder::from_parts(config);
        let builder = install(builder).await.expect("install must succeed");

        assert_eq!(
            builder.plugins().len(),
            2,
            "one attached hooks-plugin from each entry"
        );
        let manifest_ids: Vec<String> = builder.plugins().iter().map(|p| p.manifest().id).collect();
        assert!(manifest_ids.contains(&"entry-a".to_string()));
        assert!(manifest_ids.contains(&"entry-b".to_string()));

        let rules = hook_rules(&builder);
        assert_eq!(rules.len(), 2, "one rule from each entry: {rules:?}");
        // Namespaced by each entry's own report id, so the two never
        // collide even though both name the identical Claude Code event.
        assert!(rules.iter().any(|r| r.id.contains("entry-a")));
        assert!(rules.iter().any(|r| r.id.contains("entry-b")));
    }

    // ---- command wiring (board item `01M0XRCAFD7DD7N64RNRM3P8W9`) ----

    fn write_command_md(dir: &std::path::Path, file_name: &str, contents: &str) {
        std::fs::create_dir_all(dir.join("commands")).unwrap();
        std::fs::write(dir.join("commands").join(file_name), contents).unwrap();
    }

    fn config_with_one_claude_compat_entry(dir: &std::path::Path, entry_id: &str) -> ConwayConfig {
        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: entry_id.to_string(),
            dir: dir.to_path_buf(),
            timeout_ms: 5_000,
        });
        config
    }

    /// **The headline claim this item exists to prove, at the wiring
    /// level**: a `Ready` `commands/*.md` translation produces a real
    /// `conway::plugin::Plugin` -- namespaced by the report's own manifest
    /// id, exactly like [`ClaudeCompatCommandsPlugin::manifest`] documents
    /// -- and invoking the ONE command it carries submits the file's own
    /// body, verbatim. `crates/conway-cli/tests/claude_compat_commands.rs`
    /// is the sibling end-to-end proof that this reaches the compiled
    /// binary's real command dispatch; this test pins the wiring step that
    /// makes that reachable at all.
    #[tokio::test]
    async fn a_ready_command_translation_becomes_a_real_invokable_plugin() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"acme-tools"}"#,
        )
        .unwrap();
        write_command_md(
            dir.path(),
            "greet.md",
            "---\ndescription: Greets the operator\n---\n\nSay a friendly hello.\n",
        );

        let config = config_with_one_claude_compat_entry(dir.path(), "acme-tools");
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        assert_eq!(plugins.len(), 1, "exactly one plugin: {:?}", plugins.len());

        let manifest = plugins[0].manifest();
        assert_eq!(
            manifest.id, "acme-tools",
            "namespaced by the report's own manifest id, not the config entry's id"
        );
        assert!(plugins[0].tools().is_empty());

        let commands = plugins[0].commands();
        assert_eq!(commands.len(), 1);
        let spec = commands[0].spec();
        assert_eq!(spec.name, "greet", "the command name must stay bare");
        assert_eq!(spec.summary, "Greets the operator");

        let ctx = conway::plugin::CommandCtx {
            focused_agent: conway_core::ids::AgentId::new(),
            root_agent: conway_core::ids::AgentId::new(),
            session_id: conway_core::ids::SessionId::new(),
            args: String::new(),
        };
        let outcome = commands[0].invoke(ctx).await;
        assert_eq!(
            outcome,
            conway::plugin::CommandOutcome::SubmitPrompt {
                text: "Say a friendly hello.".to_string()
            }
        );
    }

    /// The config entry's own `id` and the report's manifest-derived `id`
    /// are allowed to differ (`ClaudeCompatPluginEntry::id`'s own doc) --
    /// `command_plugins` namespaces by the LATTER, mirroring
    /// `hook_registrations`'s identical choice, checked directly rather
    /// than only asserted alongside the happy-path test above.
    #[tokio::test]
    async fn a_command_plugin_is_namespaced_by_the_reports_id_not_the_config_entrys_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"acme-tools"}"#,
        )
        .unwrap();
        write_command_md(dir.path(), "greet.md", "Say hello.\n");

        // The config entry's own id deliberately differs from the
        // manifest's `name` above.
        let config = config_with_one_claude_compat_entry(dir.path(), "config-entry-id");
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].manifest().id, "acme-tools");
    }

    /// An entry with no `Ready` commands (an empty body, refused) must not
    /// contribute a vacuous `Plugin` -- an installed plugin with an empty
    /// `commands()` would register nothing anyway, so `command_plugins`
    /// skips constructing one rather than padding the returned list.
    #[tokio::test]
    async fn an_entry_with_no_ready_commands_contributes_no_plugin() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_command_md(dir.path(), "blank.md", "");

        let config = config_with_one_claude_compat_entry(dir.path(), "acme-tools");
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        assert!(
            plugins.is_empty(),
            "an entry with no Ready commands must contribute nothing: {}",
            plugins.len()
        );
    }

    /// An entry declaring no `commands/` directory at all is the same true
    /// no-op -- mirrors `install`'s own "an empty entry list is a true
    /// no-op" posture, one level down (a real entry with nothing to
    /// translate, rather than no entries at all).
    #[tokio::test]
    async fn an_entry_with_no_commands_directory_contributes_no_plugin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = config_with_one_claude_compat_entry(dir.path(), "acme-tools");
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        assert!(plugins.is_empty());
    }

    /// An empty `[plugins].claude_compat[]` list is a true no-op -- the
    /// identical property `install`'s own
    /// `an_empty_claude_compat_list_is_a_true_no_op` pins for the MCP/hook
    /// half.
    #[tokio::test]
    async fn an_empty_claude_compat_list_contributes_no_command_plugins() {
        let config = minimal_config();
        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        assert!(plugins.is_empty());
    }

    /// A directory that does not exist fails the whole call, naming the
    /// offending entry -- the identical P-13 posture `install`'s own
    /// `a_nonexistent_directory_fails_the_whole_build_naming_the_entry`
    /// pins for the MCP/hook half, checked here for the command half.
    #[tokio::test]
    async fn a_nonexistent_directory_fails_command_plugins_naming_the_entry() {
        let config = config_with_one_claude_compat_entry(
            std::path::Path::new("/does/not/exist/at/all"),
            "acme-tools",
        );
        let err = match command_plugins(&config) {
            Ok(_) => panic!("a nonexistent claude_compat directory must fail command_plugins"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("acme-tools"),
            "the failing entry's own id must be named: {err}"
        );
    }

    /// Two entries, each contributing a command, produce two separately
    /// namespaced plugins -- `command_plugins` accumulates across entries
    /// exactly like `install`'s own hook loop does for `HooksConfig`.
    #[tokio::test]
    async fn two_claude_compat_entries_each_contribute_their_own_command_plugin() {
        let dir_a = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir_a.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir_a.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"entry-a"}"#,
        )
        .unwrap();
        write_command_md(dir_a.path(), "greet.md", "Hello from a.\n");

        let dir_b = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir_b.path().join(".claude-plugin")).unwrap();
        std::fs::write(
            dir_b.path().join(".claude-plugin").join("plugin.json"),
            r#"{"name":"entry-b"}"#,
        )
        .unwrap();
        write_command_md(dir_b.path(), "greet.md", "Hello from b.\n");

        let mut config = minimal_config();
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "entry-a".to_string(),
            dir: dir_a.path().to_path_buf(),
            timeout_ms: 5_000,
        });
        config.plugins.claude_compat.push(ClaudeCompatPluginEntry {
            id: "entry-b".to_string(),
            dir: dir_b.path().to_path_buf(),
            timeout_ms: 5_000,
        });

        let plugins = command_plugins(&config).expect("command_plugins must succeed");
        assert_eq!(plugins.len(), 2);
        let ids: Vec<String> = plugins.iter().map(|p| p.manifest().id).collect();
        assert!(ids.contains(&"entry-a".to_string()));
        assert!(ids.contains(&"entry-b".to_string()));
    }
    // ---- deny-capable classification (board item `01M0XRD8VMWD273W0W51T8ECCM`) ----

    /// **The regression this item exists to close.** Before this item,
    /// `report_hook_registrations` classified only `pre_tool_use` as
    /// deny-capable -- a translated `UserPromptSubmit` rule (mapped to
    /// `prompt_submitted`, `conway_plugin_claude::hooks`'s own `EVENT_MAP`)
    /// landed in the OBSERVATION-only bucket, which only ever reaches
    /// `diag::info` (suppressed at default verbosity, `crate::diag::info`'s
    /// own doc) -- even though `prompt_submitted` can deny every prompt the
    /// operator types (`conway_runtime::hook_dispatch::PROMPT_SUBMITTED`,
    /// dispatched via `HookDispatcher::dispatch_deny_only`,
    /// `runtime.rs:984`). `classify_hook_registrations` is the exact split
    /// `report_hook_registrations` feeds into `diag::warn` (unconditional)
    /// vs `diag::info` (gated) -- landing here, in the FIRST list, is what
    /// "reaches the unconditional channel" means for this function; there is
    /// no stderr to capture beyond that split, `diag::warn`/`diag::info`'s
    /// own gating is exercised by `diag`'s own tests, not re-tested here.
    #[test]
    fn a_translated_user_prompt_submit_registration_reaches_the_unconditional_channel() {
        let registrations = vec![HookRegistration {
            id: "claude_compat:acme-tools:prompt_submitted:0".to_string(),
            event: "prompt_submitted",
            match_tool: None,
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ],
            timeout_ms: 5_000,
            enabled: true,
            spawn_only: false,
        }];
        let (deny_capable, observation_only) = classify_hook_registrations(&registrations);
        assert_eq!(
            deny_capable,
            vec!["claude_compat:acme-tools:prompt_submitted:0"],
            "a translated prompt_submitted rule must be classified deny-capable, not \
             observation-only"
        );
        assert!(
            observation_only.is_empty(),
            "must not also appear in the gated bucket: {observation_only:?}"
        );
    }

    /// Regression, the other direction: `pre_tool_use` must still classify
    /// deny-capable after this item widened the set from one event to two --
    /// `conway::DENY_CAPABLE_EVENTS` replacing the old single-event constant
    /// must not silently drop the event that constant already covered.
    #[test]
    fn a_translated_pre_tool_use_registration_still_reaches_the_unconditional_channel() {
        let registrations = vec![HookRegistration {
            id: "claude_compat:acme-tools:pre_tool_use:0".to_string(),
            event: "pre_tool_use",
            match_tool: Some("Bash".to_string()),
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ],
            timeout_ms: 5_000,
            enabled: true,
            spawn_only: false,
        }];
        let (deny_capable, observation_only) = classify_hook_registrations(&registrations);
        assert_eq!(
            deny_capable,
            vec!["claude_compat:acme-tools:pre_tool_use:0"]
        );
        assert!(observation_only.is_empty());
    }

    /// An observation-only event (`session_starting`) is classified into
    /// the gated bucket, never the unconditional one -- the distinction
    /// `report_hook_registrations`'s own doc promises, checked directly
    /// rather than only asserted in prose.
    #[test]
    fn a_translated_session_starting_registration_is_observation_only() {
        let registrations = vec![HookRegistration {
            id: "claude_compat:acme-tools:session_starting:0".to_string(),
            event: "session_starting",
            match_tool: None,
            command: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo hi".to_string(),
            ],
            timeout_ms: 5_000,
            enabled: true,
            spawn_only: false,
        }];
        let (deny_capable, observation_only) = classify_hook_registrations(&registrations);
        assert!(deny_capable.is_empty());
        assert_eq!(
            observation_only,
            vec!["claude_compat:acme-tools:session_starting:0"]
        );
    }

    /// **End-to-end wiring proof, mirroring
    /// `a_mapped_pre_tool_use_hook_is_appended_as_a_dispatchable_rule`
    /// exactly**: a `UserPromptSubmit` rule in a directory's own
    /// `hooks/hooks.json` is translated to a real, dispatchable
    /// `prompt_submitted` `[hooks].rules[]` entry -- the SAME event
    /// `HookDispatcher::dispatch_deny_only` (`runtime.rs:984`) consults for
    /// every submitted prompt. Before this item, this exact shape had zero
    /// coverage in this module (spec's own
    /// `grep -c "UserPromptSubmit\|prompt_submitted"` check).
    #[tokio::test]
    async fn a_mapped_user_prompt_submit_hook_is_appended_as_a_dispatchable_rule() {
        let dir = tempfile::tempdir().expect("tempdir");
        write_hooks_json(
            dir.path(),
            r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo prompt"}]}]}}"#,
        );
        let builder = ConwayBuilder::from_parts(config_with_claude_compat_entry(dir.path()));
        let builder = install(builder).await.expect("install must succeed");

        let rules = hook_rules(&builder);
        assert_eq!(rules.len(), 1, "exactly one mapped rule: {rules:?}");
        let rule = &rules[0];
        assert_eq!(rule.event, "prompt_submitted");
        assert!(rule.command[2].contains("echo prompt"));
        assert!(rule.enabled);
        assert_eq!(
            rule.on_failure,
            HookOnFailure::Deny,
            "a translated prompt_submitted rule must default to Deny too, the same fail-closed \
             posture every other translated rule gets"
        );
    }
}

//! `AppState`: the TUI's render model, derived purely from the same
//! `EventStream` the one-shot renderers consume.
//!
//! `apply` is the single mutation entry point -- fed one [`Envelope`] at a
//! time by the app loop -- so this module can be unit-tested with no
//! terminal at all: construct an `AppState`, feed it a sequence of
//! `Envelope`s, and assert on the resulting `transcript`/`tree`.
//!
//! `AppState` itself -- its fields, [`AppState::new`],
//! [`AppState::focus_agent`], and the [`AppState::apply`] dispatcher -- is
//! the one thing every seam below shares, so it stays here. Everything
//! `apply` dispatches INTO lives in its own submodule, split along the
//! seams the struct's own fields already group into: the transcript
//! entry model (`transcript`, turn-summary formatting in
//! `turn_summary`, pane scrolling in `scroll`), the focused agent's
//! live activity (`status`), the agent-tree data model (`agent_tree`)
//! and the `/agents` panel built over it (`agent_panel`), the
//! modal-bearing surfaces (`modal`), and the input line's own composer
//! state (`input_line`). Each submodule's own methods are additional
//! `impl AppState` blocks -- ordinary Rust, not a language feature -- so
//! this split is purely organizational: `AppState` is exactly the same
//! type, with exactly the same fields and methods, as before it moved.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::Instant;

use chrono::{DateTime, Utc};
use conway::plugin::PluginStatusContribution;
use conway::{
    AgentId, AgentIntent, AgentResult, Envelope, Event, LogSeq, PermissionMode, ResultStatus,
    SegmentId, SubagentMode, Usage,
};

use super::config::StatusLineConfig;
use super::gate::PendingPrompt;

mod agent_panel;
mod agent_tree;
mod input_line;
mod modal;
mod scroll;
mod status;
mod transcript;
mod turn_summary;

pub use agent_panel::AgentVisibility;
pub use agent_tree::{AgentTreeView, NodeStatus, TreeNode};
pub use input_line::{clamp_history_size, DEFAULT_HISTORY_SIZE};
pub use modal::{AskFate, AskModal, Mode, TrustDecision, TrustPreviewCard};
pub use status::{should_animate, Activity, SPINNER_FRAMES};
pub use transcript::{clamp_tool_preview_lines, Entry, ToolStatus};

/// One installed plugin command, projected into the shape `/help`'s pointer
/// to the palette and `view::palette` need -- `commands::CommandRegistry::palette_entries`
/// is the one producer. `name` already carries its leading `/` (e.g.
/// `"/acme.greet"`), matching `view::palette::CommandSpec::name`'s own
/// convention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginCommandEntry {
    pub name: String,
    pub description: String,
}

/// The confirmation card's three ways out (C2 -- the trust gate for classified
/// `/fork`/`/spawn` intent, which is untrusted and validated rather than
/// trusted). Each maps to exactly one outcome the app loop carries out
/// (`commands::execute_intent_confirm`): `Confirm` runs the classified recipe
/// as-is, `Edit` drops the classified prompt into the input line for the user
/// to re-shape and resubmit, and `Manual` falls back to today's
/// pre-classification flow with the original raw text. There is no fourth way
/// out: quitting with the card open (`Ctrl-C`/`Ctrl-D`) is the manual fallback
/// -- nothing has been created yet (unlike the `/ask` modal, which has a live
/// child to purge), so the quit keys simply pass through and the app loop never
/// reaches `execute_intent_confirm` for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentChoice {
    Confirm,
    Edit,
    Manual,
}

/// The confirmation card's state (C2): one classified [`AgentIntent`] (the
/// output of `Conway::classify_agent_intent`, so every mode shares one
/// classification) waiting for the user to confirm, edit, or discard before ANY
/// agent is created -- inference must never silently choose on the user's
/// behalf. The card is a single modal slot like [`modal::AskModal`].
///
/// Besides the classified `intent` itself, the card carries everything the
/// executor needs to act on `Confirm`/`Manual`:
/// - `default_recipe` is the CALLER's command default (`Fork` for `/fork`,
///   `Spawn` for `/spawn`) -- `Manual` dispatches back to the original
///   command's bare-recipe path with the raw text, and `Confirm` dispatches
///   on `intent.recipe` (which may have been cross-classified).
/// - `raw_text` is the user's original free text, untouched. `Manual` uses
///   it verbatim as the first message; `Edit` populates the input line with
///   `intent.prompt` (the classifier's rewrite), NOT `raw_text`, since the
///   user picked "edit the classified version".
/// - `parent` is the caller's current live agent (`AppState::focused_agent`
///   at classify time) -- the intent session was attached under it as an
///   ephemeral child for the few moments it existed (already purged by C1
///   before this card opens); it is NOT the eventual spawn/fork parent
///   (which is `focused_agent` for `/fork`, `host.root()` for `/spawn` --
///   `commands::execute_intent_confirm` re-derives those the same way
///   `commands::execute`'s bare arms do).
///
/// Defined here (not in `modal`, which owns the rest of the modal-bearing
/// surfaces) because `crates/conway-cli/tests/intent_confirm.rs` pins this
/// struct's definition, and [`AppState::offer_intent_confirm`]/
/// [`AppState::close_intent_confirm`]/[`AppState::begin_intent_confirm_edit`]
/// below, to `state.rs`'s own source text as a source-level surface check.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentConfirm {
    pub intent: AgentIntent,
    pub default_recipe: SubagentMode,
    pub raw_text: String,
    pub parent: AgentId,
}

/// One row of the `[p]` field editor: a top-level argument field of the
/// call being authorized, the value the call carries for it, and whether the
/// operator has pinned it (match this exact value) or left it wildcard
/// (match any value). Pinned here is the *narrowing* direction: every field
/// starts wildcard (preserving today's `[p]`-then-grant = `tool:*`
/// semantics), and the operator pins fields to narrow the grant.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternField {
    pub name: String,
    pub value: serde_json::Value,
    pub pinned: bool,
}

/// The state carried by [`Mode::EditingPattern`]: the prompt being edited
/// (moved out of `AwaitingPermission`, since [`PendingPrompt`] is not
/// `Clone`), the tool name, the per-field rows, and the selected row. The
/// grant scope is NOT carried here -- it lives on [`AppState`] as
/// `permission_grant_scope` (cycled by the prompt's `s` key) and is read at
/// submit, so the edit modal and the prompt share one scope source.
/// Manual `Debug`/`PartialEq`: [`PendingPrompt`] carries a `oneshot::Sender`
/// (not `Debug`/`Eq`), so the derive is impossible. The prompt is ignored for
/// both -- identity is `tool + fields + cursor`, and the [`Mode::EditingPattern`]
/// Debug arm (`state/modal.rs`) formats only the tool, so the prompt never
/// reaches a debug surface anyway.
pub struct EditingPatternState {
    pub prompt: PendingPrompt,
    pub tool: String,
    pub fields: Vec<PatternField>,
    pub cursor: usize,
}

impl std::fmt::Debug for EditingPatternState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditingPatternState")
            .field("tool", &self.tool)
            .field("fields", &self.fields)
            .field("cursor", &self.cursor)
            .finish()
    }
}

impl PartialEq for EditingPatternState {
    fn eq(&self, other: &Self) -> bool {
        self.tool == other.tool && self.fields == other.fields && self.cursor == other.cursor
    }
}

impl EditingPatternState {
    /// Build the field rows from a call's `arguments`: one row per top-level
    /// key of a JSON object, each starting wildcard. A non-object
    /// `arguments` (null, array, scalar) yields no rows -- the resulting
    /// grant is the all-wildcard `tool:*` equivalent, which is the honest
    /// representation (there is no field to pin).
    pub fn from_arguments(prompt: PendingPrompt) -> Self {
        let tool = prompt.request.tool.as_str().to_string();
        let fields = match &prompt.request.arguments {
            serde_json::Value::Object(map) => map
                .iter()
                .map(|(k, v)| PatternField {
                    name: k.clone(),
                    value: v.clone(),
                    pinned: false,
                })
                .collect(),
            _ => Vec::new(),
        };
        Self {
            prompt,
            tool,
            fields,
            cursor: 0,
        }
    }
}

/// One `[plugins].subprocess[]` or `[plugins].mcp[]` entry, exactly as
/// configured on disk (board item `01M0VR5RCCB8NDGG2JEQW8X7XR`,
/// `view/plugins.rs`'s own `/plugin` listing). Both tiers share this same
/// `(id, command)` shape because a listing can only ever show what is
/// CONFIGURED for them -- unlike [`PluginBrowserEntry`], there is no
/// candidate set to browse (`[plugins].subprocess`/`[plugins].mcp`: "every
/// configured entry is spawned unconditionally") and no `PluginDescription`
/// to read, so nothing more than identity is available without actually
/// spawning the command, which this listing deliberately never does (see
/// `view/plugins.rs`'s own doc for why: out of scope, and the descriptive
/// text this crate CAN show honestly is the wire vocabulary each transport
/// bridges, a compile-time constant, not something worth a live handshake
/// to confirm).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfiguredPluginEntry {
    pub id: String,
    pub command: Vec<String>,
}

/// One `[plugins].claude_compat[]` entry, translated (board item
/// `01M0VR89FB1F3Q4FQ8852K2A5E`, `view/plugins.rs`'s own `/plugin`
/// listing): the SAME "config mirror, no candidate set, no toggle" shape
/// [`ConfiguredPluginEntry`] establishes, but carrying a translation
/// REPORT's summary rather than a bare command -- a claude-compat row must
/// be honest about what got translated and what did not (acceptance 5),
/// which a bare `(id, command)` pair cannot express. Populated once at
/// `App::new` from `conway_plugin_claude::discover(&entry.dir)`, re-run
/// against the SAME directory `claude_compat_plugins::install` already
/// validated during startup -- cheap, local-disk-only re-parse, never a
/// second MCP handshake (this struct carries no live plugin object).
#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeCompatPluginEntry {
    pub id: String,
    pub source_dir: std::path::PathBuf,
    /// How many `.mcp.json` server declarations this directory translated
    /// -- the only kind acceptance 2 requires to actually run; each one is
    /// installed as a real plugin by `claude_compat_plugins::install`.
    pub mcp_server_count: usize,
    /// How many `hooks/hooks.json` rules had a same-named conway event.
    ///
    /// **Not informational-only any more.** Before board item
    /// `01M0XBZNBPXEESX8VNTJDKNG0J`, this really was "mapped by name" with
    /// no claim about dispatch; that item made `claude_compat_plugins::
    /// install` append every one of these as a real `[hooks].rules[]`
    /// entry into the SAME `ConwayBuilder` this session runs, so a mapped
    /// hook here dispatches for real -- see [`Self::deny_capable_hook_count`]
    /// for the split an operator needs to know WHAT that means.
    pub mapped_hook_count: usize,
    /// How many of [`Self::mapped_hook_count`] are deny-capable
    /// (`conway::DENY_CAPABLE_EVENTS`) rather than observation-only --
    /// board item `01M0XRD8VMWD273W0W51T8ECCM`, acceptance 4: this row must
    /// distinguish "can refuse a tool call or a submitted prompt" from
    /// "can only watch." Always `<= mapped_hook_count`.
    pub deny_capable_hook_count: usize,
    /// Every unmapped hook's own Claude Code event name.
    pub unmapped_hook_names: Vec<String>,
    /// Every other unusable thing this directory named, by its own
    /// `conway_plugin_claude::UnsupportedItem::name` (a `commands/*.md`
    /// path, a `skills/<name>` path, an `agents/*.md` path, or a
    /// malformed `.mcp.json` server key) -- acceptance 5, the full list an
    /// operator needs to tell "this plugin works" from "this plugin half
    /// works".
    pub unsupported_names: Vec<String>,
}

/// One row of the plugin browser (board item `01M0KARX71A64NTSYTDBVANVPF`,
/// `view/plugins.rs`'s own `/plugin` listing, formerly `view/settings.rs`'s
/// "plugins" section before board item `01M0VR5RCCB8NDGG2JEQW8X7XR` moved
/// it): one compiled-in first-party
/// plugin candidate (`crate::first_party_plugins::all_bundle_plugins` --
/// EVERY candidate this binary links, whether or not `[plugins].install`
/// currently names it), its manifest identity, whether it is currently
/// selected, and its operator-facing description.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginBrowserEntry {
    pub id: String,
    pub version: String,
    /// Mirrors `[plugins].install` membership at the moment `App::new` (or
    /// the last successful toggle) ran -- a DISPLAY value, not the live
    /// `Conway`'s own installed set, which never changes mid-session
    /// (restart-to-apply, `view/settings.rs`'s own footer note). A toggle
    /// flips this field on a successful write so the row reflects what the
    /// operator just asked for, even though nothing about the running
    /// session's own tool/command registry changes until restart.
    pub installed: bool,
    pub description: conway::plugin::PluginDescription,
}

/// V2c: one plugin-declared permission mode as the TUI display layer sees
/// it -- board item `01M0X4YDNVP7TZ0PVSRJ0388SS`,
/// `docs/plugins/permission-modes.md`.
///
/// **A deliberately narrower mirror, not a re-export.** The real type is
/// `conway::ModeCycleEntry::Declared`, and the real
/// cycle-order/collision/uninstall-reconciliation algorithm that consumes
/// it is `conway::ModeCycle` -- ONE implementation, per steering P-14.
/// This crate could name those directly (the facade re-exports both), and
/// deliberately does not: `AppState` is a RENDER model, and the fields it
/// carries are the ones a frame needs. Mirroring the two identifying
/// strings keeps the status line's dependency on the cycle vocabulary to
/// what it actually draws, the same way `permission_mode` below mirrors
/// the broker's mode rather than borrowing the broker.
///
/// Populated at TUI startup from `Conway::mode_cycle`, and kept in step
/// by the `Action::CyclePermissionMode` handler, which mirrors whatever
/// entry `Conway::cycle_permission_mode` moved to rather than recomputing
/// it -- the same "both are written here, together" discipline
/// `permission_mode` below already follows, for the same reason: the
/// broker is the authority, this is a display copy, and the way the two
/// drift is a caller deriving the answer a second time.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredModeMirror {
    pub plugin_id: String,
    pub name: String,
    pub base: PermissionMode,
}

/// The TUI's whole render model. Every mutation goes through [`Self::apply`]
/// (event-driven) or the app loop's direct field writes for input-driven
/// state (`input`, `mode`, `scroll`) -- see `input.rs`/`app.rs`.
pub struct AppState {
    pub transcript: Vec<Entry>,
    pub tree: AgentTreeView,
    /// The last `Event::ModelDecision` envelope seen, for `/why`. Populated
    /// by `app.rs`'s run loop on `Event::ModelDecision` and read by
    /// `commands::render_why`; `apply` intentionally leaves it untouched so
    /// it stays pure.
    pub last_model_decision: Option<Envelope>,
    /// The `ModelDecision` envelope this session saw immediately BEFORE
    /// [`Self::last_model_decision`] -- i.e. what the decision *was*, so
    /// `/why` (`commands::render_why`) can report what changed after a
    /// `/model`/`/role` switch, not merely the latest decision in
    /// isolation. Populated the SAME place `last_model_decision` is
    /// (`app.rs`'s run loop shifts the old value here before overwriting
    /// it), for the identical reason: `apply` stays pure. `None` until a
    /// SECOND `ModelDecision` has been seen this session -- the ordinary
    /// "nothing to compare against yet" case for a session's first turn.
    pub previous_model_decision: Option<Envelope>,
    pub input: String,
    /// Cursor position within `input`, as a *char* index (not byte offset)
    /// -- `input.rs` translates to a byte offset via `char_indices` before
    /// touching the `String`, so this never lands mid-UTF-8-character.
    /// Always in `0..=input.chars().count()`.
    pub cursor: usize,
    pub mode: Mode,
    /// V2: the active permission mode, mirrored from the runtime broker so
    /// the status line can render it every frame without reaching across
    /// the facade per draw. Updated when `/settings` changes it.
    ///
    /// Mirrored rather than owned: the broker is the authority (it is what
    /// actually gates calls); this is a display copy. If the two ever
    /// disagree the broker wins, and the visible consequence is a stale
    /// label -- which is why `/settings` writes both together.
    pub permission_mode: PermissionMode,
    /// V2c: every plugin-declared mode currently installed, mirrored the
    /// same way [`Self::permission_mode`] above is -- see
    /// [`DeclaredModeMirror`]'s own doc for why this stays a NARROW display
    /// mirror rather than a re-exported `conway_runtime::permission_mode`
    /// type. Empty -- which is what every build with no mode-declaring
    /// plugin installed produces -- cycles `Action::CyclePermissionMode`
    /// through the three core modes exactly as it always has.
    pub declared_modes: Vec<DeclaredModeMirror>,
    /// V2c: which entry of [`Self::declared_modes`] (if any) is the
    /// CURRENTLY SELECTED one -- `(plugin_id, name)`, matching a
    /// `DeclaredModeMirror`'s own two identifying fields. `None` means the
    /// operator is in a plain core mode. Modeled as `Option<(String, String)>`
    /// rather than `Option<DeclaredModeMirror>` deliberately: identity (
    /// which mode is selected) and the mode's own data (its `base`) are
    /// separate questions -- the SAME distinction
    /// `conway_runtime::permission_mode::DeclaredModeRef` draws one layer
    /// down, so an uninstalled plugin's stale entry can be recognized by
    /// identity without needing its (now possibly gone) `base` to compare.
    pub active_declared_mode: Option<(String, String)>,
    /// V2b: where a newly-granted pattern is persisted, in precedence
    /// order (project first, then global). Resolved once at `App::new`.
    /// Empty when neither scope resolves, in which case a grant applies
    /// to the session but is not written anywhere.
    pub permission_paths: Vec<std::path::PathBuf>,
    /// V2b: the active pattern ALLOW grants, for the settings review list.
    /// A mirror of the broker's `active_patterns()`, refreshed when
    /// `/settings` opens and after any revoke action — the broker remains
    /// the authority. kept as the
    /// structured `(rule, origin)` pair rather than a pre-formatted string
    /// so `view/settings.rs::build_tree` can both LABEL a row (via
    /// `rule.describe()`/`origin.describe()`) and ADDRESS it for per-rule
    /// revocation — a formatted string alone could show a grant but never
    /// name it to `Conway::revoke_permission_pattern`.
    pub permission_grants: Vec<(conway::PatternRule, conway::PatternOrigin)>,
    /// The structured ALLOW rules the flat form cannot express (F12's
    /// `Rule { select, when, then }` -- `paths_under`, `categories`,
    /// `category_in`, multi-tool), mirrored from the broker's
    /// `active_structured_allow_rules()` with each rule's grant scope.
    /// Rendered in the SAME allow section as [`Self::permission_grants`]
    /// and -- unlike the deny/prompt mirrors below -- REVOCABLE, addressed
    /// by its own `(rule, origin)` pair through
    /// `Conway::revoke_structured_allow_rule` (the flat revoke's key
    /// collapses every structured rule to `None`, which is why these rows
    /// exist as their own leaf-id space). Refreshed alongside
    /// `permission_grants` when `/settings` opens and after any revoke.
    pub structured_allow_rules: Vec<(conway::Rule, conway::PatternOrigin, conway::GrantScope)>,
    /// The active DENY rules (flat form), mirrored from the broker's
    /// `active_deny_patterns()` for `/settings`' read-only deny section.
    /// Deny rules install from ANY permissions file, trusted or not (D4 §3)
    /// -- an untrusted checkout can ship one -- so the operator must be
    /// able to see them and where they came from: a rule set nobody can
    /// inspect is a trap. Read-only by design: they are not revocable from
    /// the menu (`Conway::revoke_permission_pattern`'s own doc argues why a
    /// one-keystroke removal is the wrong shape for a safety rule), so
    /// unlike `permission_grants` these pairs are never used to ADDRESS a
    /// revocation -- only to label a row. Refreshed alongside
    /// `permission_grants` when `/settings` opens.
    pub permission_denies: Vec<(conway::PatternRule, conway::PatternOrigin)>,
    /// The active PROMPT rules (flat form), mirrored from the broker's
    /// `active_prompt_patterns()` -- the prompt half of the same read-only
    /// inspection surface as [`Self::permission_denies`].
    pub permission_prompts: Vec<(conway::PatternRule, conway::PatternOrigin)>,
    /// The structured deny rules the flat form cannot express (F12's
    /// `Rule { select, when, then }`), mirrored from the broker's
    /// `active_structured_deny_rules()`. Rendered in the same read-only
    /// deny section as [`Self::permission_denies`] via `Rule::describe()`.
    pub structured_deny_rules: Vec<(conway::Rule, conway::PatternOrigin)>,
    /// The structured prompt rules the flat form cannot express, mirrored
    /// from the broker's `active_structured_prompt_rules()` -- the
    /// structured half of [`Self::permission_prompts`].
    pub structured_prompt_rules: Vec<(conway::Rule, conway::PatternOrigin)>,
    /// The fourth review list --
    /// every currently-installed DENY-CAPABLE hook-backed rule
    /// (`pre_tool_use` and `prompt_submitted`; see [`conway::Conway::
    /// active_deny_capable_hook_rules`]'s own doc for why observation-only
    /// events are excluded), mirrored from the broker/dispatcher for the
    /// same reason `permission_grants` is: the menu builder stays a pure
    /// function of `AppState`. Revocable, addressed by each row's own
    /// `(event, id)` identity via `Conway::revoke_hook_rule` -- refreshed
    /// alongside `permission_grants` when `/settings` opens and after any
    /// revoke, on the same seam.
    pub hook_rules: Vec<conway::HookRuleView>,
    /// The plugin browser's own read surface (board item
    /// `01M0KARX71A64NTSYTDBVANVPF`): every compiled-in first-party plugin
    /// candidate, on or off, with its description -- populated once at
    /// `App::new` and mutated locally by a successful toggle (see
    /// [`PluginBrowserEntry::installed`]'s own doc for why this is a
    /// display mirror, never the live installed set).
    pub plugin_browser: Vec<PluginBrowserEntry>,
    /// Every configured `[plugins].subprocess[]` entry (board item
    /// `01M0VR5RCCB8NDGG2JEQW8X7XR`) -- populated once at `App::new` from
    /// `conway.config().plugins.subprocess`, never mutated afterward (no
    /// candidate set, no toggle: `view/plugins.rs`'s own doc). Read by the
    /// `/plugin` listing alongside [`Self::plugin_browser`] and
    /// [`Self::mcp_plugins`].
    pub subprocess_plugins: Vec<ConfiguredPluginEntry>,
    /// Every configured `[plugins].mcp[]` entry, the MCP-tier counterpart of
    /// [`Self::subprocess_plugins`] -- same "config mirror, no candidate
    /// set, no toggle" shape.
    pub mcp_plugins: Vec<ConfiguredPluginEntry>,
    /// Every configured `[plugins].claude_compat[]` entry, translated
    /// (board item `01M0VR89FB1F3Q4FQ8852K2A5E`) -- the fourth `/plugin`
    /// source, populated once at `App::new` by re-running
    /// `conway_plugin_claude::discover` against the SAME directory
    /// `claude_compat_plugins::install` already validated. See
    /// [`ClaudeCompatPluginEntry`]'s own doc for why this carries a
    /// translation summary rather than a bare command.
    pub claude_compat_plugins: Vec<ClaudeCompatPluginEntry>,
    /// Board item `01M0VR5RCCB8NDGG2JEQW8X7XR`: whether the `/plugin`
    /// listing (`view/plugins.rs`) is showing. Follows [`Self::help_open`]/
    /// [`Self::settings_open`]'s own pattern exactly -- informational, not
    /// decision-owed, so a plain flag rather than a `Mode` variant --
    /// mutually exclusive with BOTH of them (`Self::open_plugins`/
    /// `Self::open_settings`/`Self::open_help` each clear the other two).
    pub plugins_open: bool,
    /// The `/plugin` listing's own arrow-navigated cursor -- the RAW row
    /// index, mirroring [`Self::settings_selected`]'s own "persisted
    /// unclamped, re-clamped on read by `MenuState::selected_index`" shape.
    pub plugins_selected: usize,
    /// The scope the permission prompt's remembered-grant keys (`a` and
    /// `p`) grant at: `Session` (the default, and the only scope the prompt
    /// offered before this item), `Agent` (only the agent whose call is
    /// being asked about), or `AgentSubtree` (that agent's whole subtree).
    /// Cycled by the prompt's `s` key (`input.rs::handle_permission_key`),
    /// rendered by `view/mod.rs::draw_permission_overlay`, and reset to
    /// `Session` every time a NEW prompt becomes the active one (see
    /// [`Self::offer_prompt`]/`Self::promote_next_surface`) -- a scope
    /// chosen for one call must never silently carry over to the next,
    /// exactly the same reason `modal_scroll` resets per surface.
    pub permission_grant_scope: conway::PermissionScope,
    /// The transcript's scroll offset (wrapped lines from the top), only
    /// meaningful while `follow_tail` is `false` -- see that field's own
    /// doc. Mutated by [`Self::scroll_page_up`]/[`Self::scroll_page_down`]
    /// (`input.rs`'s PageUp/PageDown), never directly by `app.rs`.
    pub scroll: u16,
    /// Stick-to-bottom auto-follow (the "the UI doesn't scroll" report's
    /// root cause: new output scrolling off-screen with no way back to it).
    /// `true` (the default) means the transcript view is pinned to its own
    /// bottom regardless of `scroll`'s stored value -- `view/transcript.rs`'s
    /// `draw` recomputes the effective scroll offset as `max_scroll` on
    /// every render while this is set, so growth never has to notify this
    /// struct at all. Set to `false` by [`Self::scroll_page_up`] (scrolling
    /// up to review history); reset to `true` by [`Self::scroll_page_down`]
    /// once it lands back on the bottom.
    pub follow_tail: bool,
    /// Prompts that arrived while another was already showing -- drained
    /// into `mode` as each one resolves (module notes: "concurrent requests
    /// queue in arrival order").
    pub queued_prompts: std::collections::VecDeque<PendingPrompt>,
    /// Whether the below-chat agent-tree panel (criterion 4) is
    /// currently shown. Toggled by `/agents` (handled in `app.rs`, since
    /// `commands.rs` -- out of this item's file scope -- owns no such
    /// command); never an always-on pane.
    pub agent_view_open: bool,
    /// Arrow-navigated row in the slash-command palette, or `None`
    /// when the user has typed a `/` prefix but not yet pressed an arrow. The
    /// arrow keys move this and autofill [`AppState::input`] with the
    /// highlighted command (see `input.rs`); typing resets it to `None`.
    pub palette_selected: Option<usize>,
    /// The text the palette's match list stays anchored to: whatever the
    /// user last *typed*. Arrow navigation autofills `input` with a whole
    /// command but leaves this alone, so cycling the list does not collapse
    /// it to the single autofilled entry. Read via [`AppState::palette_source`].
    palette_stem: String,
    /// Arrow-selected row in the on-demand agent panel. An index
    /// into the panel's FILTERED rows (`Self::visible_agent_nodes`), not the
    /// raw `tree.nodes` (item A2: the draw-time visibility filter decides
    /// which rows exist); clamped against the filtered count wherever it is
    /// read, so tree growth/shrink or a filter change never leaves it
    /// dangling. Only meaningful while `agent_view_open`.
    pub agent_selected: usize,
    /// The `/agents` panel's draw-time visibility filter (item A2) --
    /// which tree nodes the panel rows show. Defaults to `All` (V5: see
    /// [`AgentVisibility::All`]'s own doc for why); cycled by `v` while
    /// the panel is open via [`Self::cycle_agent_visibility`]. Read only at
    /// draw time (`view/agents.rs`) and by the panel's own navigation
    /// (`Self::agent_scroll`, `input.rs`'s Enter-to-focus); the tree itself
    /// is never filtered.
    pub agent_visibility: AgentVisibility,
    /// The agent whose conversation the transcript pane currently shows
    ///. Distinct from `agent_selected` -- that field is only the
    /// `/agents` panel's browsing cursor (which row is highlighted while
    /// navigating with the arrow keys); this is which agent's OWN
    /// transcript+live stream `app.rs` is actually subscribed to and
    /// `self.transcript` reflects. Defaults to the session's root
    /// (`AppState::new`). Mutated by [`Self::focus_agent`] only -- `apply`
    /// never touches it, so a live envelope from the currently-focused
    /// agent's own stream is applied without needing to re-check this field
    /// at all (the app loop only ever hands `apply` envelopes from whichever
    /// stream it is currently subscribed to).
    pub focused_agent: AgentId,
    /// The focused agent's current live activity , rendered by `view/status.rs`. See
    /// [`Activity`]'s own doc for the event-driven transitions.
    pub activity: Activity,
    /// The focused agent's cumulative token spend, rendered alongside
    /// `activity` in the status line (same). This field is
    /// live-incremented from `Event::TurnFinished{usage}` in
    /// [`Self::apply`] for immediate feedback, but is NOT authoritative on
    /// its own -- `app.rs`'s run loop re-fetches the true total via the
    /// `SessionHandle::session_usage` facade accessor (through the `Host`
    /// trait's `session_usage`) on focus change and after
    /// `TurnFinished`/`AgentFinished` for the focused agent, overwriting
    /// whatever this field held (replay carries no `Usage` at all -- the
    /// `record_to_event` maps a replayed `Assistant` record to `TextDelta`,
    /// not `TurnFinished` -- so this field alone would silently stay zero
    /// after any focus switch onto an agent with prior turns without that
    /// authoritative refresh).
    pub focused_agent_usage: Usage,
    /// The persisted head [`LogSeq`] of THIS session's own log -- i.e.
    /// `self.handle.id()`'s log, the calling session `CommandOutcome::
    /// ForkSession::at_seq`/`Conway::fork_from` resolve against (///, `/conway.history.rewind`'s own item).
    /// Deliberately SESSION-scoped, not agent-scoped like
    /// [`Self::focused_agent_usage`] immediately above: `SessionStore` keys
    /// one session per agent, and a fork targets `self.handle`'s OWN
    /// session regardless of which agent the transcript pane happens to be
    /// focused on, so this tracks the ROOT agent's own turn boundaries, not
    /// the focused agent's.
    ///
    /// **Why this field exists at all.** Before this item, nothing in the
    /// TUI showed an operator any `LogSeq` at all -- `Envelope::seq` (the
    /// live event stream's own field) is a PER-CONNECTION renumbering, not
    /// the persisted seq `fork_from` accepts (see that field's own doc),
    /// so surfacing it here directly would have been actively misleading.
    /// This field instead mirrors [`Self::focused_agent_usage`]'s own
    /// "authoritative refetch" shape one field over: `app.rs`'s run loop
    /// re-fetches the true head via `Conway::session_head` (a single,
    /// already-existing facade accessor -- no new port) after a root-agent
    /// `TurnFinished`/`AgentFinished`, and at session start; a
    /// `ForkSession` needs no such round trip at all, since the CHILD's
    /// fresh head is exactly the `at_seq` it was forked at
    /// (`apply_plugin_command_done`'s own `ForkSession` arm sets this
    /// directly). `None` only until the first of those points has run once
    /// (a session with literally no history yet). Rendered by
    /// `view/status.rs`'s `session` field as `session <id>@<seq>` -- the
    /// exact `<session-id>[@<seq>]` syntax `session_ref.rs`'s own
    /// `--fork-from` flag already uses, reused rather than a second
    /// notation invented for the same number.
    pub session_head_seq: Option<LogSeq>,
    /// An answered `/ask` modal waiting for a permission prompt (or another
    /// modal) to clear first (B5) -- `app.rs`'s ask-result arm calls
    /// [`Self::offer_ask_modal`], which parks the modal here whenever `mode`
    /// is not `Normal`; [`Self::resolve_current_prompt`] opens it once the
    /// prompt queue drains. The two modal surfaces never stack.
    pending_ask_modal: Option<AskModal>,
    /// A C2 confirmation card parked behind another modal-bearing surface
    /// (a permission prompt or an `/ask` modal) -- `commands::execute`'s
    /// free-text `/fork`/`/spawn` arm calls [`Self::offer_intent_confirm`]
    /// right after `Conway::classify_agent_intent` returns, which parks here
    /// whenever `mode` is not `Normal`. [`Self::close_ask_modal`],
    /// [`Self::close_intent_confirm`], and [`Self::resolve_current_prompt`]
    /// all funnel through `Self::promote_next_surface` to drain the
    /// queued-prompts / `pending_ask_modal` / `pending_intent_confirm`
    /// slots in that fixed priority order, so the three modal-bearing
    /// surfaces never stack.
    pending_intent_confirm: Option<IntentConfirm>,
    /// A trust-preview card parked behind another modal-bearing surface --
    /// mirrors `pending_intent_confirm` exactly. `commands::execute`'s
    /// `SlashCommand::Trust` arm calls [`Self::offer_trust_preview`] once
    /// `Host::preview_trust_target` returns, which parks here whenever
    /// `mode` is not `Normal`. Drained in the SAME fixed priority order
    /// [`Self::promote_next_surface`] already documents (queued prompt,
    /// then ask, then intent card, then this).
    pending_trust_preview: Option<TrustPreviewCard>,
    /// Whether an `/ask` child's single turn is currently in flight (B5).
    /// Set by `app.rs` when it spawns the ask task, cleared when the result
    /// arrives -- while set, a second `/ask` is refused with a `Notice`
    /// (the modal is a single-question surface; concurrent asks would
    /// compete for the one [`Mode::AskModal`] slot).
    pub ask_in_flight: bool,
    /// The ephemeral ask child's `AgentId`, known once `app::ask::AskUpdate::
    /// Started` arrives (the fork succeeded -- `SessionHandle::ask` returned
    /// a `TurnHandle`, before its single turn has necessarily finished).
    /// `None` from submit until that update arrives, and again once the ask
    /// resolves (answered, abandoned, or failed to fork at all). This is
    /// what a keyboard abandon (`App::abandon_ask`) needs to target --
    /// `ask_in_flight` alone names THAT something is running, not WHICH
    /// agent to cancel.
    pub ask_child: Option<AgentId>,
    /// When the current `/ask` started (board item
    /// `01M0RWFH6V709B7WTAFRZGFKG3`): stamped at submit time (`commands::
    /// execute`'s `SlashCommand::Ask` arm, alongside `ask_in_flight`), read
    /// by the status line's `activity` field to show live elapsed seconds
    /// while the ask is in flight -- the SAME spinner/elapsed visual
    /// language an ordinary turn's `turn_started_at` already uses (see
    /// `view/status.rs::activity_ladder`), not a second progress language.
    /// Cleared whenever `ask_in_flight` is cleared.
    pub ask_started_at: Option<Instant>,
    /// Set by `App::abandon_ask` (board item `01M0RWFH6V709B7WTAFRZGFKG3`)
    /// the moment the operator abandons an in-flight ask from the keyboard,
    /// BEFORE the spawned task's eventual `AskUpdate::Done` arrives (that
    /// task is still running -- cancelling it does not make it vanish
    /// instantly, it makes the child's turn wind down). When `Done` does
    /// arrive with this set, `App::run`'s own arm purges the (by then
    /// actually-finished) child and records an "ask abandoned" notice
    /// instead of opening the answer modal over a question nobody is
    /// waiting on any more. Cleared alongside `ask_in_flight`/`ask_child`.
    pub ask_abandoned: bool,
    /// The shared modal body-scroll offset (V1; originated as the
    /// permission-overlay-only `permission_scroll`, bug fix
    ///: "no way to see the entire command" for a
    /// long tool-call argument). Driven by `PageUp`/`PageDown` while any of
    /// the four modal-bearing surfaces is up (`Mode::AwaitingPermission`/
    /// `Mode::AskModal`/`Mode::IntentConfirm`, or the informational `/help`
    /// overlay -- `input.rs`'s four `handle_*_key` fns), read by whichever
    /// `view/mod.rs::draw_*`/`view/help.rs::draw` is currently on screen
    /// (each clamps it to its OWN content's wrapped line count via
    /// `view/modal.rs::clamp_scroll`, so this can hold an arbitrarily large
    /// value with no risk of scrolling past real content).
    ///
    /// **One field serves all four surfaces** because at most one of them is
    /// EVER showing at a time -- the three `Mode` variants are mutually
    /// exclusive by construction (`Self::mode`'s own doc), and `/help` never
    /// stacks on top of one either (`Self::help_open`'s own doc) -- so there
    /// is never a moment where two surfaces could each want a different
    /// scroll position out of this one field. Reset to 0 whenever a NEW
    /// surface becomes the active one, so a leftover scroll position from a
    /// previous, unrelated surface's content never carries over: see
    /// [`Self::offer_prompt`], `Self::promote_next_surface`,
    /// [`Self::offer_ask_modal`], [`Self::offer_intent_confirm`], and
    /// [`Self::open_help`].
    pub modal_scroll: u16,
    /// The current braille spinner frame index (T2). Advanced by
    /// [`Self::tick_animation`] modulo [`SPINNER_FRAMES`]' length, only while
    /// [`Self::activity`] is not [`Activity::Idle`]. Rendered by
    /// `view/status.rs` as the glyph preceding the activity phrase.
    pub spinner_frame: usize,
    /// When the focused agent's current turn started (T2): set by
    /// `Event::TurnStarted` for the focused agent and cleared whenever
    /// `activity` returns to [`Activity::Idle`] (`TurnFinished`/
    /// `AgentFinished` for the focused agent, or [`Self::focus_agent`]). The
    /// status line renders live `elapsed` from `Instant::now() -
    /// turn_started_at` while this is `Some`; `None` while idle.
    pub turn_started_at: Option<Instant>,
    /// New context tokens ADDED this turn (T2): the sum of
    /// `Event::ContextSegmentAdded { tokens_est }` deltas observed on the
    /// focused agent's own stream between `TurnStarted` and `TurnFinished`.
    /// The runtime emits `ContextSegmentAdded` only for segments NEW to a
    /// session-scoped `seen_segments` set that is deliberately NEVER reset
    /// across turns, so this is a session-deduped segment-delta count -- NOT
    /// total context occupancy and NOT the authoritative turn-end token
    /// total. On turn 1 it reads ~full context size (every segment is new);
    /// on turn 2+ only genuinely new segments fire, so for the same
    /// conversation it is large on turn 1 then small on turn 2. The status
    /// line renders it with a leading `+` (`+{n} tok`) to signal "added
    /// this turn" and to distinguish it from the cumulative
    /// `| {tokens} tok |` slot; the authoritative turn-end token total
    /// lands via the turn-end summary (T4). Reset to 0 on `TurnStarted` and
    /// on [`Self::focus_agent`]; cleared when `activity` returns to idle.
    /// Distinct from [`Self::focused_agent_usage`], which is the cumulative
    /// spend across all of the focused agent's turns.
    pub turn_running_tokens: u64,
    /// T4: the transcript length at the moment the focused agent's current
    /// turn started (`Event::TurnStarted`) -- the watermark that bounds
    /// `Self::stamp_turn_summary`'s reverse scan to entries THIS turn
    /// produced.
    ///
    /// Without it the scan walks the whole transcript, so a turn that emits
    /// no model text of its own (a tool-only agentic round) would walk past
    /// its own `Tool` entries into the PREVIOUS turn and re-stamp that
    /// already-settled bubble with this turn's elapsed/token figures --
    /// silently misattributing spend to an unrelated reply, the exact
    /// provenance corruption T4 exists to prevent. Bounding the scan makes
    /// the tool-only case the intended no-op instead.
    ///
    /// Reset to the current transcript length on `TurnStarted` and to 0 on
    /// [`Self::focus_agent`] (a fresh focus clears the transcript, so 0 is
    /// the correct floor).
    pub turn_transcript_start: usize,
    /// T3: the focused agent's serving model display name, from
    /// `Event::ModelDecision { chosen }` (`ModelRef::to_string()`). `None`
    /// until a `ModelDecision` is known for the focused agent. Reset to
    /// `None` on [`Self::focus_agent`], but -- T3 follow-up -- not left
    /// there: `app.rs`'s `try_focus_agent` immediately re-fetches the
    /// serving model via `SessionHandle::last_model` (reads the last
    /// `LogRecord::Assistant` directly, so this works for an agent that has
    /// already run a turn with no LIVE `ModelDecision` required) and also
    /// repopulated whenever the focused agent's own next live
    /// `ModelDecision` arrives. The status line's `model` field renders
    /// this and is omitted while it is `None` (genuinely no turn yet, on
    /// either path).
    pub focused_model: Option<String>,
    /// T3: the focused model's max context window in tokens, looked up from
    /// the local model-metadata map (`Conway::model_metadata`, T3
    /// follow-up: no longer re-read from disk here -- see
    /// [`Self::model_max_context`]'s own doc) by the focused model's
    /// `"backend/model"` string at the time a `ModelDecision` arrives OR
    /// `try_focus_agent`'s re-fetch resolves one (same lookup, same
    /// fallback-to-bare-model-id rule, in both places). `None` when the
    /// metadata map has no entry for the chosen model (or is empty) -- the
    /// status line then renders the raw `focused_ctx_tokens` figure (e.g.
    /// `ctx 12.3k`) instead of a percentage. Reset to `None` on
    /// [`Self::focus_agent`].
    pub focused_model_max_context: Option<u32>,
    /// T3: the focused agent's cumulative context-occupancy estimate, the
    /// deduped-by-`SegmentId` sum of every
    /// `Event::ContextSegmentAdded { tokens_est }` observed on the focused
    /// agent's own stream since the focus began. The status line's `ctx`
    /// field renders `focused_ctx_tokens / focused_model_max_context` as a
    /// percentage when the max is known, else the raw token count. Reset to
    /// 0 on [`Self::focus_agent`], then -- T3 follow-up -- immediately
    /// re-seeded by `app.rs`'s `try_focus_agent` from
    /// `SessionHandle::context_report_current`'s `total_tokens_est` (and
    /// [`Self::focused_seen_segments`] from that same report's segment
    /// ids, so the very next live `ContextSegmentAdded` dedupes correctly
    /// against what this fetch already counted) -- see that method's own
    /// doc for why a fresh focus no longer needs to wait on a live turn.
    ///
    /// Dedup rationale (T3 code-review fix 1): the runtime's
    /// `seen_segments` is a LOCAL `HashSet` constructed fresh at the top of
    /// each `AgentLoop::run_inner`, NOT a session-scoped set. For
    /// `keep_alive: false` children (every spawned child), each new prompt
    /// spawns a fresh `AgentLoop` with an empty `seen_segments`, so the
    /// first turn of the new run re-emits `ContextSegmentAdded` for EVERY
    /// existing context segment. Without per-segment-id dedup at the
    /// renderer this double-counts and `focused_ctx_tokens` climbs to
    /// `ctx 100%` and never comes back down. [`Self::focused_seen_segments`]
    /// is the dedup set; accumulation is gated on its `insert(segment)`
    /// returning true (genuinely new segment id for this focused session).
    /// Replay itself still does NOT synthesize `ContextSegmentAdded`
    /// (`record_to_event` maps a replayed `Assistant` record to `TextDelta`,
    /// never to `ContextSegmentAdded`) -- but as of the T3 follow-up above,
    /// nothing depends on replay for this figure any more: `try_focus_agent`
    /// re-fetches the true total directly, so a freshly focused agent shows
    /// its real `ctx%` immediately, not `ctx 0%` pending its own next live
    /// turn.
    pub focused_ctx_tokens: u64,
    /// T3 code-review fix 1: per-focused-agent session-scoped dedup set for
    /// `ContextSegmentAdded` segment ids. Accumulation into
    /// [`Self::focused_ctx_tokens`] only happens when
    /// `focused_seen_segments.insert(segment)` returns true. Reset on
    /// [`Self::focus_agent`] -- a freshly focused agent starts with an
    /// empty seen-set -- then (T3 follow-up) immediately re-seeded by
    /// `app.rs`'s `try_focus_agent` with the segment ids already counted in
    /// the re-fetched [`Self::focused_ctx_tokens`] total, so dedup stays
    /// correct against a live agent's next `ContextSegmentAdded` instead of
    /// double-counting a segment that fetch already included.
    pub focused_seen_segments: HashSet<SegmentId>,
    /// T3: the current git branch, read once at startup via
    /// `git rev-parse --abbrev-ref HEAD` (best-effort: `None` when not a
    /// git repo, git is absent, or the command fails). No polling. The
    /// status line's `git` field renders this and is omitted while `None`.
    pub git_branch: Option<String>,
    /// T3: the session's working directory display string, from the `Cli`
    /// / session config at startup. The status line's `cwd` field renders
    /// this; `None` means "do not render the cwd field".
    pub cwd_display: Option<String>,
    /// T3: the resolved `[tui.status_line]` config (ordered field names +
    /// visibility). Set at `App::new` from `crate::tui::config::load`;
    /// `AppState::new` defaults to the Lean line. The status-line renderer
    /// reads this to decide which fields to render and in what order.
    pub status_line_config: StatusLineConfig,
    /// T5: the cap on collapsed tool-preview lines in the transcript
    /// (`[tui.tool_preview_lines]`, default 3). A tool entry whose stored
    /// `preview` has more physical lines than this renders the first N
    /// lines followed by a dim `… (+M lines, Ctrl-E to expand)` affordance
    /// while `Entry::Tool::expanded` is `false`; the full preview renders
    /// while `expanded` is `true`. The stored `preview` is NEVER truncated
    /// -- the cap is render-time only. Set at `App::new` from
    /// `crate::tui::config::load`'s `tool_preview_lines` via
    /// [`clamp_tool_preview_lines`] (config is untrusted: clamped to `1..=200` with a
    /// fallback to the default of 3 on a missing/out-of-range/bad value,
    /// never a panic).
    pub tool_preview_lines: u32,
    /// T3: the local model-metadata map (`"backend/model"` -> max context
    /// tokens), derived once at `App::new` from `Conway::model_metadata()`
    /// (T3 follow-up: no longer a second, independent read of
    /// `[models.metadata_path]` -- `ConwayBuilder::build` already loaded and
    /// parsed that file once; `App::new` now reuses that SAME parse instead
    /// of re-reading the file itself, so there is exactly one code path
    /// that can drift from the file's actual contents). `apply`'s
    /// `ModelDecision` arm, and `app.rs`'s `try_focus_agent` re-fetch alike,
    /// look up the chosen model here to set `focused_model_max_context`.
    /// Empty when the builder found no metadata file or it named no models
    /// -- the status line then renders raw context tokens instead of a
    /// percentage.
    pub model_max_context: HashMap<String, u32>,
    /// T4: whether reasoning-trace entries ([`Entry::Reasoning`]) are
    /// rendered in the transcript. Defaults `true` (reasoning EXPANDED by
    /// default) -- the user opts OUT from the `/settings` menu's "show
    /// reasoning traces" row (V4; formerly the standalone `/thinking`
    /// command), which flips this to `false` and `build_lines` then skips
    /// `Entry::Reasoning` entirely. Toggled by [`AppState::toggle_thinking`].
    /// Kept on the state (not the entry) because the show/hide is a global
    /// view preference, not per-entry state -- reasoning entries are still
    /// STORED regardless, so toggling back on restores them without replay.
    pub show_reasoning: bool,
    /// T4: whether per-entry timestamps are rendered. Defaults `false`
    /// (timestamps OFF by default) -- the user opts IN from the `/settings`
    /// menu's "show timestamps" row (V4; formerly the standalone
    /// `/timestamps` command), which flips this to `true` and `entry_lines`
    /// then prepends `HH:MM ` to each entry's first rendered line. Toggled
    /// by [`AppState::toggle_timestamps`]. The timestamp itself is always
    /// STORED on the entry (`Entry::Assistant::ts` etc., stamped from the
    /// envelope's `ts` at apply time) so toggling back on restores the
    /// stamps without replay.
    pub show_timestamps: bool,
    /// T8: the persisted input-history FIFO, oldest entry at the front.
    /// Loaded once at `App::new` from the history file (best-effort -- see
    /// `history::load`'s own doc -- the file is untrusted input) and appended to by
    /// [`Self::push_history`] on every submit; `App::submit` persists the
    /// updated deque back to disk after each push (also best-effort -- a
    /// failed WRITE must never fail the submit that triggered it). Bounded
    /// by [`Self::history_cap`]: [`Self::push_history`] evicts from the
    /// front once the cap is exceeded, so this can never grow unbounded.
    pub history: VecDeque<String>,
    /// T8: the cap on [`Self::history`]'s length (`[tui.history_size]`,
    /// default 500). Set at `App::new` via [`clamp_history_size`]. `0` is a
    /// valid (if degenerate) cap -- [`Self::push_history`] then clears
    /// `history` on every push rather than dividing by zero or growing
    /// unbounded.
    pub history_cap: usize,
    /// T8: which entry of [`Self::history`] `Up`/`Down` are currently
    /// showing in [`Self::input`], or `None` when the user is composing a
    /// fresh, unrecalled line. `Some(i)` indexes `history` directly (`0` =
    /// oldest). Reset to `None` by [`Self::push_history`] (a fresh submit
    /// always starts unrecalled) and by [`Self::history_recall_next`] once
    /// `Down` walks past the newest entry back to the in-progress draft.
    /// Editing the recalled text (typing, Backspace, ...) deliberately does
    /// NOT reset this -- the recalled prompt stays "editable inline"
    /// (item spec) without losing your place in the history list, mirroring
    /// how a shell's own history search behaves.
    history_index: Option<usize>,
    /// T8: the unsent text that was in `input` at the moment `Up` first
    /// started browsing `history` (`history_index` went from `None` to
    /// `Some`) -- restored by [`Self::history_recall_next`] once `Down`
    /// walks past the newest history entry, so composing a message, then
    /// idly pressing `Up` to glance at an old one, then pressing `Down`
    /// back down never loses what you were typing.
    history_draft: String,
    /// T7: whether the `/help` keybinding overlay is showing. Toggled by
    /// [`Self::open_help`]/[`Self::close_help`] (`commands.rs`'s `/help` arm
    /// and `Esc`, respectively, via `input.rs`).
    ///
    /// **Deliberately NOT a [`Mode`] variant**, unlike the three modal-
    /// bearing surfaces above (`AwaitingPermission`/`AskModal`/
    /// `IntentConfirm`): those three are each a DECISION the user owes an
    /// answer to (a tool call is blocked, an ephemeral ask needs a fate, a
    /// classified intent needs confirming) -- `mode` exists precisely to
    /// make "exactly one such decision is live at a time" a type-level
    /// invariant, with `promote_next_surface` draining the queue/park slots
    /// in a fixed priority order once one resolves. The help overlay is
    /// nothing like that: it is a passive, read-only reference with no
    /// state of its own to lose and nothing the user owes an answer to, so
    /// giving it a `Mode` slot (and a park/promote path alongside the other
    /// three) would be complexity with no payoff.
    ///
    /// Instead, `view::draw` gates the overlay on `help_open &&
    /// matches!(mode, Mode::Normal)` (see that function's own comment) and
    /// `input::handle_key` gates its own key-swallowing the same way. This
    /// gives the required "never stacks on an active decision" behavior for
    /// free: `offer_prompt`/`offer_ask_modal`/`offer_intent_confirm` all
    /// transition `mode` away from `Normal` the instant one of those three
    /// surfaces arrives, regardless of `help_open` -- the overlay just stops
    /// being drawn/reachable the moment that happens, with no need to touch
    /// this flag at all, and reappears on its own once `mode` returns to
    /// `Normal` (nothing ever resets `help_open` on their account). A
    /// `/help` submission can only ever reach [`Self::open_help`] while
    /// `mode` is already `Normal` in the first place -- the input line is
    /// inert while any of the other three surfaces owns `mode` (see each of
    /// their own "input line is inert" docs), so `/help` itself can never be
    /// typed/submitted while one is active.
    pub help_open: bool,
    /// V4: whether the `/settings` menu (`view/settings.rs`) is showing.
    /// Follows [`Self::help_open`]'s own pattern EXACTLY -- see that field's
    /// doc for the full "informational, not decision-owed, so a plain flag
    /// rather than a `Mode` variant" reasoning, which applies here
    /// unchanged: settings is a session-only display-preferences surface
    /// with nothing the user owes an answer to.
    ///
    /// The one addition V4 makes: `settings_open` and `help_open` are also
    /// mutually exclusive WITH EACH OTHER (`Self::open_settings`/
    /// `Self::open_help` each clear the other). Both are gated the same way
    /// (checked ahead of the `Mode` match in `input::handle_key`, drawn the
    /// same way in `view::draw`), so if both were ever `true` at once, only
    /// ONE of them would actually be reachable/visible -- whichever this
    /// crate's fixed check order happens to see first -- stranding the
    /// other open in the background with no way back to it except by
    /// re-toggling its own flag from outside. Clearing the other on open
    /// makes "at most one of the two is ever showing" a real invariant
    /// instead of an accident of check order.
    pub settings_open: bool,
    /// V4: the settings menu's arrow-navigated cursor -- the RAW row index,
    /// persisted across renders/keypresses the same way
    /// [`Self::agent_selected`] is for the `/agents` panel. Read/written via
    /// `view/settings.rs::build_tree` (which rebuilds a fresh `MenuState`
    /// from the CURRENT settings values on every call and restores this
    /// cursor onto it via `MenuState::set_selected`) and
    /// `input::handle_settings_key` (which writes back whatever
    /// `MenuState::selected_index` -- already clamped to the current row
    /// count -- comes out the other side). Unclamped storage is safe: a
    /// stale value left over from before a group collapsed elsewhere is
    /// re-clamped on read the same way `MenuState::selected_index` already
    /// clamps internally.
    pub settings_selected: usize,
    /// V4: which of the settings tree's top-level GROUP labels are
    /// currently collapsed (default: none, i.e. every group starts
    /// expanded -- mirrors `view/menu.rs::MenuNode::group`'s own
    /// `expanded: true` default). Keyed by the group's own label text
    /// rather than an enum,
    /// so a future settings category needs no new field here -- only a new
    /// entry in `view/settings.rs::build_tree`'s root list. Toggled by
    /// `input::handle_settings_key`'s `Enter` arm on a group row.
    pub settings_collapsed_groups: HashSet<String>,
    /// The installed plugin commands, for `/help`'s pointer to the palette
    /// and `view::palette`'s own live-filtered listing. **NOT reset by `/resume`** despite
    /// `AppState::new` seeding it empty by default -- this is
    /// process-lifetime configuration (which plugins `conway-cli` installed
    /// at startup), not session-scoped state; `commands::execute`'s own
    /// `Resume` arm carries the pre-reset value across by hand (see that
    /// arm's own comment). `Arc` so cloning it (every `AppState::new` call,
    /// the `Resume` carry-across) is a refcount bump, not a `Vec` copy.
    pub plugin_commands: std::sync::Arc<Vec<PluginCommandEntry>>,
    /// The operator-chosen agent names `conway.names` stores, when that
    /// plugin is installed (board item `01M0TV5BSE98S16SFYECG9G9WP`,
    /// decision `01M0TV3ZZBDKSSV7MD0FW3FSY7`).
    ///
    /// **`None` is the whole of the uninstalled behaviour.** Every reader
    /// -- `commands::resolve_agent`, `view::agents::draw` -- treats `None`
    /// and "installed but this agent has no name" identically, so a build
    /// without `conway.names` in `[plugins].install` behaves exactly as
    /// this crate did before the field existed. `AppState::new` seeds it
    /// `None`; `tui::run` sets it once, immediately after `App::new`, from
    /// the ONE store `main.rs` resolved for this process (see that
    /// function's own doc for why it is not an `App::new` parameter).
    ///
    /// **NOT reset by `/resume`**, for the same reason
    /// [`Self::plugin_commands`] is not: this is process-lifetime
    /// configuration (which plugins this binary installed at startup), not
    /// session-scoped state. `commands::execute`'s `Resume` arm carries it
    /// across the `AppState::new` reset by hand, alongside
    /// `plugin_commands` (and, since board item
    /// `01M0XDEDBR5YDF71Q7ZRXYMT85`, [`Self::plugin_status_contributions`]).
    ///
    /// The trait is `conway_plugin_names`'s own, not `conway-core`'s --
    /// naming ships entirely in the plugin tier and core never learns the
    /// word "name". This crate may name it because it already links that
    /// crate in order to install it; see `conway_plugin_names`'s module doc.
    pub agent_names: Option<std::sync::Arc<dyn conway_plugin_names::AgentNames>>,
    /// A snapshot of `Conway::plugin_status_contributions()` (board item
    /// `01M03VKQ738DTGHHK2C4RWXC0E`), read by the status line's `plugins`
    /// field (board item `01M0X1B7Z41J57N6YP2JFZ2AZW`,
    /// `view::status::status_line_spans` -- see that module's own doc for
    /// the bounding/degrade rules and the guarantee that a contribution can
    /// never displace the `mode` field's own safety signal).
    ///
    /// `AppState::new` seeds this empty, matching every other collection
    /// field's construction-time default. **Populated once, at TUI
    /// startup** (board item `01M0XC1GF73Z9GTE7TN65TRW4A`), by
    /// `App::new` copying `conway.plugin_status_contributions()` -- the
    /// same "populate once outside the render path" shape
    /// [`Self::plugin_commands`]/[`Self::agent_names`] already use.
    ///
    /// **NOT reset by `/resume`**, for the same reason
    /// [`Self::plugin_commands`]/[`Self::agent_names`] are not (board item
    /// `01M0XDEDBR5YDF71Q7ZRXYMT85`, closing the gap those two items'
    /// carry-across list left this field out of): the value is
    /// `Conway`-level, process-lifetime data, not session-scoped state, so
    /// `commands::execute`'s `Resume` arm carries it across the
    /// `AppState::new` reset by hand, alongside its two siblings.
    ///
    /// **`App::new`'s copy is a one-time snapshot; this field itself is no
    /// longer frozen for the rest of the process's life** (board item
    /// `01M0Y3A8MYKKE0GMYKZE1K0QTD`). `App::run`'s own event loop
    /// (`app/run.rs`'s `plugin_status_ticker` arm) calls `App::
    /// refresh_plugin_status_contributions` on a bounded cadence
    /// (`PLUGIN_STATUS_POLL_TICK`), which overwrites this field wholesale
    /// with whatever `Conway::poll_plugin_status_contributions()` returns at
    /// that moment -- a plugin whose health changes mid-session (a guard
    /// that dies, a build that finishes, a build that later FAILS) is
    /// reflected here within one tick either way, and a plugin that stops
    /// reporting entirely drops out of this field on the very next tick
    /// rather than leaving a stale value behind. See `app/plugin_status.rs`
    /// for the refresh method and its own tests, and `Conway::
    /// poll_plugin_status_contributions`'s doc for the non-blocking
    /// contract the cadence relies on.
    ///
    /// Tests in `view/status.rs` still set this field directly, matching
    /// every other `AppState` field's own test idiom in that module; the
    /// end-to-end "does a real build actually populate it at startup" proof
    /// lives in `app/startup.rs`'s own test module, the end-to-end "does a
    /// live poll actually update it" proof lives in `app/plugin_status.rs`'s
    /// own test module, and the end-to-end "does it survive `/resume`"
    /// proof lives in `app.rs`'s own test module (board item
    /// `01M0XDEDBR5YDF71Q7ZRXYMT85`) -- `/resume` still carries whatever
    /// this field CURRENTLY holds across the `AppState::new` reset, live
    /// poll or not, for the same reason it always has: the value is
    /// `Conway`-level, process-lifetime-reachable data, not something a
    /// resume should reset to empty and wait a full tick to refill.
    pub plugin_status_contributions: Vec<PluginStatusContribution>,
}

impl AppState {
    pub fn new(root: AgentId) -> Self {
        let mut tree = AgentTreeView::default();
        tree.root = Some(root);
        tree.insert(TreeNode {
            agent_id: root,
            parent: None,
            agent_def: None,
            status: NodeStatus::Starting,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        Self {
            transcript: Vec::new(),
            tree,
            last_model_decision: None,
            previous_model_decision: None,
            input: String::new(),
            cursor: 0,
            mode: Mode::Normal,
            permission_mode: PermissionMode::default(),
            declared_modes: Vec::new(),
            active_declared_mode: None,
            permission_paths: Vec::new(),
            permission_grants: Vec::new(),
            structured_allow_rules: Vec::new(),
            permission_denies: Vec::new(),
            permission_prompts: Vec::new(),
            structured_deny_rules: Vec::new(),
            structured_prompt_rules: Vec::new(),
            hook_rules: Vec::new(),
            plugin_browser: Vec::new(),
            subprocess_plugins: Vec::new(),
            mcp_plugins: Vec::new(),
            claude_compat_plugins: Vec::new(),
            plugins_open: false,
            plugins_selected: 0,
            // See the field's own doc: `None` is exactly the behaviour of
            // a build with `conway.names` uninstalled, which is what every
            // `AppState::new` caller other than `tui::run` wants.
            agent_names: None,
            permission_grant_scope: conway::PermissionScope::Session,
            scroll: 0,
            follow_tail: true,
            queued_prompts: std::collections::VecDeque::new(),
            agent_view_open: false,
            palette_selected: None,
            palette_stem: String::new(),
            agent_selected: 0,
            // V5: the default is `All`, not `ActiveOnly` -- see
            // `AgentVisibility::All`'s own doc for why hiding finished
            // agents by default reads as "agents randomly disappearing"
            // rather than as the intended "what is still running" view.
            agent_visibility: AgentVisibility::All,
            focused_agent: root,
            activity: Activity::Idle,
            focused_agent_usage: Usage::default(),
            session_head_seq: None,
            pending_ask_modal: None,
            ask_in_flight: false,
            ask_child: None,
            ask_started_at: None,
            ask_abandoned: false,
            modal_scroll: 0,
            pending_intent_confirm: None,
            pending_trust_preview: None,
            spinner_frame: 0,
            turn_started_at: None,
            turn_running_tokens: 0,
            turn_transcript_start: 0,
            focused_model: None,
            focused_model_max_context: None,
            focused_ctx_tokens: 0,
            focused_seen_segments: HashSet::new(),
            git_branch: None,
            cwd_display: None,
            status_line_config: StatusLineConfig::default(),
            tool_preview_lines: 3,
            model_max_context: HashMap::new(),
            show_reasoning: true,
            show_timestamps: false,
            history: VecDeque::new(),
            history_cap: DEFAULT_HISTORY_SIZE,
            history_index: None,
            history_draft: String::new(),
            help_open: false,
            settings_open: false,
            settings_selected: 0,
            settings_collapsed_groups: HashSet::new(),
            // empty here by default
            // (mirrors every other collection field's construction-time
            // default) -- `App::new` overwrites this immediately after
            // construction with the real, resolved `CommandRegistry::
            // palette_entries()`; see this field's own doc for why
            // `/resume` must NOT go through this default a second time.
            plugin_commands: std::sync::Arc::new(Vec::new()),
            plugin_status_contributions: Vec::new(),
        }
    }

    /// Switches the transcript pane to `agent`'s own conversation.
    /// A pure state transition: clears `transcript` and resets the scroll
    /// position back to a fresh, following view (whatever history was
    /// scrolled to for the PREVIOUS focus has no meaning for a different
    /// agent's stream) -- the actual replay is not done here. `app.rs`
    /// re-subscribes to `handle.agent_events(agent)` immediately after
    /// calling this, and that stream's own replay-then-live envelopes flow
    /// through the SAME `Self::apply` this struct already uses for the root
    /// stream (the app loop's event-arm is agnostic to which agent a given
    /// envelope's stream is scoped to), so no second `LogRecord`/`Envelope`
    /// -> `Entry` mapping is introduced here.
    ///
    /// A no-op re-focus onto the agent already focused still clears and
    /// resets exactly as any other switch would -- deliberately, not
    /// specially skipped: cheap, and correct if `app.rs`'s own replay ever
    /// changed underneath (e.g. a session resumed mid-way).
    ///
    /// V5: this clears the transcript down to `agent`'s OWN log with no
    /// lineage content mixed in, deliberately -- a spawn child's transcript
    /// must never show text from a parent it never actually saw (the
    /// fork/spawn trap; see `view/status.rs::agent_field`'s own doc). What
    /// lineage this DOES surface -- who created `agent` and how (fork/spawn,
    /// fork point, `agent_def`) -- is read straight from `self.tree` by the
    /// status line's `lineage` field (`view/status.rs::agent_field`) on
    /// every render, so nothing needs to be seeded into the transcript here.
    pub fn focus_agent(&mut self, agent: AgentId) {
        self.focused_agent = agent;
        self.transcript.clear();
        self.scroll = 0;
        self.follow_tail = true;
        // The activity/usage indicators are about whichever agent is
        // CURRENTLY focused -- a
        // freshly focused agent starts with no activity signal until its
        // own next event arrives, and no stale token figure carried over
        // from the previous focus. `app.rs` re-fetches the true cumulative
        // total via `SessionHandle::session_usage` immediately after
        // calling this (see `focused_agent_usage`'s own doc); this reset is
        // what that fetch is filling back in, not a value meant to persist
        // on its own.
        self.activity = Activity::Idle;
        self.focused_agent_usage = Usage::default();
        // T2: the spinner/elapsed/running-token state is per focused-agent --
        // a freshly focused agent has no turn in flight, so the animation
        // counters reset and the status line shows no elapsed/running tokens
        // until the new focus's own `TurnStarted` arrives.
        self.spinner_frame = 0;
        self.turn_started_at = None;
        self.turn_running_tokens = 0;
        // T4: `focus_agent` clears the transcript above, so the turn-summary
        // watermark floors at 0 for the newly focused agent.
        self.turn_transcript_start = 0;
        // T3: the model display name, max-context, and cumulative context
        // tokens are per focused-agent -- a freshly focused agent has no
        // routing decision yet and no accumulated context figure until its
        // own events arrive, so this zeroing is correct for the instant
        // `focus_agent` itself runs. It does NOT stick, though: replay
        // still does not repopulate these (`record_to_event` maps
        // a replayed `Assistant` record to `TextDelta`, never to
        // `ContextSegmentAdded` or `ModelDecision`), but T3 follow-up's
        // `app.rs::try_focus_agent` re-fetches all three authoritatively
        // right after calling this -- `SessionHandle::last_model` for the
        // serving model (reads the last `LogRecord::Assistant` directly,
        // not a live `ModelDecision`) and
        // `SessionHandle::context_report_current` for the cumulative
        // context total (falling back to the durable store when this
        // process has no live report yet -- see that method's own doc for
        // the resumed-session case) -- alongside the pre-existing
        // `session_usage` re-fetch (see `focused_agent_usage`'s own doc).
        // A freshly focused agent that has already run a turn therefore
        // shows its real model and `ctx%` immediately; only a GENUINELY
        // fresh agent (no turn anywhere yet) legitimately still shows
        // `ctx 0%` / no model, pending its own first live turn.
        self.focused_model = None;
        self.focused_model_max_context = None;
        self.focused_ctx_tokens = 0;
        self.focused_seen_segments.clear();
    }

    /// Opens the NL intent confirmation card (C2), parking it in
    /// `pending_intent_confirm` instead whenever another modal surface (a
    /// permission prompt or an `/ask` modal) currently owns `mode` --
    /// mirroring [`Self::offer_ask_modal`]'s parking behavior, so
    /// the modal-bearing surfaces never stack. `Self::promote_next_surface`
    /// opens the parked card once the surface ahead of it clears. Called
    /// by `commands::execute`'s free-text `/fork`/`/spawn` arm right after
    /// `Conway::classify_agent_intent` returns `Ok`.
    pub fn offer_intent_confirm(&mut self, card: IntentConfirm) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::IntentConfirm(card);
            // V1: see `Self::offer_ask_modal`'s own comment on the same
            // reset.
            self.modal_scroll = 0;
        } else {
            self.pending_intent_confirm = Some(card);
        }
    }

    /// Closes the intent confirmation card (C2) after a `Confirm` or
    /// `Manual` choice, promoting the next parked/queued surface via
    /// `Self::promote_next_surface`. A no-op when no card is open.
    /// `Edit` does NOT call this -- [`Self::begin_intent_confirm_edit`]
    /// drops the classified prompt into the input line and then closes the
    /// card via this same method, but with the input line populated so the
    /// user can edit and resubmit normally.
    pub fn close_intent_confirm(&mut self) {
        if !matches!(self.mode, Mode::IntentConfirm(_)) {
            return;
        }
        self.mode = Mode::Normal;
        self.promote_next_surface();
    }

    /// The `Edit` choice (C2): drops the classified `intent.prompt` into
    /// the input line (replacing whatever was there), positions the cursor
    /// at the end, and closes the card -- the user edits and submits
    /// normally. The classifier's rewrite (not the raw text) is what lands
    /// in the input line: the user picked "edit the classified version",
    /// not "edit my raw text". A no-op when no card is open.
    pub fn begin_intent_confirm_edit(&mut self) {
        if let Mode::IntentConfirm(card) = &self.mode {
            let prompt = card.intent.prompt.clone();
            self.input = prompt;
            self.cursor = self.input.chars().count();
        }
        self.close_intent_confirm();
    }

    /// Opens the `[p]` field editor from a permission prompt. Only callable
    /// while a prompt is showing (`Mode::AwaitingPermission`): the `p` key
    /// is offered only there, and only for `RenderKind::Structured` tools
    /// (where `suggested_rule` returns `Some`). The [`PendingPrompt`] is
    /// MOVED out of `mode` into [`EditingPatternState`] (it is not `Clone`),
    /// so the prompt is not lost -- cancel restores it, submit resolves it.
    /// Does not park/queue: this modal can only open from `AwaitingPermission`
    /// and returns there, so it never stacks against the other modal-bearing
    /// surfaces.
    pub fn offer_editing_pattern(&mut self) {
        if !matches!(self.mode, Mode::AwaitingPermission(_)) {
            return;
        }
        let Mode::AwaitingPermission(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            unreachable!()
        };
        self.mode = Mode::EditingPattern(EditingPatternState::from_arguments(prompt));
        self.modal_scroll = 0;
    }

    /// Cancels the field editor and returns the prompt to the screen
    /// unresolved -- the operator can press `y`/`a`/`n`/`p` again.
    pub fn cancel_editing_pattern(&mut self) {
        if !matches!(self.mode, Mode::EditingPattern(_)) {
            return;
        }
        let Mode::EditingPattern(ed) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        self.mode = Mode::AwaitingPermission(ed.prompt);
        self.modal_scroll = 0;
    }

    /// Submits the field editor: builds an `ArgsMatch` allow rule from the
    /// pinned fields, restores the prompt to `AwaitingPermission` (so the
    /// app loop's dispatch can resolve it with the existing
    /// `resolve_current_prompt` path), and returns the rule + scope for the
    /// key handler to wrap in an `Action::GrantPermissionRule`. Returns
    /// `None` if no editor is open. The grant covers FUTURE calls; THIS
    /// call is resolved separately by the dispatch arm as `AllowOnce`.
    pub fn submit_editing_pattern(&mut self) -> Option<(conway::Rule, conway::PermissionScope)> {
        if !matches!(self.mode, Mode::EditingPattern(_)) {
            return None;
        }
        let Mode::EditingPattern(ed) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        let mut pinned: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        for f in ed.fields.iter().filter(|f| f.pinned) {
            pinned.insert(f.name.clone(), f.value.clone());
        }
        let rule = conway::Rule::args_match_allow_rule(&ed.tool, pinned);
        self.mode = Mode::AwaitingPermission(ed.prompt);
        self.modal_scroll = 0;
        Some((rule, self.permission_grant_scope))
    }

    /// The single mutation entry point: applies one envelope's effect to
    /// `transcript`/`tree`. Never panics -- an event about an unknown
    /// call/agent degrades to a `Notice` rather than being dropped silently
    /// or aborting the loop (criteria: `Lagged` and unknown-parent
    /// `AgentSpawned`).
    pub fn apply(&mut self, env: &Envelope) {
        match &env.event {
            Event::Lagged { skipped } => {
                self.transcript.push(Entry::Notice {
                    text: format!(
                        "-- missed {skipped} event(s); some history may be incomplete --"
                    ),
                });
            }
            Event::AgentSpawned {
                kind,
                parent,
                agent_def,
                inherited_upto,
                ephemeral,
                ..
            } => {
                // Ephemeral `/ask`-style forks flow through
                // `apply_agent_spawned` like any other agent: they enter
                // the tree with `ephemeral: true` on their node (provenance
                // is kept, not erased); only the inline
                // `Entry::Agent` transcript push is suppressed for them
                // (inside `apply_agent_spawned`).
                self.apply_agent_spawned(
                    env.agent,
                    *kind,
                    *parent,
                    agent_def.clone(),
                    *inherited_upto,
                    *ephemeral,
                );
            }
            Event::AgentFinished { result, .. } => {
                self.apply_agent_finished(env.agent, result);
                // the focused
                // agent's own finish is the terminal "stopped working"
                // signal -- an unrelated agent (sibling/other subtree)
                // finishing must not reset an activity indicator that is
                // about the FOCUSED agent specifically.
                if env.agent == self.focused_agent {
                    self.activity = Activity::Idle;
                    // T2: a finished focused agent has no turn in flight;
                    // clear the elapsed/running-token counters so the status
                    // line shows no working indicator.
                    self.clear_turn_state();
                }
            }
            Event::AgentPromoted { .. } => {
                // B3: the event is the ONLY signal for this flip -- no
                // optimistic TUI-side flip. The facade emits it strictly
                // after BOTH the durable header rewrite and the runtime
                // tree flip have succeeded (`Conway::promote`'s failure
                // ordering), so the cached node can be flipped
                // unconditionally on receipt. An unknown agent degrades to
                // a `Notice` per `apply`'s never-panic contract (same
                // contract the unknown-parent `AgentSpawned` arm honors).
                if let Some(node) = self.tree.get_mut(env.agent) {
                    node.ephemeral = false;
                } else {
                    self.transcript.push(Entry::Notice {
                        text: format!("agent {} was promoted but is not in the tree", env.agent),
                    });
                }
            }
            // Bug 2 fix: without this arm,
            // `TurnStarted` fell into the wildcard below and the whole
            // submit->model-latency window showed `Idle` -- for a
            // non-streaming backend (one full-text `TextDelta` immediately
            // before `TurnFinished`) the `Responding` set below is coalesced
            // away entirely by the ~16ms redraw cap, so `Idle` was the ONLY
            // activity a user ever saw. Marking `Thinking` here, before any
            // delta arrives, closes that window. `ThinkingDelta`/`TextDelta`
            // still refine this to `Thinking`/`Responding` as real content
            // streams in.
            Event::TurnStarted { .. } => {
                if env.agent == self.focused_agent {
                    self.activity = Activity::Thinking;
                    // T2: a new turn for the focused agent starts the elapsed
                    // clock and resets the new-segment-token count (the
                    // previous turn's `TurnFinished` already folded its
                    // authoritative `Usage` into `focused_agent_usage`).
                    self.turn_started_at = Some(Instant::now());
                    self.turn_running_tokens = 0;
                    // T4: watermark the transcript so the turn-end summary
                    // can only attach to a block THIS turn produced (see
                    // `turn_transcript_start`).
                    self.turn_transcript_start = self.transcript.len();
                }
            }
            // This item: the SINGLE path that renders a prompt bubble now --
            // `app.rs`'s `submit`/`deliver_first_message` used to push
            // `Entry::User` locally, synchronously, before ever calling the
            // facade; they no longer do (a behavioral difference
            // between the TUI and a library consumer watching the same
            // `EventStream` is a renderer bug, and pushing locally was
            // exactly that -- a library embedder never saw the prompt at
            // all). Every prompt -- live submit, a replayed `LogRecord::
            // UserTurn` (`record_to_event`), or a focus-switch's replay
            // batch -- now reaches the transcript through this ONE arm.
            // Unconditional, matching `TextDelta`'s own convention just
            // below: `apply` is only ever fed the currently subscribed
            // agent's own stream (`SessionHandle::agent_events`/`events()`),
            // so `env.agent` is already the right agent by construction.
            Event::UserTurn { text, .. } => {
                self.transcript.push(Entry::User(text.clone()));
            }
            Event::ThinkingDelta { text } => {
                // T4: feed the reasoning-trace delta into the transcript
                // (previously only `activity` was flipped to `Thinking`).
                // Mirrors `TextDelta` -> `append_assistant_text`:
                // create-or-append an `Entry::Reasoning`, stamping the
                // serving model + envelope timestamp on a fresh entry.
                // Reasoning is EXPANDED by default (`show_reasoning`);
                // `build_lines` skips it when the flag is off.
                if env.agent == self.focused_agent {
                    self.append_reasoning_text(text, env.ts);
                    self.activity = Activity::Thinking;
                }
            }
            // This item (board: "pulling in an /ask answer wedges the status
            // bar in a working state forever"): the `turn_started_at.
            // is_some()` guard added to the `activity` write. Pulling in an
            // ask answer is a LOG operation (`Runtime::pull_in`), not an
            // agent run -- it copies the child's records into the parent's
            // log and emits synthetic "live twin" events (`Event::UserTurn`,
            // `Event::TextDelta`) so a subscriber sees the merged content
            // appear, but it never emits `Event::TurnStarted` because no
            // turn ever actually starts. Before this guard, this arm set
            // `activity = Responding` on the twin exactly like it does for a
            // real streaming reply, and nothing was left to ever clear it --
            // `Event::TurnFinished`/`Event::AgentFinished` (the only two
            // event-driven paths back to `Idle`, alongside a focus switch)
            // never fire for a pull-in, so the status bar spun forever after
            // the merge. `turn_started_at` is `Some` ONLY between
            // `Event::TurnStarted` and `Event::TurnFinished`
            // (`TurnStarted`'s own arm above; `clear_turn_state`, called
            // from the `TurnFinished` arm below) -- exactly the predicate
            // "a real turn is running" -- so a pull-in's twin, which carries
            // no such bracket, now leaves `activity` untouched instead of
            // wedging it.
            //
            // Ordering premise, traced (not assumed): a real turn's
            // `Event::TurnStarted` is unconditionally emitted (`agent_loop.
            // rs`, `AgentLoop::run_inner`) before any context assembly,
            // model call, or streamed `TextDelta` for that same turn, over
            // one shared, per-session-ordered `EventBus` (`events.rs`:
            // `emit`/`emit_pruning` hold the per-session seq mutex ACROSS
            // the broadcast `send`, specifically so no later `seq` can ever
            // be observed before an earlier one). So for a subscriber that
            // was ALREADY ATTACHED when the turn began, `TurnStarted` is
            // guaranteed to reach `apply` -- and stamp `turn_started_at` --
            // strictly before that turn's own first `TextDelta` can. The two
            // call sites that flip `activity` to `Thinking` optimistically
            // (`app.rs`'s `submit`, `app/focus.rs`'s `deliver_first_message`)
            // cannot spoof this guard either, for a simpler reason than
            // scheduling: they never stamp `turn_started_at` at all, so their
            // timing relative to `prompt_agent`'s return is irrelevant to it.
            //
            // RESOLVED (board `01M0VWMMEG4CER8Y8VH77KZ0CV`): the premise
            // above holds only for a stream subscribed BEFORE the turn
            // started -- `Event::TurnStarted` is bus-only (not a
            // `LogRecord` variant; `record_to_event` has no arm for it) so
            // it is never replayed to a subscriber that attaches later, and
            // `focus_agent` (this file) resets `turn_started_at` to `None`
            // on every switch. Focusing away from a streaming agent and
            // back mid-turn used to attach a fresh stream that missed that
            // turn's `TurnStarted`, leaving `activity` at `Idle` and the
            // streaming cursor off for the remainder of that turn. Fixed at
            // the FACADE, not here: `App::try_focus_agent` (`app/focus.rs`)
            // now seeds both `turn_started_at` and `activity` itself, right
            // after `focus_agent`'s reset, from `SessionHandle::
            // turn_in_progress` -- a `conway-runtime::AgentTree`-backed
            // query answering "is a turn in flight for this agent right
            // now" that is NOT `NodeStatus::Running` (which cannot tell an
            // idle keep-alive root from one mid-turn, and would reinstate
            // the wedge this guard exists to prevent -- see that method's
            // own doc). This arm's own gate is unchanged: it still only
            // ever gets to `Some` between a real `TurnStarted` and
            // `TurnFinished`, whichever subscriber first observes it.
            //
            // Sibling closed for free, verified (`record_to_event`,
            // `conway/src/session_handle.rs`): a `--resume`/focus-switch
            // replay batch whose last content record is an assistant reply
            // maps that record to this same `Event::TextDelta` shape and
            // likewise never synthesizes a `Event::TurnStarted` -- and
            // `AppState::focus_agent` (`state.rs`, this file) resets BOTH
            // `activity` to `Idle` AND `turn_started_at` to `None` before
            // the replay batch is ever applied. So a replayed assistant
            // reply now leaves `activity` at that reset `Idle` instead of
            // wedging it into `Responding` the same way pull-in used to.
            Event::TextDelta { text } => {
                self.append_assistant_text(text, env.ts);
                if env.agent == self.focused_agent && self.turn_started_at.is_some() {
                    self.activity = Activity::Responding;
                }
            }
            // T2/T3: accumulate the focused agent's context-token figures
            // from context-segment additions. Two accumulators share this
            // arm:
            // - `turn_running_tokens` (T2): the per-turn "added this turn"
            //   figure, reset on `TurnStarted`/`focus_agent`. Accumulated
            //   only while a turn is in flight (`turn_started_at.is_some()`).
            // - `focused_ctx_tokens` (T3): the CUMULATIVE context-occupancy
            //   estimate across the focused session (NOT reset per turn),
            //   the numerator for the status line's `ctx%` field.
            //   Accumulated for every segment-add on the focused agent's
            //   stream regardless of turn state, GATED on
            //   `focused_seen_segments.insert(segment)` so a repeated
            //   segment id (e.g. a non-keep-alive child's fresh
            //   `AgentLoop` re-emitting its existing context on the first
            //   turn of a new run) is counted once, not re-added every
            //   run. `turn_running_tokens` is NOT deduped -- it is a
            //   per-turn "what fired this turn" figure, so a re-emitted
            //   segment legitimately counts toward the turn that re-saw
            //   it.
            Event::ContextSegmentAdded {
                segment,
                tokens_est,
                ..
            } => {
                if env.agent == self.focused_agent {
                    if self.turn_started_at.is_some() {
                        self.turn_running_tokens = self
                            .turn_running_tokens
                            .saturating_add(u64::from(*tokens_est));
                    }
                    if self.focused_seen_segments.insert(*segment) {
                        self.focused_ctx_tokens = self
                            .focused_ctx_tokens
                            .saturating_add(u64::from(*tokens_est));
                    }
                }
            }
            // T3: capture the focused agent's serving model display name
            // (`ModelRef::to_string()`, e.g. `anthropic/claude-sonnet-4-6`)
            // and look up its max context window from the model-metadata
            // map populated at `App::new`. The status line's `model` field
            // renders the display name; `ctx%` divides `focused_ctx_tokens`
            // by this max. `app.rs` already captures the whole
            // `ModelDecision` envelope for `/why` (`last_model_decision`),
            // but that field is intentionally left untouched by `apply`
            // -- this arm only updates the display-name/max-context
            // pair on the focused agent's own stream.
            Event::ModelDecision { chosen, .. } => {
                if env.agent == self.focused_agent {
                    let name = chosen.to_string();
                    let max = self.model_max_context.get(&name).copied().or_else(|| {
                        // Fall back to a bare `model` lookup (no
                        // backend prefix) -- some metadata files key
                        // on the model id alone.
                        self.model_max_context.get(chosen.model.as_str()).copied()
                    });
                    self.focused_model = Some(name);
                    self.focused_model_max_context = max;
                }
            }
            Event::ToolCallProposed {
                call_id,
                tool,
                args,
                ..
            } => {
                // T4: store the call's `args` (previously discarded via
                // `..`). Serialized to a compact JSON string at apply time
                // (a `serde_json::Value` is not `Clone`-cheap to keep on the
                // entry, and the renderer only needs a string anyway). A
                // non-serializable value is impossible for valid JSON, so
                // `to_string` cannot panic on real input; on the empty
                // object it yields `"{}"`.
                self.transcript.push(Entry::Tool {
                    call_id: call_id.clone(),
                    name: tool.to_string(),
                    status: ToolStatus::Proposed,
                    preview: String::new(),
                    args: args.to_string(),
                    progress: String::new(),
                    expanded: false,
                    ts: Some(env.ts),
                });
                self.set_tree_status(env.agent, NodeStatus::Running);
                if env.agent == self.focused_agent {
                    self.activity = Activity::RunningTool(tool.to_string());
                }
            }
            Event::PermissionRequested { call_id, .. } => {
                self.set_tool_status(call_id, ToolStatus::AwaitingPermission);
                self.set_tree_status(env.agent, NodeStatus::AwaitingPermission);
                if env.agent == self.focused_agent {
                    self.activity = Activity::AwaitingPermission;
                }
            }
            Event::TurnFinished { usage, .. } => {
                // the live-increment
                // half of the token counter (immediate feedback) -- see
                // `focused_agent_usage`'s own doc for why `app.rs`'s
                // authoritative `session_usage` refetch still overwrites
                // this afterward.
                if env.agent == self.focused_agent {
                    // T4: stamp the turn-end summary (`1m 6s · 1.4k tok
                    // (88% cached)`) onto the last Assistant or Reasoning
                    // block BEFORE `clear_turn_state` zeroes
                    // `turn_started_at` (which the elapsed figure reads).
                    self.stamp_turn_summary(usage);
                    self.activity = Activity::Idle;
                    self.focused_agent_usage += *usage;
                    // T2: the turn is over -- stop the elapsed clock and drop
                    // the running-estimate counter (the authoritative `Usage`
                    // is now folded into `focused_agent_usage` above).
                    self.clear_turn_state();
                }
            }
            Event::PermissionResolved { call_id, decision } => {
                // `AllowOnce`/`AllowAlways`/`Cached` resolutions don't get a
                // dedicated status here -- `ToolCallStarted`/
                // `ToolCallFinished` carry the outcome for an approved call.
                // A denial has no further event for that call, so it needs
                // its own visible note.
                use conway::PermissionDecisionKind as Kind;
                if matches!(decision, Kind::Denied | Kind::DeniedWithFeedback) {
                    self.transcript.push(Entry::Notice {
                        text: format!("tool call {call_id} denied"),
                    });
                }
            }
            Event::ToolCallStarted { call_id } => {
                self.set_tool_status(call_id, ToolStatus::Running);
            }
            // T4: append the progress note to the matching in-flight
            // `Entry::Tool` by `call_id` (previously dropped by the wildcard
            // arm). Rendered as a dim `-> {note}` line between the args line
            // and the output block. A no-op if no matching tool entry exists
            // (e.g. a progress event for a call whose `ToolCallProposed` was
            // never seen -- never panics on untrusted input).
            Event::ToolProgress { call_id, note } => {
                self.append_tool_progress(call_id, note);
            }
            Event::ToolCallFinished {
                call_id,
                is_error,
                preview,
            } => {
                self.finish_tool(call_id, *is_error, preview.clone());
            }
            Event::BackendDegraded { .. } => {
                self.transcript.push(Entry::Notice {
                    text: "backend degraded".to_string(),
                });
            }
            //: was pushed as an
            // `Entry::Notice`, rendering `theme.notice`'s cyan regardless of
            // `fatal` -- a genuine fatal runtime error looked identical to
            // "backend degraded". Now a dedicated `Entry::Error`, styled by
            // severity in `entry_lines` (see that variant's doc). The
            // `"fatal "` text prefix is kept even though severity is now
            // carried structurally by the `fatal` field: `entry_lines`'s
            // clean-copy guarantee means a copied transcript carries no
            // style/color at all, so the word is the only trace of severity
            // that survives a copy-paste.
            Event::Error { error, fatal } => {
                self.transcript.push(Entry::Error {
                    text: format!("{}error: {error}", if *fatal { "fatal " } else { "" }),
                    fatal: *fatal,
                });
            }
            // review fix (finding 1, CRITICAL): this used to fall
            // into the wildcard arm below, silently dropped. Two producers
            // rely on this now being visible: `record_to_event`'s replay
            // mapping (`UserTurn`/`ForkDirective`/`ParentSteer`/
            // `SystemNote`/`ContextReportRecord` all synthesize an
            // `AgentProgress{note}` on replay -- a focus-switched
            // transcript with no arm for it showed only tool/lifecycle
            // activity, none of the actual user turns), and the LIVE agent
            // loop (`conway-runtime`'s `agent_loop.rs`, e.g. steering
            // notes), which already emits real `AgentProgress` envelopes
            // that were equally invisible before this fix -- an accepted,
            // reasonable improvement, not a scope change (they carry
            // genuine free-text informational content either way).
            Event::AgentProgress { note } => {
                self.transcript.push(Entry::Notice { text: note.clone() });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
pub(super) mod fixtures {
    use conway::SessionId;

    use super::*;

    pub(super) fn envelope(session: SessionId, agent: AgentId, event: Event) -> Envelope {
        Envelope {
            seq: 0,
            ts: chrono::Utc::now(),
            session,
            agent,
            event,
        }
    }

    pub(super) fn spawned(parent: Option<AgentId>) -> Event {
        Event::AgentSpawned {
            kind: SubagentMode::Spawn,
            parent,
            agent_def: None,
            inherited_upto: None,
            ephemeral: false,
        }
    }
}

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
use conway::backend_usability::Usability;
use conway::config::schema::BackendEntry;
use conway::plugin::PluginStatusContribution;
use conway::{
    AgentId, AgentIntent, AgentResult, Envelope, Event, LogSeq, ModelRef, PermissionDecisionKind,
    PermissionMode, ResultStatus, RoleAlias, RoutingReason, SegmentId, SessionId, SubagentMode,
    Usage,
};

use super::config::StatusLineConfig;
use super::form::PendingFormAsk;
use super::gate::PendingPrompt;

mod agent_panel;
mod agent_tree;
mod busy_input;
mod editor_mode;
mod input_line;
mod mentions;
mod modal;
mod scroll;
mod status;
mod transcript;
mod turn_summary;

pub use agent_panel::AgentVisibility;
pub use agent_tree::{AgentTreeView, NodeStatus, SpawnRoleOrModel, TreeNode};
pub use busy_input::BusyInputMode;
pub use editor_mode::EditorMode;
pub use input_line::{clamp_history_size, DEFAULT_HISTORY_SIZE};
pub use mentions::MentionScanRequest;
pub use modal::{
    AddProviderContextWindowState, AddProviderCredentialState, AskFate, AskModal,
    DenyFeedbackState, DistillFate, DistillModal, Mode, SettingsPreviewSection, SkillProposalFate,
    SkillProposalModal, TrustDecision, TrustPreviewCard, UiFormDecision, UiFormState,
    DEFAULT_DENY_FEEDBACK,
};
pub use status::{should_animate, Activity, SPINNER_FRAMES};
pub use transcript::{backfill_entries, clamp_tool_preview_lines, Entry, ToolStatus};

/// Board item A1d: how many `Event::ModelDecision` envelopes
/// [`AppState::model_decision_history`] retains, oldest dropped first --
/// enough for `/why` to show a genuinely useful session history (INTENT.md
/// §5c's own example session saw six flips) without an unbounded session
/// growing this field forever.
pub const MODEL_DECISION_HISTORY_CAP: usize = 10;

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

    /// Board item `01M331DN5YF9J1T12QRASZCHV7`: the rule THIS editor's
    /// current field state would submit right now, byte-identical to what
    /// [`AppState::submit_editing_pattern`] actually builds --
    /// same `pinned` filter, same field order (`BTreeMap` sorts by key, so
    /// the fields' own on-screen order does not matter), same
    /// [`conway::Rule::args_match_allow_rule`] constructor. Read by the
    /// view to render [`conway::Rule::describe`]'s own human-readable
    /// summary ("any `tool` call" / "`tool` with path=… pinned") BEFORE
    /// `Enter`, not just after -- the disclosure this board item exists to
    /// add. A pure read: does not consume or mutate `self`, unlike the
    /// submit path it mirrors, so the view can call it every frame.
    pub fn preview_rule(&self) -> conway::Rule {
        let mut pinned: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        for f in self.fields.iter().filter(|f| f.pinned) {
            pinned.insert(f.name.clone(), f.value.clone());
        }
        conway::Rule::args_match_allow_rule(&self.tool, pinned)
    }
}

/// Board item `01M32EBPWZZG6EA77ZG5KYC8KQ`: the state carried by
/// [`Mode::EditingShellPrefix`] -- the session-scoped shell-prefix grant
/// editor, opened from `AwaitingPermission` for a `RenderKind::
/// ShellCommand` prompt (mirroring [`EditingPatternState`]'s "prompt moved
/// out of `AwaitingPermission`, since [`PendingPrompt`] is not `Clone`"
/// shape exactly -- cancel restores it, submit resolves it). Unlike the
/// `[p]` field editor's per-field pin/wildcard rows, this is a single
/// free-text line the operator can edit character by character: `input`/
/// `cursor` follow the identical char-index convention
/// [`crate::tui::state::modal::DenyFeedbackState::input`]/`cursor` and
/// [`crate::tui::state::modal::AddProviderCredentialState::input`]/`cursor`
/// already use. `input` starts at [`conway::permission_pattern::
/// default_shell_prefix`]'s own proposal for the pending call's rendered
/// text -- narrow by construction (two tokens, never a bare program name)
/// -- and the operator edits from there; nothing installs it un-reviewed.
///
/// The grant scope is NOT carried here, mirroring [`EditingPatternState`]'s
/// own doc on the identical point: it lives on [`AppState`] as
/// `permission_grant_scope` (cycled by `Ctrl-S` while this editor is open,
/// the SAME field the prompt's own `s` key and the `[p]` field editor's `s`
/// key cycle), so every remembered-grant surface shares one scope source.
///
/// `error` is set by [`AppState::submit_editing_shell_prefix`] when
/// [`conway::permission_pattern::shell_command_is_compound`] refuses the
/// (trimmed) `input` -- the editor stays open with the reason shown,
/// mirroring [`crate::tui::state::modal::AddProviderCredentialState::
/// error`]'s own "a rejected attempt is shown, never silently re-prompted"
/// contract, rather than silently discarding the keystroke or installing a
/// grant nothing could actually cover.
pub struct EditingShellPrefixState {
    pub prompt: PendingPrompt,
    pub input: String,
    pub cursor: usize,
    pub error: Option<String>,
}

impl std::fmt::Debug for EditingShellPrefixState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditingShellPrefixState")
            .field("input", &self.input)
            .field("cursor", &self.cursor)
            .field("error", &self.error)
            .finish()
    }
}

impl PartialEq for EditingShellPrefixState {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input && self.cursor == other.cursor && self.error == other.error
    }
}

impl EditingShellPrefixState {
    /// Seeds `input` from [`conway::permission_pattern::
    /// default_shell_prefix`] applied to the pending call's own rendered
    /// text -- the operator sees this exact proposal and can widen or
    /// narrow it before accepting; nothing here installs a grant.
    ///
    /// `error` is pre-populated (rather than left `None` for the operator
    /// to discover only on `Enter`) on the rare candidate whose FIRST TWO
    /// WHITESPACE-DELIMITED TOKENS already embed a compound construct with
    /// no surrounding space -- e.g. `git;rm -rf /` tokenizes to
    /// `["git;rm", "-rf", ...]`, so the two-token default itself is
    /// `git;rm -rf` and `shell_command_is_compound` correctly refuses it.
    /// This is NOT the common case (an ordinary `&&`/`|`/`;` a few tokens
    /// further into the command, e.g. `git status && rm -rf /`, seeds a
    /// perfectly fine `git status` default -- the compound construct sits
    /// past the two tokens this function ever looks at) but it is a real
    /// one, and the operator should not have to press `Enter` once to
    /// discover a proposal this editor already knew was unusable.
    pub fn from_prompt(prompt: PendingPrompt) -> Self {
        let input = conway::permission_pattern::default_shell_prefix(&prompt.request.rendered);
        let cursor = input.chars().count();
        let error = if conway::permission_pattern::shell_command_is_compound(&input) {
            Some(compound_prefix_refusal_message().to_string())
        } else {
            None
        };
        Self {
            prompt,
            input,
            cursor,
            error,
        }
    }
}

/// The refusal message shown by both [`EditingShellPrefixState::
/// from_prompt`] (a pre-populated default) and [`AppState::
/// submit_editing_shell_prefix`] (a submitted `Enter`) -- one wording, so
/// the operator sees the identical explanation regardless of which path
/// triggered it.
fn compound_prefix_refusal_message() -> &'static str {
    // Deliberately does NOT quote the refused text back at the operator --
    // they can already see it, unchanged, in the input line directly
    // above this message; quoting it here would cost this short, one-row
    // message the width budget `draw_editing_shell_prefix`'s own doc
    // explains it needs to stay visible without a scroll.
    "refused -- a grant covers one simple command only, nothing chained, piped, or substituted"
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

/// One row of [`AppState::role_listing`] -- board item
/// `01M24ZJ9ABPP0DGVAA2PS3XVDD`: a role name, its currently configured
/// `chain`, and whether it is `conway::config::is_baked_in_role_floor`'s
/// baked-in `"default"` floor rather than something an operator declared
/// (see that field's own doc for why bare `/role` needs the floor
/// included, labelled, rather than excluded the way `known_role_names`
/// excludes it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleListingEntry {
    pub name: String,
    pub chain: Vec<String>,
    pub builtin: bool,
}

/// Drops every session-scoped permission grant held by the process's one
/// shared `PermissionBroker`. Required by [`AppState::reset_for_new_session`]
/// so adopting a different session cannot reset the TUI's mirrors of those
/// grants while leaving the live grants in force.
pub trait RevokeSessionGrants {
    fn revoke_all_session_scoped_grants(&self);
}

impl RevokeSessionGrants for conway::Conway {
    fn revoke_all_session_scoped_grants(&self) {
        conway::Conway::revoke_all_session_scoped_grants(self);
    }
}

/// The TUI's whole render model. Every mutation goes through [`Self::apply`]
/// (event-driven) or the app loop's direct field writes for input-driven
/// state (`input`, `mode`, `scroll`) -- see `input.rs`/`app.rs`.
pub struct AppState {
    pub transcript: Vec<Entry>,
    pub tree: AgentTreeView,
    /// Board item A1d ("say why a turn fell back"): every `Event::
    /// ModelDecision` envelope this session has seen, oldest first, capped
    /// at [`MODEL_DECISION_HISTORY_CAP`] entries -- what lets `/why`
    /// (`commands::render_why`) show a short SESSION history of routing
    /// changes ("mid-session model changes are ordinary") rather than only
    /// the latest decision. Populated by `app.rs`'s run loop on
    /// `Event::ModelDecision`; `apply` intentionally leaves it untouched so
    /// it stays pure -- the same split the single-slot `last_model_decision`
    /// field this replaced always used.
    ///
    /// **Supersedes the former `last_model_decision`/
    /// `previous_model_decision` pair** (a fixed two-slot cache -- the
    /// newest decision and the one immediately before it, nothing older):
    /// both were reachable ONLY through `render_why`, so widening the
    /// question from "what changed last" to "what changed this session"
    /// meant replacing the pair rather than adding a third field beside
    /// them. `back()` is today's `last_model_decision`;
    /// `iter().rev().nth(1)` is today's `previous_model_decision`.
    pub model_decision_history: VecDeque<Envelope>,
    pub input: String,
    /// Cursor position within `input`, as a *char* index (not byte offset)
    /// -- `input.rs` translates to a byte offset via `char_indices` before
    /// touching the `String`, so this never lands mid-UTF-8-character.
    /// Always in `0..=input.chars().count()`.
    pub cursor: usize,
    pub mode: Mode,
    /// Board item `01M1YVJNS575YN5DCQG9BKZR4E`: the vim prompt-editing
    /// engine's own transient state (current submode, pending operator/
    /// count, the undo stack, the unnamed register, ...) -- live only while
    /// [`Self::editor_mode`] is [`EditorMode::Vim`]; `emacs` mode never
    /// reads or mutates this field at all. See `crate::tui::input::vim`'s
    /// own module doc for the engine itself, and `state::editor_mode`'s for
    /// why this is RESET (never carried) across `/new`/`/resume` while
    /// `editor_mode` itself is carried.
    pub vim: crate::tui::input::vim::VimState,
    /// V2: the active permission mode, mirrored from the runtime broker so
    /// the status line can render it every frame without reaching across
    /// the facade per draw. Updated when `/settings` changes it.
    ///
    /// Mirrored rather than owned: the broker is the authority (it is what
    /// actually gates calls); this is a display copy. If the two ever
    /// disagree the broker wins, and the visible consequence is a stale
    /// label -- which is why `/settings` writes both together.
    pub permission_mode: PermissionMode,
    /// V2b (board item 01M1YVP3FDPHY4WZ72SXMWAN2D): the mode this session
    /// STARTED in -- the resolved, trust-gated `permissions.default_mode`
    /// (or `--default-permission-mode`) `App::new` computed once, before
    /// the first `Shift-Tab`/`/settings` change to [`Self::permission_mode`]
    /// could ever happen. Never mutated after `App::new` sets it: this is
    /// what `/settings -> permissions` shows BESIDE the current, cycling
    /// [`Self::permission_mode`] row so the two are never confused (a
    /// standing surface ruling -- see `App::new`'s own doc for the
    /// precedence/trust computation).
    pub default_permission_mode: PermissionMode,
    /// V2b: every permission-file candidate `App::new` considered, in
    /// precedence order (project first, then every operator-authored
    /// candidate -- `crate::config::discovery::permission_file_paths`'s own
    /// construction). `.first()` is specifically the PROJECT-scope file --
    /// `/trust permissions` (`tui::commands`) addresses exactly that entry,
    /// since it is the one candidate that ever NEEDS an explicit trust
    /// decision. Resolved once at `App::new`.
    ///
    /// **Not where a newly-granted pattern is persisted** (board item
    /// `01M3TJQGJHFFPWE2YYN60WN1XB`, superseding this field's former role):
    /// see [`Self::grants_path`] for that -- a REMEMBERED grant answer must
    /// never land in the project's own `permissions.json`, so the write
    /// path and this read/addressing list diverged on purpose.
    pub permission_paths: Vec<std::path::PathBuf>,
    /// Board item `01M3TJQGJHFFPWE2YYN60WN1XB`: where a REMEMBERED
    /// permission grant (the TUI's `a`/`p` prompt answers, `PermissionScope::
    /// Session`) is actually written --
    /// `conway::config::discovery::user_scope_project_permissions_path`'s
    /// file, the operator's own user-scope, project-keyed grants store,
    /// never the project's own `.conway/permissions.json`. `None` only when
    /// that function itself cannot resolve a config directory at all, in
    /// which case a grant applies to the session but is not written
    /// anywhere -- the same "best-effort, never fails the grant itself"
    /// posture `persist_permission_rule`'s own doc already takes for every
    /// other failure on this path.
    pub grants_path: Option<std::path::PathBuf>,
    /// Board item `01M3TJQGJHFFPWE2YYN60WN1XB`: `true` for the life of the
    /// session whenever `App::new` saw a
    /// `conway::config::WarningCode::UntrustedProjectConfigIgnored` entry
    /// among `Conway::warnings()` -- an untrusted (or trusted-then-edited)
    /// project `settings.json` was ignored at startup. Drives the status
    /// line's persistent `project config ignored` marker
    /// (`tui::view::status`'s own `StatusLineField::Trust`) -- the "ignoring
    /// must never be silent" half board item `01M3TJQGJHFFPWE2YYN60WN1XB`'s
    /// own ruling asks for BEYOND the one-time transcript notice
    /// (`Conway::warnings()` already renders
    /// into the transcript unconditionally; this is what keeps the fact
    /// visible for the rest of the session, not just at the moment it
    /// scrolled past). Never re-checked after startup: an operator who
    /// trusts the project mid-session restarts to apply it anyway (this
    /// module's own established "restart to apply" precedent), so there is
    /// no mid-session transition this would need to track.
    pub project_config_ignored: bool,
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
    /// Board item `01M350FR4SM6QT0EM6M35EY5AZ`: the active session-scoped
    /// shell-prefix grants (board item `01M32EBPWZZG6EA77ZG5KYC8KQ`),
    /// mirrored from the broker's `active_shell_prefix_grants()` for the
    /// review list this class was missing since it shipped -- an operator
    /// who granted three prefixes over a long day had no list, no count,
    /// and no revoke, only memory. Rendered in the SAME allow section as
    /// [`Self::permission_grants`]/[`Self::structured_allow_rules`] and
    /// likewise REVOCABLE, addressed by its own `(prefix, scope)` pair
    /// through `Conway::revoke_shell_prefix_grant` (this class has no
    /// `PatternRule`/`Rule` identity to reuse either flat revoke's key).
    /// Refreshed alongside `permission_grants` when `/settings` opens and
    /// after any revoke.
    pub shell_prefix_grants: Vec<(String, conway::GrantScope)>,
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
    /// offered before this scope existed), `Agent` (only the agent whose call is
    /// being asked about), or `AgentSubtree` (that agent's whole subtree).
    /// Cycled by the prompt's `s` key (`input.rs::handle_permission_key`),
    /// rendered by `view/mod.rs::draw_permission_overlay`, and reset to
    /// `Session` every time a NEW prompt becomes the active one (see
    /// [`Self::offer_prompt`]/`Self::promote_next_surface`) -- a scope
    /// chosen for one call must never silently carry over to the next,
    /// exactly the same reason `modal_scroll` resets per surface.
    pub permission_grant_scope: conway::PermissionScope,
    /// Board item `01M44PK089DF2M9TM3C4P5CKMZ` (operator ruling, typeahead):
    /// when the CURRENTLY shown permission prompt first became visible, and
    /// whether [`Self::input`] already held a draft at that exact instant --
    /// armed by [`Self::offer_prompt`]/`Self::promote_next_surface` the
    /// moment a NEW prompt is promoted (never re-armed by a round trip
    /// through the pattern/shell-prefix editor back to the SAME prompt --
    /// those are not a fresh ambush). `input.rs`'s `handle_permission_key`
    /// is the one reader: a key arriving while still inside the window this
    /// pair defines is typing, not a decision -- see that function's own
    /// doc for the exact window widths and why a mid-draft arrival gets a
    /// longer one. `None` is not meaningfully distinct from "the window has
    /// long since closed" to that reader, so nothing besides a fresh arm
    /// ever needs to clear it back to `None`.
    pub permission_prompt_armed_at: Option<(std::time::Instant, bool)>,
    /// Board item `01M44PK089DF2M9TM3C4P5CKMZ` (operator ruling, "no single
    /// stray key can grant beyond once"): `true` while the FIRST press of
    /// the prompt's `allow_always` key is waiting for a deliberate
    /// confirming second keystroke (the same key again, or `Enter`) before
    /// the broader-than-once grant actually commits -- `input.rs`'s
    /// `handle_permission_key` is the one reader/writer; `view/mod.rs`'s
    /// `draw_permission_overlay` renders the confirm prompt while this is
    /// `true`. Reset to `false` whenever a NEW prompt is armed
    /// ([`Self::offer_prompt`]/`Self::promote_next_surface`) -- an armed
    /// confirm belongs to the specific prompt that armed it, never carries
    /// to a different one.
    pub permission_confirm_always: bool,
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
    /// Whether the below-chat agent-tree panel is
    /// currently shown. Toggled by `/agents` (handled in `app.rs`;
    /// `commands.rs` owns no such
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
    /// **Why this field exists at all.** Nothing in the
    /// TUI used to show an operator any `LogSeq` at all -- `Envelope::seq` (the
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
    /// Board item `01M19NH39AE2D5AMJK0RZRQY86`: an `ask_question` question
    /// parked behind another modal-bearing surface -- mirrors
    /// `pending_trust_preview` exactly. `App::run`'s `form_rx.recv()` arm
    /// calls [`Self::offer_ui_form`], which parks here whenever `mode` is
    /// not `Normal`. Drained in the SAME fixed priority order
    /// [`Self::promote_next_surface`] already documents (queued prompt,
    /// then ask, then intent card, then trust preview, then this).
    pending_ui_form: Option<PendingFormAsk>,
    /// Slice 2 (board item `01M3DTT078W25MD2S4527R0WAV`): a skill proposal
    /// parked behind another modal-bearing surface -- mirrors
    /// `pending_ui_form` exactly. `tui/app/skill_propose.rs`'s own
    /// `SkillProposeDone` arm calls [`Self::offer_skill_proposal`], which
    /// parks here whenever `mode` is not `Normal`. Drained in the SAME
    /// fixed priority order [`Self::promote_next_surface`] already
    /// documents (queued prompt, ask, intent card, trust preview, ui form,
    /// then this -- lowest priority of all seven).
    pending_skill_proposal: Option<SkillProposalModal>,
    /// Slice 2: whether `/conway.skills.propose`'s (or the automatic
    /// trigger's) ephemeral fork is currently in flight -- mirrors
    /// `ask_in_flight` exactly: while set, a second proposal is refused with
    /// a `Notice` rather than competing for the one [`Mode::SkillProposal`]
    /// slot. Set by `commands::execute`'s `SlashCommand::SkillsPropose` arm
    /// (or the automatic trigger's own call site), cleared when
    /// `SkillProposeDone` arrives.
    pub skill_propose_in_flight: bool,
    /// `/distill` (board item `01M1YVKQ6ABQDWYSA7CEF20WKG`): a briefing
    /// parked behind another modal-bearing surface -- mirrors
    /// `pending_skill_proposal` exactly. `tui/app/distill.rs`'s own
    /// `DistillDone` arm calls [`Self::offer_distill`], which parks here
    /// whenever `mode` is not `Normal`. Drained in the SAME fixed priority
    /// order [`Self::promote_next_surface`] already documents (queued
    /// prompt, ask, intent card, trust preview, ui form, skill proposal,
    /// then this -- lowest priority of all eight).
    pending_distill: Option<DistillModal>,
    /// Whether `/distill`'s ephemeral fork is currently in flight -- mirrors
    /// `skill_propose_in_flight` exactly: while set, a second `/distill` is
    /// refused with a `Notice` rather than competing for the one
    /// [`Mode::Distill`] slot. Set by `commands::execute`'s
    /// `SlashCommand::Distill` arm, cleared when `DistillDone` arrives.
    pub distill_in_flight: bool,
    /// Board item `01M1YVKQ6ABQDWYSA7CEF20WKG` (review finding, round 2):
    /// the generation of the ONE `/distill` fork whose eventual
    /// `DistillDone` reply (`tui::app::distill`, private to that module) is
    /// actually wanted. **Replaces a former `distill_abandoned: bool`**,
    /// which a second
    /// `/distill` started right after a `Ctrl-C` abandon could not
    /// distinguish from the first: resetting the bare flag at the second
    /// `/distill`'s own start would have let the FIRST fork's late reply
    /// (arriving after the second one started, but before it finishes) open
    /// as though it were current, while leaving it set would have dropped
    /// the SECOND fork's own genuine reply.
    ///
    /// `commands::execute`'s `SlashCommand::Distill` arm increments this
    /// (`wrapping_add(1)`) the moment it starts a fork, alongside setting
    /// `distill_in_flight`; `App::spawn_distill` reads the value at spawn
    /// time and threads it through to `DistillDone::generation`;
    /// `App::abandon_distill` ALSO increments it (so an abandoned fork's
    /// eventual reply is stale even if the operator never starts a second
    /// `/distill` at all); `App::apply_distill_done` compares `done.
    /// generation` against the CURRENT value of this field FIRST, before
    /// touching `distill_in_flight` or anything else -- a mismatch means a
    /// newer `/distill` (or an abandon) has already moved past the fork that
    /// produced this reply, so it is dropped silently, leaving whatever the
    /// current generation's own in-flight/modal state already is untouched.
    /// `wrapping_add` rather than a checked increment: this is a monotonic
    /// tag compared only for equality, never ordered, so wraparound after
    /// `u64::MAX` abandons in one process is harmless.
    pub distill_generation: u64,
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
    /// The set of agents the OPERATOR (not the model) currently has an
    /// `/await` outstanding on -- one waiter per agent from this surface
    /// (`commands::execute`'s `SlashCommand::Await` arm refuses a second
    /// `/await` on an agent already in this set, rather than queueing or
    /// silently replacing the first waiter). Populated the moment `/await`
    /// is accepted (the same submit-time stamping `ask_in_flight`'s own doc
    /// describes for `/ask`), removed once the spawned wait task's result
    /// arrives (`App`'s own `await_rx.recv()` arm, mirroring `plugin_cmd_rx`'s
    /// shape -- see `app/await_cmd.rs`'s module doc). Deliberately NOT
    /// `Mode`-scoped and NOT keyed to `focused_agent`: an `/await` outlives
    /// whatever the operator focuses next, so its completion notice must
    /// still land in the transcript regardless of what is on screen when it
    /// arrives -- this set exists only to refuse a duplicate `/await`, never
    /// to gate whether/where the notice is shown.
    pub awaiting_agents: HashSet<AgentId>,
    /// Board item `01M1YVFRPH0DCE8N0DR5BS5BRT` ("Run a shell command
    /// yourself"): whether a `!`-prefixed operator-typed shell command is
    /// currently running. Mirrors `ask_in_flight`'s own shape: set at
    /// submit time (`App::submit`, before the spawn), cleared when
    /// `tui::app::shell_cmd::ShellDone` arrives -- while set, a second `!`
    /// is refused with a `Notice` rather than racing two child processes
    /// for the one `Ctrl-C`-kill target `App::handle_ctrl_c` reads
    /// (`App::shell_cancel_tx`, a plain field, not `AppState`'s -- see that
    /// field's own doc for why it lives on `App`, not here).
    pub shell_in_flight: bool,
    /// The most recently RUN `!`/`!>` command (text with no leading `!`,
    /// and whether it was the `!>` to-model form), for a bare `!!` to
    /// repeat. `None` until the first `!` command runs; never set by `!!`
    /// itself (repeating does not change what "last" means) or by `!`/`!>`
    /// alone (both "run nothing" -- see `tui::app::shell_cmd`'s own parser
    /// doc). Set even when the command was refused by a `deny` rule: the
    /// operator typed it, and `!!` repeating the SAME refused command is
    /// more honest than silently repeating something else or nothing.
    pub last_shell_command: Option<(String, bool)>,
    /// Board item A5.6: every agent (root or child) this session has
    /// observed cross 80% of one of its OWN budget dimensions
    /// (`Event::BudgetWarning`), so the `/agents` panel can mark the row --
    /// the acceptance criterion this field exists for. A SEPARATE side-table
    /// from `AgentTreeView`/`TreeNode` (mirrors `agent_names`' own "alongside,
    /// never a `TreeNode` field" shape, `view/agents.rs`'s `agent_name`
    /// doc) rather than a new `TreeNode` field: `TreeNode` is constructed by
    /// plain struct literal at every one of this crate's own call sites (no
    /// `Default` impl, deliberately -- every field is meant to be an
    /// explicit choice at each construction site), so adding a field there
    /// would touch every one of them for a marker only THIS event ever
    /// sets. Once an agent crosses, it stays marked for the rest of the
    /// session -- the crossing already happened and stays a true fact about
    /// that agent's run even after it later finishes; nothing clears an
    /// entry.
    pub budget_warned_agents: HashSet<AgentId>,
    /// Board item A1d ("say why a turn fell back"), the "switch-forks do not
    /// pile up" half: every `/model`/`/role` switch's OWN child, mapped to
    /// the agent it replaced -- `switch_session`'s (`commands.rs`) one write
    /// site, right after the fork it just made succeeds. A side-table
    /// alongside `AgentTreeView`/`TreeNode`, mirroring `budget_warned_
    /// agents`' own reasoning just above (`TreeNode` has no `Default`, so a
    /// marker only ONE call site ever sets does not belong on it): the
    /// runtime's own fork/spawn machinery has no notion of "this fork was
    /// PURELY a model/role switch" at all (a `/model` switch and a plain
    /// `/fork` build byte-identical `ForkSpec`s save for the `.model`/
    /// `.role` override -- see `switch_session`'s own doc), so this fact
    /// exists ONLY here, in the one place that actually decided to make the
    /// switch. [`Self::is_switch_replaced`] and
    /// [`Self::switch_history`] are the two ways this gets read.
    pub switch_lineage: HashMap<AgentId, AgentId>,
    /// Board item `01M2PGS1GGNDNSA0A6E074G4VF` ("every `/model`/`/role`
    /// switch destroys its own confirmation notice before it is ever
    /// drawn"): the switch confirmation text `switch_session`
    /// (`commands.rs`) computed for its just-created child, held here
    /// instead of being pushed onto `transcript` directly, because
    /// `switch_session` returns `Effect::FocusNewSession` and THAT effect's
    /// handling -- `app/focus.rs::try_focus_agent` calling
    /// [`Self::focus_agent`] -- unconditionally clears `transcript` in the
    /// very same synchronous tick (`focus_agent`'s own doc: switching to a
    /// freshly-created child must show ONLY that child's own log, never a
    /// parent's leftover text). A plain `notice()` push before that clear
    /// would be wiped before the run loop's single `if dirty` redraw ever
    /// sees it; parking the text here instead and having
    /// [`Self::focus_agent`] re-push it immediately after its OWN clear is
    /// what keeps it alive for that first frame. `None` for every focus
    /// switch that is NOT a `/model`/`/role` switch (bare `/fork`,
    /// `/spawn`, `/resume`, plain re-focus) -- those paths never set this
    /// field, so the post-clear re-push is a no-op for them, exactly
    /// preserving `focus_agent`'s "child's own log only" guarantee.
    ///
    /// Cleared by `App::submit`, not by `Event::UserTurn`: focusing a child
    /// REPLAYS its history, so a replayed `UserTurn` would drop a notice
    /// staged by the very switch that just focused it.
    pub pending_focus_notice: Option<String>,
    /// Board item `01M1YVH1X49WYSQ9C2Z4D6B4XM`: the trimmed, lowercased
    /// word (`"exit"`, `"quit"`, `"q"`, `":q"`, `":wq"`) from the operator's
    /// MOST RECENTLY INTERCEPTED bare submission -- an operator reaching
    /// for another tool's quit command, which would otherwise be sent to
    /// the model as an ordinary prompt and get a cheerful, useless reply
    /// (the operator's own real sessions were the trigger: they typed
    /// `exit` as a prompt twice).
    /// `tui::app::exit_guard::check` reads this to tell "first time" (show
    /// a one-line hint, refuse to send, set this) from "the operator typed
    /// the SAME word again right away" (send it as a normal prompt this
    /// time -- a deliberate repeat is the override). `App::submit` clears
    /// it on EVERY submission that is not itself one of the intercepted
    /// words, so only a genuinely consecutive repeat sends -- an unrelated
    /// line in between resets the hint for next time.
    pub pending_exit_word: Option<String>,
    /// What an operator's own command NAMED for a child -- a `--role`/
    /// `--model` flag on `/spawn`/`/fork`, or the whole point of a
    /// `/model <backend/model>`/`/role <alias>` switch -- keyed by the
    /// child's own `AgentId`. Written by
    /// `commands::bare_fork`/`bare_spawn` (and the explicit-target
    /// `/fork @<agent> --role`/`--model` arm) right after the fork/spawn
    /// that used the flag succeeds, and by `commands::switch_session`
    /// (board item `01M2TWAZXTVB50YGMDK7MRN2W1`) right after the fork a
    /// switch is made OF succeeds -- beside its `switch_lineage` write, so
    /// both facts about that child land before anything observes it.
    ///
    /// **What this is not: the model an agent effectively resolves to.**
    /// Every agent has one of those; it is re-resolved per turn from the
    /// child's own `AgentSpec` and is not knowable here without a routing
    /// lookup per row per frame. This map holds only the narrower fact
    /// that somebody TYPED a model or role for this agent, which is
    /// exactly the fact worth spending a deep tree's row width on -- see
    /// `commands::switch_session`'s own doc. A side-table alongside
    /// `AgentTreeView`/`TreeNode`, mirroring `budget_warned_agents`'/
    /// `switch_lineage`'s own reasoning just above (`TreeNode` has no
    /// `Default`, so a marker only a few call sites ever set does not
    /// belong on it): the runtime's own `Event::AgentSpawned` carries no
    /// role/model field at all (routing is resolved lazily, per turn, off
    /// the child's own `AgentSpec` -- see `conway_runtime::subagent::start`),
    /// so this fact exists ONLY here, in the places that actually typed
    /// the flag or the switch -- a model-invoked `conway_spawn`/
    /// `conway_fork` tool call elsewhere in the tree has no entry here at
    /// all, and its row shows the plain recipe exactly as it did before
    /// this field existed.
    /// `view::agents::recipe_parts`/`view::agents::hop_label` are the two
    /// places this gets read (the `/agents` panel row and the status
    /// line's lineage breadcrumb).
    pub spawn_role_or_model: HashMap<AgentId, SpawnRoleOrModel>,
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
    /// The current braille spinner frame index. Advanced by
    /// [`Self::tick_animation`] modulo [`SPINNER_FRAMES`]' length, only while
    /// [`Self::activity`] is not [`Activity::Idle`]. Rendered by
    /// `view/status.rs` as the glyph preceding the activity phrase.
    pub spinner_frame: usize,
    /// When the focused agent's current turn started: set by
    /// `Event::TurnStarted` for the focused agent and cleared whenever
    /// `activity` returns to [`Activity::Idle`] (`TurnFinished`/
    /// `AgentFinished` for the focused agent, or [`Self::focus_agent`]). The
    /// status line renders live `elapsed` from `Instant::now() -
    /// turn_started_at` while this is `Some`; `None` while idle.
    pub turn_started_at: Option<Instant>,
    /// When the permission prompt the focused agent is currently blocked on
    /// opened (board item `01M2V60KWK9AYX3J7V5TPJZN7Q`): stamped by
    /// `Event::PermissionRequested` for the focused agent, cleared by that
    /// call's `Event::PermissionResolved` and by [`Self::focus_agent`].
    ///
    /// This is a SECOND clock, deliberately not a reuse of
    /// [`Self::turn_started_at`]. `turn_started_at` means "a model turn is
    /// in flight", and by the time a permission prompt is up that is false:
    /// the agent loop's binding event order is `TurnStarted <
    /// ModelDecision < TextDelta* < TurnFinished < ToolCallProposed*`
    /// (`conway-runtime`'s `agent_loop` module doc), so `TurnFinished` --
    /// and with it `clear_turn_state`, which zeroes `turn_started_at` --
    /// has always already been applied before any permission prompt for
    /// that turn's tool calls can open. The status line's elapsed figure
    /// read `turn_started_at` unconditionally and therefore rendered a
    /// literal `0s` for the WHOLE wait, in the one state where the number
    /// is the operator's actual decision input. Making `turn_started_at`
    /// survive the permission path instead would have made every other
    /// reader of it ("is a turn in flight?" -- `state.rs`'s own
    /// `TextDelta`/`ContextSegmentAdded` gates, `app/plugin_cmd.rs`,
    /// `app/focus.rs`) lie instead.
    ///
    /// Reads the same wall clock the persisted record does: the
    /// `waited_ms` on `LogRecord::PermissionDecisionRecord` measures the
    /// same interval from the broker's side, so the live figure and the
    /// recorded one agree.
    pub awaiting_permission_since: Option<Instant>,
    /// When the tool call that the `running <tool>…` rung NAMES was
    /// proposed (board item `01M2VDD6MQG7H73AHGX8HJGV2J`): stamped by
    /// `Event::ToolCallProposed` for the focused agent, in the same breath
    /// as the [`Activity::RunningTool`] that carries the name, and cleared
    /// wherever `apply` moves the focused agent's `activity` off that rung.
    ///
    /// A THIRD clock, for exactly the reason
    /// [`Self::awaiting_permission_since`] is a second one, and with
    /// exactly the same defect behind it: the agent loop's binding event
    /// order is `TurnStarted < ModelDecision < TextDelta* < TurnFinished <
    /// ToolCallProposed*`, so `TurnFinished` -- and with it
    /// `clear_turn_state`, which zeroes [`Self::turn_started_at`] -- has
    /// ALWAYS already been applied before any tool call of that turn is
    /// dispatched. The status line's elapsed figure read `turn_started_at`
    /// for this rung too, found `None` every single time, and rendered
    /// `running bash… 0s` for the whole execution of every tool call on
    /// every turn while the spinner beside it kept advancing.
    ///
    /// **What the number means when several calls are in flight -- the
    /// decision this field encodes.** The rung names ONE tool, because
    /// `Activity::RunningTool` carries one name and each
    /// `ToolCallProposed` overwrites it; with a parallel batch the name is
    /// the most recently proposed call's. So the number here is *that*
    /// call's own age: name and clock are written together, in one place,
    /// and are replaced together. The figure answers the question the row
    /// actually asks -- "how long has THIS call been going" -- and never a
    /// different one.
    ///
    /// The rejected alternative was the oldest in-flight call ("how long
    /// has this batch been going"). It is a real question, but it is not
    /// the one a row reading `running bash…` asks, and pairing that name
    /// with a batch-wide duration is precisely the plausible-looking
    /// number attached to the wrong call that the board item calls worse
    /// than an obvious zero -- a wrong `0s` is at least visibly wrong. A
    /// per-`call_id` map was rejected for a subtler reason: the rung can
    /// only ever show one figure, so the map would still need this very
    /// "which one" rule on top of a lifetime to manage.
    ///
    /// The consequence, stated so no later reader files it as a bug: while
    /// a batch runs, the figure restarts each time a later call in that
    /// batch is proposed -- because the NAME restarts with it. The pair
    /// stays coherent, and the figure is never the batch's age. Per-call
    /// history is not lost: the transcript keeps an `Entry::Tool` row per
    /// `call_id`, each stamped with its own envelope timestamp.
    ///
    /// **Lifetime.** Stamped only at the sites that set
    /// `Activity::RunningTool` (`apply`'s `ToolCallProposed` and
    /// `ToolCallStarted` arms), and cleared at each `apply` site that takes
    /// the focused agent off that rung (`TurnStarted`,
    /// `PermissionRequested`, a denial in `PermissionResolved`,
    /// `TurnFinished`, `AgentFinished`) and by [`Self::focus_agent`].
    /// Because the renderer reads this field ONLY from
    /// `activity_elapsed_secs`'s `Activity::RunningTool` arm, and every
    /// writer of that variant is the same statement that stamps this
    /// clock, a value left here by an `activity` write elsewhere in the
    /// TUI (`app.rs`/`app/focus.rs` both set `Thinking`) can never be the
    /// one rendered.
    ///
    /// The adjacent behavior recorded here as known and
    /// unchanged -- a prompted call executing under the `awaiting
    /// permission…` rung because `Event::PermissionResolved` left
    /// `activity` at [`Activity::AwaitingPermission`] -- was then fixed by
    /// board item `01M2X463TDVV5TG53M3X1M3M6V`: `Event::ToolCallStarted`
    /// puts the rung back up and re-stamps this clock, so an approved
    /// call's figure measures its execution and not the deliberation that
    /// preceded it. See that arm in `apply` for why the transition is
    /// carried by `ToolCallStarted` rather than by the resolution itself,
    /// and what a denied call shows instead.
    pub running_tool_since: Option<Instant>,
    /// New context tokens ADDED this turn: the sum of
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
    /// lands via the turn-end summary. Reset to 0 on `TurnStarted` and
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
    /// T3 sibling of [`Self::focused_model_max_context`]: the resolved
    /// window's `ContextTokensSource` for the SAME focused model, looked up
    /// from [`Self::model_max_context_source`] at the exact same two sites
    /// (and with the exact same fallback-to-bare-model-id rule) as
    /// `focused_model_max_context` itself, so the two can never disagree
    /// about which model they describe (board item
    /// `01M1ZJ796E0YP6Y8QWS8HB0AVB`). `None` under the identical conditions
    /// `focused_model_max_context` is `None`. Reset to `None` on
    /// [`Self::focus_agent`], alongside its sibling.
    pub focused_model_max_context_source: Option<conway::ContextTokensSource>,
    /// Board item `01M2NS0996E139VN5R8W4PGD8V` (cache-suffix wording):
    /// the focused model's backend's declared `CacheReporting` -- looked up
    /// from [`Self::model_cache_reporting`] at the same `Event::
    /// ModelDecision` site [`Self::focused_model_max_context_source`] is,
    /// but keyed by BACKEND ID ALONE (`ModelRef::backend`), not the full
    /// `"backend/model"` string -- `CacheReporting` is a `Backend`-level
    /// declaration (`Backend::cache_reporting`'s own doc), not a
    /// per-`(backend, model)` fact like the context window, so there is no
    /// bare-model-id fallback here: the key is unambiguous already. `None`
    /// when [`Self::model_cache_reporting`] has no entry for the focused
    /// backend (its id was never configured/injected) -- distinct from,
    /// and never collapsed into, either declared `CacheReporting` variant
    /// (`crate::tui::usage_format::cache_suffix`'s own doc: a candidate
    /// this state could not ask is not the same fact as one that answered
    /// either way). Reset to `None` on [`Self::focus_agent`], alongside its
    /// `focused_model_max_context*` siblings.
    pub focused_model_cache_reporting: Option<conway::CacheReporting>,
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
    /// Dedup rationale: the runtime's
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
    /// empty seen-set -- then immediately re-seeded by
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
    /// lines followed by a dim `… (+M lines, Ctrl-O to expand)` affordance
    /// while `Entry::Tool::expanded` is `false`; the full preview renders
    /// while `expanded` is `true`. The stored `preview` is NEVER truncated
    /// -- the cap is render-time only. Set at `App::new` from
    /// `crate::tui::config::load`'s `tool_preview_lines` via
    /// [`clamp_tool_preview_lines`] (config is untrusted: clamped to `1..=200` with a
    /// fallback to the default of 3 on a missing/out-of-range/bad value,
    /// never a panic).
    pub tool_preview_lines: u32,
    /// T3: the local model-metadata map (`"backend/model"` -> max context
    /// tokens), derived once at `App::new`. Board item
    /// `01M1ZJ796E0YP6Y8QWS8HB0AVB` (context-window-provenance, status-line
    /// half): the window NUMBER now comes from `Conway::capability_index()`
    /// -- the same resolved index `routes explain`/the runway
    /// notice/the admission gate already read -- rather than
    /// `Conway::model_metadata()`'s bare `max_context_tokens` directly;
    /// [`Self::model_max_context_source`] is the companion map carrying
    /// each entry's provenance. The known key set (which `"backend/model"`
    /// strings even get looked up) still comes from `model_metadata()`,
    /// since that map is still the one place `models.json` names which
    /// pairs exist at all -- only the VALUE for each key changed source.
    /// `apply`'s `ModelDecision` arm, and `app.rs`'s `try_focus_agent`
    /// re-fetch alike, look up the chosen model here to set
    /// `focused_model_max_context`. Empty when the builder found no
    /// metadata file, it named no models, or the index has no entry for a
    /// named pair (its backend was never configured/injected) -- the
    /// status line then renders raw context tokens instead of a
    /// percentage.
    pub model_max_context: HashMap<String, u32>,
    /// T3 sibling of [`Self::model_max_context`], keyed identically:
    /// `"backend/model"` -> the resolved window's `ContextTokensSource`
    /// (board item `01M1ZJ796E0YP6Y8QWS8HB0AVB`). Populated from the SAME
    /// `Conway::capability_index()` lookup that fills `model_max_context`,
    /// so a key present in one is present in the other (both come from the
    /// identical `CapabilityIndex::get`/`::context_window_source` pair per
    /// model ref) -- never independently resolved. The status line's `ctx`
    /// field reads [`Self::focused_model_max_context_source`] (this map's
    /// per-focused-agent projection) to decide whether to append the
    /// "floor (assumed)" marker -- see `view::status::ctx_label`'s own doc.
    pub model_max_context_source: HashMap<String, conway::ContextTokensSource>,
    /// Board item `01M2NS0996E139VN5R8W4PGD8V`: `"backend"` (bare backend
    /// id, NOT `"backend/model"` -- see [`Self::focused_model_cache_reporting`]'s
    /// own doc for why the key shape differs from [`Self::model_max_context`])
    /// -> that backend's declared [`conway::CacheReporting`], derived once
    /// at `App::new` from `Conway::capability_index().cache_reporting`, the
    /// SAME already-resolved index [`Self::model_max_context`] reads.
    /// `apply`'s `ModelDecision` arm looks up the chosen model's backend id
    /// here to set [`Self::focused_model_cache_reporting`]. Empty when the
    /// builder found no capability index entry for a configured backend --
    /// the status line then falls back to the "capability unknown" cache
    /// wording rather than fabricating one.
    pub model_cache_reporting: HashMap<String, conway::CacheReporting>,
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
    /// Board item `01M1YVHKTQVXJRDSRYT3TCRXFX`: what submitting a message
    /// while the focused agent's turn is running does -- `queue` (default),
    /// `steer`, or `interrupt`. Seeded at `App::new` from
    /// `[tui.busy_input]`; the `/settings` menu's "display" group cycles it
    /// for the rest of THIS session only, the same session-only posture
    /// `show_reasoning`/`show_timestamps` already have (`view/settings.rs`'s
    /// own module doc). See `state::busy_input`'s own module doc for the
    /// full mechanism.
    pub busy_input: BusyInputMode,
    /// Board item `01M1YVJNS575YN5DCQG9BKZR4E`: `emacs` (default, today's
    /// readline-shaped keymap unchanged) or `vim` (a modal editing layer,
    /// `crate::tui::input::vim`). Seeded at `App::new` from `[tui.
    /// editor_mode]`; the `/settings` menu's "display" group toggles it for
    /// the rest of THIS session only, the same session-only posture
    /// [`Self::busy_input`] immediately above already has. See `state::
    /// editor_mode`'s own module doc for the full CARRY/RESET split against
    /// [`Self::vim`].
    pub editor_mode: EditorMode,
    /// `busy_input = queue`'s own withheld-message FIFO, tagged with the
    /// `AgentId` each entry was queued FOR -- review round 1's CRITICAL
    /// fix: a bare `String` queue implicitly assumed delivery would always
    /// target whichever agent happened to be focused, which is wrong the
    /// instant focus moves between queuing and delivery. `Up` acts on the
    /// BACK of the FOCUSED agent's own entries only (never another
    /// agent's); `App::flush_ready_queues` (`app/busy_input.rs`) reads by
    /// `AgentId`, never by `focused_agent`. See `AppState::
    /// recall_last_queued`/`take_held_prompts_for`'s own docs, and
    /// `state::busy_input`'s own module doc for the full design. Mirrored,
    /// one-for-one, by an `Entry::QueuedUser` transcript row per entry
    /// still belonging to the CURRENTLY focused agent -- see `AppState::
    /// queue_prompt`.
    ///
    /// **Not `queued_prompts`** (a name already taken by the PERMISSION
    /// gate's own `VecDeque<PendingPrompt>`, `gate.rs`'s unrelated queue of
    /// pending tool-call authorizations) -- this is a different queue
    /// entirely, so it gets a different name rather than shadowing that
    /// one.
    pub held_prompts: std::collections::VecDeque<(AgentId, String)>,
    /// `busy_input = steer`'s own pending-VISIBILITY FIFO, tagged with the
    /// `AgentId` each steer was sent TO -- the same per-agent tagging
    /// `held_prompts` carries, for the same reason. A steer is already
    /// sent (through the existing mailbox) the instant it is queued here,
    /// so this exists only to drive the queued strip/transcript until
    /// `App::flush_ready_queues` observes that agent is no longer
    /// mid-generation and calls `AppState::clear_pending_steers_for`,
    /// never to support recall (there is nothing left to withdraw -- see
    /// `state::busy_input`'s own module doc).
    pub pending_steers: std::collections::VecDeque<(AgentId, String)>,
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
    /// Board item `01M11XWB4T8ZADNDB4M8R482MA`: the settings menu's own
    /// providers section -- every `backends.<id>` entry the CURRENT merged
    /// config declares, refreshed (never merged into) each time `/settings`
    /// is opened and after any add/remove from this section, via
    /// `App::refresh_provider_entries_and_kick_off_status` (`app/
    /// provider_status.rs`) -- the same "re-run the real merge, don't
    /// enumerate layers by hand" idiom `app/plugin_toggle.rs`'s own
    /// project-layer-override check already uses. This is what makes
    /// acceptance 5 ("appears as working without a restart") possible at
    /// all: `self.conway.config()` is the STALE, build-time snapshot
    /// (restart-to-apply, exactly as `/plugin`'s own doc establishes), so a
    /// provider added THIS session would never appear here if this field
    /// mirrored that instead of a fresh `conway::config::load`.
    pub provider_entries: BTreeMap<String, BackendEntry>,
    /// The most recent live classification of [`Self::provider_entries`],
    /// keyed identically -- `conway::backend_usability::classify_fleet`'s
    /// own per-entry map, called with `ProbePolicy::All` (never `LocalOnly`:
    /// this screen is the operator looking at the list with live status
    /// implicitly requested by opening it, not a startup path -- see that
    /// module's own doc). An id present in [`Self::provider_entries`] but
    /// ABSENT here means "not yet classified" (the initial state, and the
    /// window between opening the section and the spawned probe task's
    /// reply arriving via `App`'s `provider_status_rx`) -- rendered as its
    /// own "checking..." row, distinct from both `Usable` and `Unusable`,
    /// so a slow probe can never be misread as a broken provider.
    pub provider_status: BTreeMap<String, Usability>,
    /// Whether a background classification is currently in flight -- set by
    /// `App::refresh_provider_entries_and_kick_off_status` the instant the
    /// probe task is spawned, cleared by `App::apply_provider_status_done`
    /// once its reply arrives. Read only by the row-rendering helper (`view/
    /// settings.rs`) to decide whether an id absent from
    /// [`Self::provider_status`] is "checking..." or a genuine gap.
    pub provider_status_loading: bool,
    /// Board item `01M18Q7P25DTSKQJDJJCC3E800`: the settings menu's own
    /// "defaults" section -- a fresh (never `Conway::config()`'s stale
    /// build-time snapshot) read of the CURRENT merged `default_role`,
    /// refreshed on the same "reopen `/settings`" seam
    /// [`Self::provider_entries`] already uses, via
    /// `App::refresh_default_entries` (`app/defaults.rs`). Empty only
    /// before the first refresh has ever run.
    pub default_role_snapshot: String,
    /// The default model: `conway::config::schema::ConwayConfig::model_for`
    /// applied to [`Self::default_role_snapshot`] and
    /// [`Self::known_role_names`]' own chains -- a DERIVED display, never a
    /// second stored value (see `ConwayConfig::default_model`'s own doc for
    /// why). `None` when the default role has no `[roles]` entry, or one
    /// with an empty `chain` -- rendered as "not configured", never a
    /// synthesized guess.
    pub default_model_snapshot: Option<String>,
    /// Every role name the current merged config's `[roles]` declares that
    /// an OPERATOR actually configured, sorted (mirrors `BTreeMap`'s own
    /// iteration order, since `app/defaults.rs` reads the same lax `roles`
    /// map `app/provider_manage.rs::load_roles_lax` already uses) --
    /// `input::activate_settings_selection`'s `LEAF_DEFAULT_ROLE` arm
    /// cycles [`Self::default_role_snapshot`] through this list, wrapping.
    /// Refreshed on the identical seam as the two fields above.
    ///
    /// Excludes `conway::config::merge::default_document`'s baked-in
    /// `"default"` role floor (see
    /// `conway::config::is_baked_in_role_floor`'s own doc) -- that entry
    /// exists only so an unconfigured `default_role` still validates, and
    /// was never something an operator chose; offering it here would let a
    /// human cycle the session default onto a role with an intentionally
    /// empty chain.
    pub known_role_names: Vec<String>,
    /// Board item `01M1A35S609TZ613GAECPEHX8D`: every `"backend/model"` pair
    /// any OPERATOR-configured role's `chain` names, sorted and deduped --
    /// what bare `/model` lists (`commands::execute`'s `Model { model: None
    /// }` arm) rather than a remote roster (a live provider API call), so
    /// this is "what is configured", not "what could be configured".
    /// Refreshed on the SAME seam as [`Self::known_role_names`] (`App::
    /// refresh_default_entries`), read fresh right before `/model` bare
    /// runs (`App::submit`'s own `/model`-bare branch, mirroring how it
    /// already refreshes this seam ahead of `/settings`). Excludes the same
    /// baked-in `"default"` role floor [`Self::known_role_names`] excludes,
    /// for the identical reason: an unconfigured floor role is a validation
    /// safety net, never something an operator actually set up to route to.
    pub configured_models: Vec<String>,
    /// Board item `01M24ZJ9ABPP0DGVAA2PS3XVDD`: the `--model` flag's own
    /// `"backend/model"` string (`cli.model`, parsed once via `crate::
    /// model_pin::parse_model_pin`), set at `App::new` and never mutated
    /// afterward. `None` when no `--model` flag was given.
    ///
    /// Distinct from [`Self::focused_model`]: that field is the CONFIRMED
    /// serving model of the most recently completed turn (`Event::
    /// ModelDecision`/`SessionHandle::last_model`), `None` until one
    /// exists. This field is the CLI's own stated intent, known before any
    /// turn at all -- what lets bare `/model` (`commands::execute`'s
    /// `Model { model: None }` arm) list and mark the pin even before the
    /// session has answered a single message, the exact gap the
    /// reproduction in this board item's own spec exercises: a launch with
    /// backends declared only through `CONWAY_BACKENDS__*` (so
    /// [`Self::configured_models`] is necessarily empty -- no role chain
    /// can be built from the environment at all, see
    /// `conway::config::merge::env_to_value`'s own doc) plus a `--model`
    /// pin. `model_picker::candidate_models` unions this in as a third
    /// source, alongside the chain and metadata sources its own doc
    /// already named.
    pub model_pin: Option<String>,
    /// [`Self::model_pin`]'s own sibling for `--role-override`: `cli.
    /// role_override`, set at `App::new` and never mutated afterward.
    /// `None` when the flag was not given, in which case bare `/role`
    /// (`commands::execute`'s `Role { role: None }` arm) reports
    /// [`Self::default_role_snapshot`] as the active role instead.
    pub role_pin: Option<String>,
    /// Board item `01M24ZJ9ABPP0DGVAA2PS3XVDD`: every `backends.<id>` key
    /// the CURRENT merged config declares, file OR environment layer alike
    /// (`conway::config::merged_document`'s own five-layer merge -- the
    /// SAME source [`Self::provider_entries`] reads, but refreshed on bare
    /// `/model`/`/role`'s own seam, `App::refresh_default_entries`,
    /// instead of `/settings`' -- see that method's own doc). Sorted (a
    /// `serde_json::Map`'s own key order, alphabetic without the
    /// `preserve_order` feature).
    ///
    /// What this is FOR: telling "zero backends configured" -- the only
    /// case that earns bare `/model`'s "no models are configured" notice
    /// -- apart from "backends exist, but no role chain or `--model` pin
    /// names a model on any of them," a different situation that needs a
    /// different message (naming `roles.<alias>.chain`/`--model` as what
    /// to add, not "add a provider": a provider already IS configured).
    /// Empty only before the first refresh, exactly like
    /// [`Self::configured_models`]'s own doc.
    pub configured_backend_ids: Vec<String>,
    /// Board item `01M24ZJ9ABPP0DGVAA2PS3XVDD`: every role the CURRENT
    /// merged config's `[roles]` table names, INCLUDING `conway::config::
    /// is_baked_in_role_floor`'s baked-in `"default"` floor -- unlike
    /// [`Self::known_role_names`], which deliberately excludes it (that
    /// field drives `/settings`' cycle list, where offering the floor
    /// would let an operator switch the session default onto an
    /// intentionally empty chain). Bare `/role` has the opposite need: an
    /// environment-only configuration can never author a real role at all
    /// (`conway::config::merge::env_to_value`'s own doc, "a role chain
    /// cannot be created from the environment"), so the floor is the ONLY
    /// role such a launch ever has, and it must still be listed --
    /// labelled built-in, per this board item's own acceptance criterion
    /// -- rather than making bare `/role` report nothing configured at
    /// all. Refreshed on the SAME seam as [`Self::known_role_names`]
    /// (`App::refresh_default_entries`), sorted by name.
    pub role_listing: Vec<RoleListingEntry>,
    /// Set by `commands::execute`'s `Model { model: None }` arm the instant
    /// it opens `Mode::UiForm` as `/model`'s own picker (`conway.ui`
    /// installed) -- read and cleared by `run.rs`'s `Action::
    /// UiFormDecision` dispatch arm BEFORE the generic `AppState::
    /// resolve_ui_form` call, so that arm can tell "the form the operator
    /// just answered was `/model`'s own menu" apart from a real model-called
    /// `ask_question` (which never touches this flag). `false` otherwise --
    /// in particular, always `false` again once that one resolution has
    /// been read, so a LATER, unrelated `ask_question` is never mistaken for
    /// a pending model switch.
    pub model_picker_active: bool,
    /// [`Self::model_picker_active`]'s own sibling for bare `/resume`'s
    /// picker (board item `01M1YS550T52VR1VW8NMETXQS1`): set the instant
    /// `commands::execute`'s `Resume { sid: None }` arm opens `Mode::
    /// UiForm` over `commands::open_session_picker`'s rows, read and
    /// cleared by the identical `run.rs` dispatch arm BEFORE the generic
    /// `AppState::resolve_ui_form` call, for the identical reason: telling
    /// "the form just answered was the session picker" apart from a real
    /// model-called `ask_question` (which touches neither this flag nor
    /// `model_picker_active`). The two flags are mutually exclusive in
    /// practice (only one picker is ever open at a time -- `Mode::UiForm`
    /// itself has room for exactly one), but each is read/cleared
    /// independently rather than folded into one shared flag, matching
    /// [`Self::model_picker_active`]'s own precedent: a THIRD picker
    /// reusing this same furniture later gets its own flag too, not a
    /// widening enum every existing call site would have to learn.
    pub session_picker_active: bool,
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
    /// The TUI renders a dim one-line entry under a tool call for a
    /// PROMPTED permission decision (source: operator, reaching
    /// `PermissionGate::check` live) -- built from TWO sibling events for
    /// the same `call_id`, correlated here rather than duplicating either
    /// one's own data: `Event::PermissionResolved`'s `PermissionDecisionKind`
    /// (`AllowOnce`/`AllowAlways`/`Denied`/`DeniedWithFeedback`/`Cached`,
    /// already reachable via `conway::PermissionDecisionKind` -- P-14, this
    /// crate never restates that mapping) supplies the WORDING;
    /// `Event::PermissionDecision`'s own `waited_ms`/`feedback` (a newer,
    /// audit-record-shaped event whose `decision`/`source` fields this crate
    /// has no dependency reachable to NAME -- `conway-core` is a
    /// `[dev-dependencies]`-only crate for `conway-cli`, so a production
    /// match arm can read those two fields' VALUES structurally but cannot
    /// write a pattern naming their enum types) supplies the DURATION and
    /// any denial reason. `PermissionResolved` always precedes its sibling
    /// `PermissionDecision` for the same call (`PermissionBroker::decide`'s
    /// own emission order, `conway-runtime`), so the `PermissionResolved`
    /// arm stashes its kind here keyed by `call_id`; the `PermissionDecision`
    /// arm below reads and removes it (one-shot per call, never leaked
    /// across a session) to build the rendered line. Entries with no
    /// stashed kind (a `PermissionDecision` this session never watched the
    /// matching `PermissionResolved` for -- e.g. a fresh subscription mid-
    /// call) still render, falling back to a wording derived from
    /// `feedback.is_some()` alone (see
    /// `transcript::format_permission_decision_note`'s own doc).
    pub permission_decision_pending: HashMap<String, PermissionDecisionKind>,
    /// Board item `01M1YVJ4RA5V7FF95MFRQMTQW3`: this session's EFFECTIVE
    /// rebindable keymap -- built-in defaults (`AppState::new`) merged with
    /// `$CONWAY_CONFIG_DIR/keybindings.json`, if any, once at `App::new`
    /// (`app/startup.rs`). `input.rs`'s dispatcher and `/help`'s overlay
    /// both resolve through THIS SAME field -- an ordinary `AppState`
    /// field, not a thread-local/process-global, because this crate's own
    /// `#[tokio::main]` runtime is multi-threaded: a task can resume on a
    /// DIFFERENT OS thread after any `.await`, which would silently strand
    /// a thread-local's installed value on the thread that happened to
    /// install it. Threading it through `AppState` like every other piece
    /// of session state (module doc, "one `AppState`, driven by explicit
    /// field writes") sidesteps that hazard entirely, and mirrors how this
    /// crate's tests already inject config explicitly rather than mutating
    /// process-global env/statics (`crate::tui::keybindings`'s own doc).
    pub keybindings: crate::tui::keybindings::Keymap,
    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: this session's RUNNING
    /// per-path content for `edit`/`write` tool calls -- what conway
    /// believes each touched file now holds. The FIRST time a path is seen
    /// (in `Event::ToolCallProposed`, below) its current on-disk bytes are
    /// read ONCE and stored here (and, unchanged forever after, in
    /// [`Self::diff_baseline`]); every subsequent successful call to that
    /// same path folds its own change on top (`AppState::finish_tool`, via
    /// `crate::diff::apply_touch`), with no further disk access. That is
    /// what lets a per-call settled-entry diff (stored once in
    /// [`Self::tool_diffs`], never recomputed) be computed from in-memory
    /// state alone after the first touch to a given path: the `before` it
    /// diffs against is this field's value just prior to the fold.
    ///
    /// **This field is the *after* side, not the *before* side, and
    /// `/diff` does not read it** (board item
    /// `01M2V6HMBAWKM0GG90J14K4Q8F` corrected an earlier version of this
    /// doc that claimed otherwise). `/diff` needs the bytes the session
    /// STARTED from, which this field has already folded away; it takes
    /// those from [`Self::diff_baseline`] and re-folds the transcript's
    /// own successful touches through `crate::diff::cumulative_diffs`,
    /// which applies the identical `apply_touch` rule -- so the two can
    /// never disagree about what a given call did.
    pub(crate) diff_track: HashMap<String, String>,
    /// Board item `01M2V6HMBAWKM0GG90J14K4Q8F`: the FROZEN twin of
    /// [`Self::diff_track`] -- the bytes each `edit`/`write` target held the
    /// first time this session proposed a call against it, seeded from the
    /// SAME single disk read and never folded, never overwritten
    /// afterwards. This is `/diff`'s baseline (passed to
    /// `crate::diff::cumulative_diffs` as `known_baselines` by
    /// `tui/commands.rs::render_diff_snapshot`).
    ///
    /// **Why a captured snapshot and not a read at `/diff` time.** By the
    /// time the operator types `/diff` the calls have landed, so the file's
    /// current bytes are the *after*; reading them then and calling them
    /// the baseline yielded an empty diff for every real session -- the
    /// defect this field exists to fix. The capture happens at PROPOSE
    /// time, before the tool runs, which is the only moment the true
    /// "before" is still on disk.
    ///
    /// Populated only for paths this `AppState` actually watched a
    /// `ToolCallProposed` for. A focus-switch/resume replay emits no
    /// `ToolCallProposed` at all (`conway::session_handle::record_to_event`
    /// has no arm for it), so a replayed session's paths are simply absent
    /// here and `cumulative_diffs` falls back to reconstructing their
    /// baselines from the recorded touches -- see its own doc for that
    /// fallback's limits.
    pub(crate) diff_baseline: HashMap<String, String>,
    /// Board item 01M1YVEJB6GAPST5YZET4KZZE2: one settled `edit`/`write`
    /// tool call's own unified diff (`crate::diff::unified_diff`), keyed by
    /// `call_id` and computed EXACTLY ONCE, in
    /// `AppState::finish_tool`, at the moment the call settles --
    /// never recomputed later (the file may have moved on by the time the
    /// transcript renders again, and the transcript must show the diff that
    /// actually happened, not a diff against whatever the file looks like
    /// now). Absent (no entry) for a call with nothing to show: any
    /// non-`edit`/`write` tool, a failed call, or a call whose computed
    /// diff was empty. `view/transcript.rs` looks this up by the entry's
    /// own `call_id` to fold the diff under `tool_preview_lines`, the same
    /// way the entry's own `preview` already folds -- kept as a SEPARATE
    /// map rather than a new field on `Entry::Tool` itself, since that
    /// variant has ~54 construction sites across this module (several in
    /// `input.rs`'s own tests) that would all need updating for one new
    /// required field; this keeps the change additive instead.
    pub(crate) tool_diffs: HashMap<String, String>,
    /// Board item `01M1YVF4X864GKSGZM4PSCTMEH`: the base directory
    /// [`crate::tui::mentions::scan_paths`] walks for `@`-mention/plain-
    /// `Tab` path completion -- the operator's `--root` confinement
    /// directory when one is set (so the candidate list can never name a
    /// path outside it), else this session's own `cwd`. `AppState::new`
    /// defaults this to the process's real cwd (so a test/fixture
    /// `AppState` still behaves sanely with no further setup);
    /// `App::new` (`app/startup.rs`) overwrites it with the resolved
    /// `--root`-or-`--cwd` value immediately after construction, mirroring
    /// `cwd_display`'s own "constructed here as a default, overwritten
    /// once at startup" shape.
    pub mention_scan_root: std::path::PathBuf,
    /// The char index of the `@` the currently-open mention completion
    /// list is anchored to, or `None` when no mention is open. Set by
    /// [`Self::sync_mention`] whenever the cursor enters a NEW `@`-token
    /// (`crate::tui::mentions::active_mention`'s own `start`); cleared the
    /// same way once the cursor leaves it (`Self::close_mention`).
    mention_anchor: Option<usize>,
    /// Which candidate universe the anchored mention addresses -- see
    /// [`crate::tui::mentions::MentionMode`]. `None` exactly when
    /// `mention_anchor` is `None`.
    mention_mode: Option<crate::tui::mentions::MentionMode>,
    /// The candidate universe for the anchored mention: file paths for a
    /// [`crate::tui::mentions::MentionMode::Path`] mention, agent
    /// names/ids for [`crate::tui::mentions::MentionMode::Agent`] --
    /// computed ONCE when the mention opens ([`Self::sync_mention`]), never
    /// re-walked on every keystroke; [`Self::mention_matches`] filters this
    /// cached universe live instead, which is what keeps live filtering
    /// cheap even though the walk behind it is not.
    mention_candidates: Vec<String>,
    /// Whether [`Self::mention_candidates`]'s own walk hit
    /// [`crate::tui::mentions::DEFAULT_SCAN_CAP`] -- surfaced in the
    /// overlay's own title so a capped listing never silently looks
    /// complete.
    mention_capped: bool,
    /// The arrow-navigated row in the mention completion list, mirroring
    /// [`Self::palette_selected`]'s own shape exactly.
    pub mention_selected: Option<usize>,
    /// Board item `01M1YVF4X864GKSGZM4PSCTMEH`: the `@`-token (by its own
    /// anchor char index) `Esc` most recently DISMISSED, if the cursor is
    /// still sitting inside that exact token. [`Self::sync_mention`] checks
    /// this FIRST: without it, typing one more character after dismissing
    /// the list with `Esc` would immediately re-open it (a fresh
    /// `mention_anchor != Some(q.start)` transition, since dismissal clears
    /// `mention_anchor`) -- the opposite of what `Esc` is for. Reset the
    /// moment the cursor leaves the token (a `None` mention query) or
    /// enters a genuinely different one, so a FRESH `@` always gets a fresh
    /// chance.
    mention_dismissed_for: Option<usize>,
    /// Review finding (CRITICAL, round 1): the shared cache both the
    /// `@`-mention overlay AND plain-`Tab` completion read
    /// (`state/mentions.rs::MentionScanCacheEntry`'s own doc) -- replaced
    /// the item's original per-feature, walk-on-this-call-stack design,
    /// which blocked `handle_key` (and so the whole TUI) for as long as the
    /// walk took. `None` until the first completed background walk lands
    /// (`Self::apply_mention_scan_result`); refreshed wholesale by each
    /// later one, never merged.
    mention_scan_cache: Option<crate::tui::state::mentions::MentionScanCacheEntry>,
    /// A background walk [`Self::sync_mention`]/[`Self::
    /// request_path_scan_warm_up`] determined is needed but cannot run on
    /// this struct's own call stack -- drained by `App::run`'s loop via
    /// [`Self::take_pending_mention_scan_request`] after every key/paste it
    /// dispatches, which is what actually spawns it off-loop
    /// (`app/mention_scan.rs`). See `state/mentions.rs`'s own module doc for
    /// the full "why" of this split.
    pending_mention_scan_request: Option<crate::tui::state::mentions::MentionScanRequest>,
    /// The anchor (char index of the `@`) a background walk is currently
    /// running FOR, if any -- distinct from `pending_mention_scan_request`
    /// (that field's own window closes the instant `App::run` takes it;
    /// this one stays set for the walk's whole in-flight duration). Read by
    /// [`Self::mention_scan_pending`] (the overlay's own "scanning..."
    /// signal) and used by [`Self::apply_mention_scan_result`] to decide
    /// whether a reply is still the one thing the live overlay is waiting
    /// on.
    mention_scan_in_flight: Option<usize>,
    /// Review finding (minor, round 1): whether the CURRENTLY open
    /// `@`-mention was opened by a bracketed paste rather than ordinary
    /// typing -- see [`Self::mention_blocks_enter_accept`]'s own doc for
    /// why that distinction gates whether `Enter` may accept a candidate.
    mention_via_paste: bool,
}

/// Board item `01M2N2HDV9YAFQP5S1ZJKPWE5V` (acceptance 5's own remainder):
/// the EXACT wording `conway routes explain` already prints for a resolved
/// [`conway::ContextTokensSource`]
/// (`crates/conway-cli/src/commands/routes.rs::render_context_window_source`),
/// reused verbatim here rather than a second phrasing -- the identical
/// discipline `tui::view::status::CTX_ASSUMED_FLOOR_MARKER`'s own doc
/// already names for the same constraint. **A duplicated FORMATTING
/// function, not a second resolver:** `render_context_window_source` is
/// private to its own module (`commands/routes.rs` is outside this lane's
/// own fence this wave, and was never asked to export it), so this is a
/// second copy of the same match arms, not a second decision about WHICH
/// source a `(backend, model)` pair resolves to -- that decision is still
/// made exactly once, by `conway_plugin_backends::capabilities::
/// max_context_tokens_source` (via `Conway::capability_index()`), and both
/// this function and `render_context_window_source` only ever format an
/// already-resolved answer. `#[non_exhaustive]` on `ContextTokensSource`
/// itself (mirrored by the wildcard arm here) is why `render_context_
/// window_source`'s own test is named "every DECLARED variant", not "every
/// variant" -- a future variant reads `"unknown"` on both surfaces until
/// each is updated, never a compile error nor a panic on either one.
pub(crate) fn context_window_source_word(source: conway::ContextTokensSource) -> &'static str {
    match source {
        conway::ContextTokensSource::Override => "models.json",
        conway::ContextTokensSource::Metadata => "verified",
        conway::ContextTokensSource::Probed => "probed",
        conway::ContextTokensSource::DialectDefaultFloor => "verified",
        conway::ContextTokensSource::Unverified => "floor (assumed)",
        _ => "unknown",
    }
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
            model_decision_history: VecDeque::new(),
            input: String::new(),
            cursor: 0,
            mode: Mode::Normal,
            vim: crate::tui::input::vim::VimState::default(),
            permission_mode: PermissionMode::default(),
            default_permission_mode: PermissionMode::default(),
            permission_paths: Vec::new(),
            grants_path: None,
            project_config_ignored: false,
            permission_grants: Vec::new(),
            structured_allow_rules: Vec::new(),
            permission_denies: Vec::new(),
            permission_prompts: Vec::new(),
            structured_deny_rules: Vec::new(),
            structured_prompt_rules: Vec::new(),
            shell_prefix_grants: Vec::new(),
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
            permission_prompt_armed_at: None,
            permission_confirm_always: false,
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
            awaiting_agents: HashSet::new(),
            shell_in_flight: false,
            last_shell_command: None,
            budget_warned_agents: HashSet::new(),
            switch_lineage: HashMap::new(),
            pending_focus_notice: None,
            pending_exit_word: None,
            spawn_role_or_model: HashMap::new(),
            modal_scroll: 0,
            pending_intent_confirm: None,
            pending_trust_preview: None,
            pending_ui_form: None,
            pending_skill_proposal: None,
            skill_propose_in_flight: false,
            pending_distill: None,
            distill_in_flight: false,
            distill_generation: 0,
            spinner_frame: 0,
            turn_started_at: None,
            awaiting_permission_since: None,
            running_tool_since: None,
            turn_running_tokens: 0,
            turn_transcript_start: 0,
            focused_model: None,
            focused_model_max_context: None,
            focused_model_max_context_source: None,
            focused_model_cache_reporting: None,
            focused_ctx_tokens: 0,
            focused_seen_segments: HashSet::new(),
            git_branch: None,
            cwd_display: None,
            status_line_config: StatusLineConfig::default(),
            tool_preview_lines: 3,
            model_max_context: HashMap::new(),
            model_max_context_source: HashMap::new(),
            model_cache_reporting: HashMap::new(),
            show_reasoning: true,
            show_timestamps: false,
            busy_input: BusyInputMode::default(),
            editor_mode: EditorMode::default(),
            held_prompts: VecDeque::new(),
            pending_steers: VecDeque::new(),
            history: VecDeque::new(),
            history_cap: DEFAULT_HISTORY_SIZE,
            history_index: None,
            history_draft: String::new(),
            help_open: false,
            settings_open: false,
            settings_selected: 0,
            settings_collapsed_groups: HashSet::new(),
            provider_entries: BTreeMap::new(),
            provider_status: BTreeMap::new(),
            provider_status_loading: false,
            default_role_snapshot: String::new(),
            default_model_snapshot: None,
            known_role_names: Vec::new(),
            configured_models: Vec::new(),
            model_pin: None,
            role_pin: None,
            configured_backend_ids: Vec::new(),
            role_listing: Vec::new(),
            model_picker_active: false,
            session_picker_active: false,
            // empty here by default
            // (mirrors every other collection field's construction-time
            // default) -- `App::new` overwrites this immediately after
            // construction with the real, resolved `CommandRegistry::
            // palette_entries()`; see this field's own doc for why
            // `/resume` must NOT go through this default a second time.
            plugin_commands: std::sync::Arc::new(Vec::new()),
            plugin_status_contributions: Vec::new(),
            permission_decision_pending: HashMap::new(),
            // Plain built-in defaults -- `App::new` (`app/startup.rs`)
            // overwrites this immediately after construction with the
            // real, resolved `Keymap::load` result, mirroring
            // `plugin_commands`'s own "constructed here as a default,
            // `App::new` overwrites it" doc just above.
            keybindings: crate::tui::keybindings::Keymap::defaults(),
            diff_track: HashMap::new(),
            diff_baseline: HashMap::new(),
            tool_diffs: HashMap::new(),
            // Overwritten by `App::new` with the resolved `--root`-or-
            // `--cwd` value -- see the field's own doc. The fallback here
            // (rather than, say, `PathBuf::new()`) keeps a bare test/
            // fixture `AppState` (no `App::new` involved) pointed at a real
            // directory, so `mentions::scan_paths` has something sane to
            // walk even when a test never sets this field itself.
            mention_scan_root: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from(".")),
            mention_anchor: None,
            mention_mode: None,
            mention_candidates: Vec::new(),
            mention_capped: false,
            mention_selected: None,
            mention_dismissed_for: None,
            mention_scan_cache: None,
            pending_mention_scan_request: None,
            mention_scan_in_flight: None,
            mention_via_paste: false,
        }
    }

    /// Board item `01M1YVKQ6ABQDWYSA7CEF20WKG` (review finding, round 2,
    /// widened past the original report): the ONE funnel `/new`
    /// (`commands::execute`'s `SlashCommand::New` arm) and `/resume`
    /// (`commands::apply_resume`) both use to reset this struct for a fresh
    /// root agent -- replacing the former `CarriedConfiguration` snapshot/
    /// restore pair, whose own hand-picked 12-field allowlist had quietly
    /// fallen behind this struct's own ~124 fields: every one configured at
    /// startup (`app/startup.rs`) and never recomputed thereafter --
    /// keybindings, `busy_input`, the status-line config, input history, the
    /// `project_config_ignored` security marker, `cwd_display`,
    /// `mention_scan_root`, the three `model_max_context*` maps, `git_branch`,
    /// `plugin_browser`, and the `subprocess_plugins`/`mcp_plugins`/
    /// `claude_compat_plugins` config mirrors -- was silently dropped by
    /// EVERY `/new`/`/resume` before this existed, with no error and no
    /// notice, reverting each to whatever bare `AppState::new` happens to
    /// default to.
    ///
    /// **The classification rule, applied field by field below** (an
    /// EXHAUSTIVE destructure with no `..` rest pattern, so a field added to
    /// this struct without being sorted into one arm or the other is a
    /// compile error, never a silent, unclassified thirteenth gap):
    ///
    /// - **CARRY** (written back onto `self` after the reset): process/
    ///   config-lifetime data -- set once from config, `Conway`, the CLI, or
    ///   the environment at `App::new` time, and never thereafter
    ///   recomputed from the OLD session's own conversation. The test for
    ///   "does this belong here" is "would a genuinely fresh TUI process,
    ///   launched against the SAME config/CLI/environment, show this same
    ///   value" -- if yes, losing it to a reset is pure data loss with no
    ///   session-identity meaning attached.
    /// - **RESET** (bound to `_`, left at whatever the fresh `AppState::new`
    ///   value already is): everything that belongs to the OLD session or
    ///   its conversation -- transcript, tree, modes, queues, in-flight
    ///   flags, focus, turn/activity clocks, per-call bookkeeping, and the
    ///   THREE revocable session-scoped permission-grant mirrors
    ///   (`permission_grants`/`structured_allow_rules`/
    ///   `shell_prefix_grants` -- a standing ruling, restated below at their
    ///   own arms).
    ///
    /// **`role_pin`/`model_pin` are RESET here, on purpose, even though
    /// `/new` carries them forward.** The two call sites disagree (`/new`
    /// carries them, `/resume` does not -- a resumed session already has its
    /// own established role/model, re-derived from ITS OWN `Event::
    /// ModelDecision` history rather than a pin a DIFFERENT prior session
    /// set), so this shared funnel cannot pick one answer for both. `/new`'s
    /// own arm captures both fields BEFORE calling this method and writes
    /// them back onto `self` AFTER it returns -- the exact same two-line
    /// shape it already used before this funnel existed, now the only
    /// hand-rolled carry-across left in either call site.
    ///
    /// **What is NOT carried, on purpose: [`Self::permission_grants`]/
    /// [`Self::structured_allow_rules`]/[`Self::shell_prefix_grants`].**
    /// Each is the REVOCABLE allow-side mirror of a blend of file-configured
    /// rules and whatever `PermissionScope::Session` grants the OLD session
    /// itself accrued, with no way to tell the two apart once flattened by
    /// `Conway::active_permission_patterns`/`active_structured_allow_rules`/
    /// `active_shell_prefix_grants`. A session-scoped grant belongs to the
    /// conversation that earned it, not to whatever fresh session replaces
    /// it. Nothing is lost by resetting them: the very next `/settings` open
    /// repopulates all three LIVE from `Conway`'s own broker/dispatcher
    /// regardless of what this struct carried across, so any rule that is
    /// still genuinely configured (as opposed to merely having been granted
    /// for the old session) reappears there on its own.
    ///
    /// **Session-scoped grants in the live broker are dropped here too.**
    /// `grants` is required, not optional: resetting the TUI's mirrors while
    /// leaving the shared `PermissionBroker`'s own session-scoped rows in
    /// place is exactly the leak DOGFOOD 4 found (`/new`, `/resume` and
    /// `/distill` all carried an "always allow" into the next session), so
    /// the one funnel that adopts a session takes the revoker as an argument
    /// rather than trusting every caller to pair two calls.
    pub fn reset_for_new_session(
        &mut self,
        root: AgentId,
        grants: &(impl RevokeSessionGrants + ?Sized),
    ) {
        grants.revoke_all_session_scoped_grants();
        let fresh = AppState::new(root);
        let AppState {
            transcript: _,
            tree: _,
            model_decision_history: _,
            input: _,
            cursor: _,
            mode: _,
            // RESET: a partially-typed vim command (pending operator, open
            // Visual selection, undo/redo stacks, the unnamed register)
            // belongs to the OLD session's own in-progress editing -- see
            // `state::editor_mode`'s own doc. `editor_mode` itself (the
            // `emacs`/`vim` session preference) is CARRIED, below.
            vim: _,
            permission_mode,
            default_permission_mode,
            permission_paths,
            grants_path,
            project_config_ignored,
            permission_grants: _,
            structured_allow_rules: _,
            permission_denies,
            permission_prompts,
            structured_deny_rules,
            structured_prompt_rules,
            shell_prefix_grants: _,
            hook_rules,
            plugin_browser,
            subprocess_plugins,
            mcp_plugins,
            claude_compat_plugins,
            plugins_open: _,
            plugins_selected: _,
            permission_grant_scope: _,
            permission_prompt_armed_at: _,
            permission_confirm_always: _,
            scroll: _,
            follow_tail: _,
            queued_prompts: _,
            agent_view_open: _,
            palette_selected: _,
            palette_stem: _,
            agent_selected: _,
            agent_visibility: _,
            focused_agent: _,
            activity: _,
            focused_agent_usage: _,
            session_head_seq: _,
            pending_ask_modal: _,
            pending_intent_confirm: _,
            pending_trust_preview: _,
            pending_ui_form: _,
            pending_skill_proposal: _,
            skill_propose_in_flight: _,
            pending_distill: _,
            distill_in_flight: _,
            distill_generation: _,
            ask_in_flight: _,
            ask_child: _,
            ask_started_at: _,
            ask_abandoned: _,
            awaiting_agents: _,
            shell_in_flight: _,
            last_shell_command: _,
            budget_warned_agents: _,
            switch_lineage: _,
            pending_focus_notice: _,
            pending_exit_word: _,
            spawn_role_or_model: _,
            modal_scroll: _,
            spinner_frame: _,
            turn_started_at: _,
            awaiting_permission_since: _,
            running_tool_since: _,
            turn_running_tokens: _,
            turn_transcript_start: _,
            focused_model: _,
            focused_model_max_context: _,
            focused_model_max_context_source: _,
            focused_model_cache_reporting: _,
            focused_ctx_tokens: _,
            focused_seen_segments: _,
            git_branch,
            cwd_display,
            status_line_config,
            tool_preview_lines,
            model_max_context,
            model_max_context_source,
            model_cache_reporting,
            show_reasoning,
            show_timestamps,
            busy_input,
            editor_mode,
            held_prompts: _,
            pending_steers: _,
            history,
            history_cap,
            history_index: _,
            history_draft: _,
            help_open: _,
            settings_open: _,
            settings_selected: _,
            settings_collapsed_groups: _,
            provider_entries: _,
            provider_status: _,
            provider_status_loading: _,
            default_role_snapshot: _,
            default_model_snapshot: _,
            known_role_names: _,
            configured_models: _,
            // RESET here on purpose -- see this method's own doc for why
            // `/new`/`/resume` disagree and where each one's own
            // carry-across actually lives.
            model_pin: _,
            role_pin: _,
            configured_backend_ids: _,
            role_listing: _,
            model_picker_active: _,
            session_picker_active: _,
            plugin_commands,
            agent_names,
            plugin_status_contributions,
            permission_decision_pending: _,
            keybindings,
            diff_track: _,
            diff_baseline: _,
            tool_diffs: _,
            mention_scan_root,
            mention_anchor: _,
            mention_mode: _,
            mention_candidates: _,
            mention_capped: _,
            mention_selected: _,
            mention_dismissed_for: _,
            mention_scan_cache: _,
            pending_mention_scan_request: _,
            mention_scan_in_flight: _,
            mention_via_paste: _,
        } = std::mem::replace(self, fresh);

        self.permission_mode = permission_mode;
        self.default_permission_mode = default_permission_mode;
        self.permission_paths = permission_paths;
        self.grants_path = grants_path;
        self.project_config_ignored = project_config_ignored;
        self.permission_denies = permission_denies;
        self.permission_prompts = permission_prompts;
        self.structured_deny_rules = structured_deny_rules;
        self.structured_prompt_rules = structured_prompt_rules;
        self.hook_rules = hook_rules;
        self.plugin_browser = plugin_browser;
        self.subprocess_plugins = subprocess_plugins;
        self.mcp_plugins = mcp_plugins;
        self.claude_compat_plugins = claude_compat_plugins;
        self.git_branch = git_branch;
        self.cwd_display = cwd_display;
        self.status_line_config = status_line_config;
        self.tool_preview_lines = tool_preview_lines;
        self.model_max_context = model_max_context;
        self.model_max_context_source = model_max_context_source;
        self.model_cache_reporting = model_cache_reporting;
        self.show_reasoning = show_reasoning;
        self.show_timestamps = show_timestamps;
        self.busy_input = busy_input;
        self.editor_mode = editor_mode;
        self.history = history;
        self.history_cap = history_cap;
        self.plugin_commands = plugin_commands;
        self.agent_names = agent_names;
        self.plugin_status_contributions = plugin_status_contributions;
        self.keybindings = keybindings;
        self.mention_scan_root = mention_scan_root;
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
        // Board item `01M2PGS1GGNDNSA0A6E074G4VF`: re-push the staged switch
        // notice after EVERY clear, and do not consume it here. A single
        // switch can drive more than one focus transition, and a
        // consume-once re-push at the `try_focus_agent` call site survives
        // exactly one clear; re-pushing from inside `focus_agent` survives
        // however many actually occur, because this is the only function
        // that clears. `App::submit` drops the staged text, so it cannot
        // leak into a later, unrelated focus change.
        if let Some(text) = self.pending_focus_notice.clone() {
            self.transcript.push(Entry::Notice { text });
        }
        self.scroll = 0;
        self.follow_tail = true;
        // The bare-`exit` hint belongs to the agent it was shown for: a
        // focus switch that never went through `App::submit` (Enter on an
        // agent-panel row) must not let a first `exit` on the new agent
        // count as the confirming second press.
        self.pending_exit_word = None;
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
        // Board item `01M2V60KWK9AYX3J7V5TPJZN7Q`: the permission clock is
        // per focused-agent for exactly the same reason the turn clock
        // above is. The activity reset a few lines up already stops the
        // `awaiting permission…` rung from rendering for the new focus, so
        // this is the value, not the gate -- but leaving another agent's
        // wait stamped here would make a later refocus-back read a clock
        // that had kept running through the time the operator spent
        // elsewhere.
        self.awaiting_permission_since = None;
        // Board item `01M2VDD6MQG7H73AHGX8HJGV2J`: and the tool-execution
        // clock for the same reason again -- the call it times belongs to
        // the agent being focused away from, and the activity reset above
        // has already taken its rung down.
        self.running_tool_since = None;
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
        self.focused_model_max_context_source = None;
        self.focused_model_cache_reporting = None;
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
    ///
    /// Board item `01M331DN5YF9J1T12QRASZCHV7`: builds the rule via
    /// [`EditingPatternState::preview_rule`] rather than re-deriving it
    /// here, so the rule this method actually submits can never drift from
    /// the one the view previewed to the operator one frame earlier.
    pub fn submit_editing_pattern(&mut self) -> Option<(conway::Rule, conway::PermissionScope)> {
        if !matches!(self.mode, Mode::EditingPattern(_)) {
            return None;
        }
        let Mode::EditingPattern(ed) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        let rule = ed.preview_rule();
        self.mode = Mode::AwaitingPermission(ed.prompt);
        self.modal_scroll = 0;
        Some((rule, self.permission_grant_scope))
    }

    /// Board item `01M32EBPWZZG6EA77ZG5KYC8KQ`: opens the session-scoped
    /// shell-prefix grant editor from a permission prompt. Only callable
    /// while a prompt is showing (`Mode::AwaitingPermission`) AND the
    /// pending call declares `RenderKind::ShellCommand` -- the exact
    /// complement of [`Self::offer_editing_pattern`]'s own "only for
    /// `Structured`" gate, so the two editors partition every tool's
    /// `render_kind` between them and never both offer for the same call.
    /// The [`PendingPrompt`] is MOVED out of `mode` (it is not `Clone`), so
    /// cancel restores it, submit resolves it -- mirroring
    /// `offer_editing_pattern`'s own non-stacking shape exactly.
    pub fn offer_editing_shell_prefix(&mut self) {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return;
        };
        if pending.request.render_kind != conway::RenderKind::ShellCommand {
            return;
        }
        let Mode::AwaitingPermission(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            unreachable!()
        };
        self.mode = Mode::EditingShellPrefix(EditingShellPrefixState::from_prompt(prompt));
        self.modal_scroll = 0;
    }

    /// Cancels the shell-prefix editor and returns the prompt to the
    /// screen unresolved -- the operator can press `y`/`a`/`n`/`p` again.
    /// Mirrors [`Self::cancel_editing_pattern`] exactly.
    pub fn cancel_editing_shell_prefix(&mut self) {
        if !matches!(self.mode, Mode::EditingShellPrefix(_)) {
            return;
        }
        let Mode::EditingShellPrefix(ed) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        self.mode = Mode::AwaitingPermission(ed.prompt);
        self.modal_scroll = 0;
    }

    /// Submits the shell-prefix editor: takes the operator's (possibly
    /// edited) `input` verbatim as the prefix, restores the prompt to
    /// `AwaitingPermission` (so the app loop's dispatch can resolve it with
    /// the existing `resolve_current_prompt` path), and returns the prefix
    /// and scope for the key handler to wrap in an
    /// `Action::GrantSessionShellPrefix`. Returns `None` if no editor is
    /// open. The grant covers FUTURE calls; THIS call is resolved
    /// separately by the dispatch arm as `AllowOnce` -- mirrors
    /// [`Self::submit_editing_pattern`]'s own shape exactly. A blank
    /// `input` (after trim) still returns `Some` -- the APP LOOP's
    /// `PermissionBroker::remember_shell_prefix_grant` is what refuses an
    /// empty prefix (see that method's own doc); this layer does not
    /// duplicate that check, it only reports what the operator actually
    /// typed.
    ///
    /// **The compound-command exclusion is enforced HERE too, before the
    /// call ever leaves this layer.** If `conway::permission_pattern::
    /// shell_command_is_compound` refuses the (trimmed) `input`, this sets
    /// `EditingShellPrefixState::error` (shown by the view, mirroring
    /// [`crate::tui::state::modal::AddProviderCredentialState::error`]'s
    /// own contract), returns `None`, and leaves the editor OPEN --
    /// exactly like a rejected credential, never a silent no-op and never
    /// an `AllowOnce` this method has no business granting on a refused
    /// submission. This is a SECOND enforcement of the SAME rule
    /// `PermissionBroker::remember_shell_prefix_grant`/`shell_prefix_grant_
    /// allows` apply -- the broker's own check is the one that cannot be
    /// bypassed; this one exists so the operator finds out immediately,
    /// in the editor, rather than by the grant quietly failing to cover
    /// anything later.
    pub fn submit_editing_shell_prefix(&mut self) -> Option<(String, conway::PermissionScope)> {
        let Mode::EditingShellPrefix(ed) = &mut self.mode else {
            return None;
        };
        let trimmed = ed.input.trim();
        if conway::permission_pattern::shell_command_is_compound(trimmed) {
            ed.error = Some(compound_prefix_refusal_message().to_string());
            return None;
        }
        let Mode::EditingShellPrefix(ed) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            unreachable!()
        };
        let prefix = ed.input.clone();
        self.mode = Mode::AwaitingPermission(ed.prompt);
        self.modal_scroll = 0;
        Some((prefix, self.permission_grant_scope))
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
                    // `01M2VDD6MQG7H73AHGX8HJGV2J`: the `running <tool>…`
                    // rung is gone with the agent that owned it -- stop the
                    // clock behind it too. NOT folded into
                    // `clear_turn_state`: that runs at `TurnFinished`, which
                    // lands BEFORE the tool calls it clears state for are
                    // even dispatched.
                    self.running_tool_since = None;
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
                    // `01M2VDD6MQG7H73AHGX8HJGV2J`: a new turn is the normal
                    // end of the PREVIOUS turn's `running <tool>…` rung --
                    // the model only gets to start another turn once those
                    // calls have come back. The rung's clock stops with it.
                    self.running_tool_since = None;
                    // T4: watermark the transcript so the turn-end summary
                    // can only attach to a block THIS turn produced (see
                    // `turn_transcript_start`).
                    self.turn_transcript_start = self.transcript.len();
                    // Board item `01M1YVHKTQVXJRDSRYT3TCRXFX`: review round 1
                    // removed this arm's own `clear_one_pending_steer` call --
                    // it could only ever fire for the FOCUSED agent's own
                    // live stream, the identical structural gap the review's
                    // delivery-scoping finding named. `App::
                    // flush_ready_queues` (`app/busy_input.rs`) now clears a
                    // pending steer's visibility by polling `SessionHandle::
                    // turn_in_progress`, focus-independent, the same
                    // mechanism that delivers `held_prompts`.
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
            // - `turn_running_tokens`: the per-turn "added this turn"
            //   figure, reset on `TurnStarted`/`focus_agent`. Accumulated
            //   only while a turn is in flight (`turn_started_at.is_some()`).
            // - `focused_ctx_tokens`: the CUMULATIVE context-occupancy
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
            // (`ModelRef::to_string()`, e.g. `anthropic/claude-sonnet-5`)
            // and look up its max context window from the model-metadata
            // map populated at `App::new`. The status line's `model` field
            // renders the display name; `ctx%` divides `focused_ctx_tokens`
            // by this max. `app.rs` already captures the whole
            // `ModelDecision` envelope for `/why`
            // (`model_decision_history`), but that field is intentionally
            // left untouched by `apply` -- this arm only updates the
            // display-name/max-context pair on the focused agent's own
            // stream.
            Event::ModelDecision { chosen, reason, .. } => {
                if env.agent == self.focused_agent {
                    let name = chosen.to_string();
                    let max = self.model_max_context.get(&name).copied().or_else(|| {
                        // Fall back to a bare `model` lookup (no
                        // backend prefix) -- some metadata files key
                        // on the model id alone.
                        self.model_max_context.get(chosen.model.as_str()).copied()
                    });
                    // Board item 01M1ZJ796E0YP6Y8QWS8HB0AVB: the identical
                    // lookup (same key, same bare-model-id fallback) against
                    // `model_max_context_source`, so `focused_model_max_
                    // context` and `focused_model_max_context_source` are
                    // never resolved from different keys.
                    let source = self
                        .model_max_context_source
                        .get(&name)
                        .copied()
                        .or_else(|| {
                            self.model_max_context_source
                                .get(chosen.model.as_str())
                                .copied()
                        });
                    // Board item `01M2NS0996E139VN5R8W4PGD8V`: keyed on the
                    // bare backend id alone (`model_cache_reporting`'s own
                    // doc) -- no bare-model-id fallback needed, unlike
                    // `max`/`source` above, since the key here was never
                    // ambiguous with a `"backend/model"` string to begin
                    // with.
                    let cache_reporting = self
                        .model_cache_reporting
                        .get(chosen.backend.as_str())
                        .copied();
                    self.focused_model = Some(name);
                    self.focused_model_max_context = max;
                    self.focused_model_max_context_source = source;
                    self.focused_model_cache_reporting = cache_reporting;

                    // Board item A1d ("say why a turn fell back"): a
                    // one-line dim notice on the turn itself, not only
                    // reachable via `/why` -- the incident that motivated it
                    // (six silent primary/fallback flips in one real
                    // session, `route_reason.after` empty every time) was
                    // invisible in the transcript, not merely unexplained
                    // on request. Fires only when `after` actually names
                    // something (a `Fallback` whose predecessors were all
                    // pinned/primary-selected, or any other reason kind,
                    // renders nothing here -- `/why` remains the place to
                    // ask for a reason that has nothing to add). Uses the
                    // EXISTING `Entry::Notice` variant/rendering -- no new
                    // transcript entry kind, no `view/transcript.rs`
                    // change.
                    //
                    // AMENDED: `after` alone was not enough. A candidate
                    // refused at ADMISSION is never attempted, so it
                    // produces no `AttemptFailure` and `after` stays
                    // legitimately empty -- and that is the commonest real
                    // cause of a fallback (a chain head whose window cannot
                    // hold the request). Those skips now arrive on
                    // `RoutingReason::Fallback::skipped`, and the notice
                    // fires on EITHER list being non-empty, so a
                    // mid-session model change caused by an admission skip
                    // is no longer silent.
                    if let RoutingReason::Fallback { after, skipped, .. } = reason {
                        if let Some(text) = fallback_notice_text(chosen, after, skipped) {
                            self.transcript.push(Entry::Notice { text });
                        }
                    }
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
                // Board item 01M1YVEJB6GAPST5YZET4KZZE2: the FIRST time this
                // session sees `edit`/`write` propose a call against a given
                // `path`, read its current on-disk bytes ONCE and seed BOTH
                // `diff_track` (which folds forward from here, for per-call
                // settled diffs) and `diff_baseline` (which never moves
                // again, and is what `/diff` diffs against) -- this is the
                // ONLY disk read this whole diff feature performs per path.
                // Read here, at PROPOSE time (before the call runs), so the
                // captured bytes are genuinely the "before" state -- reading
                // at finish time, or at `/diff` time, would already see this
                // call's own result on disk, which is exactly the defect
                // board item `01M2V6HMBAWKM0GG90J14K4Q8F` records. A read
                // failure (missing file, permission, non-UTF-8) is folded to
                // an empty baseline rather than skipped entirely: an `edit`
                // against a nonexistent/unreadable path is going to fail
                // anyway (surfaced by the tool's own error text), and a
                // `write` creating a brand-new file legitimately has no
                // prior content -- both cases want "nothing here yet", not
                // "diff unavailable".
                if matches!(tool.as_str(), "edit" | "write") {
                    if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                        if !self.diff_track.contains_key(path) {
                            let before = std::fs::read_to_string(path).unwrap_or_default();
                            self.diff_baseline.insert(path.to_string(), before.clone());
                            self.diff_track.insert(path.to_string(), before);
                        }
                    }
                }
                self.set_tree_status(env.agent, NodeStatus::Running);
                if env.agent == self.focused_agent {
                    self.activity = Activity::RunningTool(tool.to_string());
                    // Board item `01M2VDD6MQG7H73AHGX8HJGV2J`: start the
                    // clock the status line's elapsed figure reads while
                    // this rung is up. The SAME statement pair writes the
                    // name and the clock, which is the whole of the
                    // multi-call semantics: with several calls in flight the
                    // rung names the last one proposed, so the number is
                    // that call's age and not the batch's. Stamped
                    // unconditionally, not `get_or_insert` -- a later
                    // proposal renames the rung, and a figure that kept
                    // running from an earlier call would then be timing a
                    // call the row no longer names. See the field's own doc
                    // for the alternatives this rules out.
                    self.running_tool_since = Some(Instant::now());
                }
            }
            Event::PermissionRequested { call_id, .. } => {
                self.set_tool_status(call_id, ToolStatus::AwaitingPermission);
                self.set_tree_status(env.agent, NodeStatus::AwaitingPermission);
                if env.agent == self.focused_agent {
                    self.activity = Activity::AwaitingPermission;
                    // Board item `01M2V60KWK9AYX3J7V5TPJZN7Q`: start the
                    // wait clock the status line's elapsed figure reads
                    // while this rung is up. Stamped unconditionally, not
                    // `get_or_insert`: a second `PermissionRequested` for a
                    // DIFFERENT call is a new wait, and the figure names
                    // the wait the operator is being asked about now, not
                    // the age of the oldest unanswered one.
                    self.awaiting_permission_since = Some(Instant::now());
                    // `01M2VDD6MQG7H73AHGX8HJGV2J`: the prompt REPLACES the
                    // `running <tool>…` rung with `awaiting permission…`,
                    // so the clock behind the replaced rung stops here.
                    // Nothing is being executed while the prompt is open --
                    // letting it keep running would bank the operator's own
                    // deliberation time into the call's execution time.
                    self.running_tool_since = None;
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
                    // `01M2VDD6MQG7H73AHGX8HJGV2J`: belt and braces. The
                    // turn ending lands BEFORE this turn's own tool calls
                    // are dispatched, so there is normally no tool clock
                    // running here; a previous batch's stamp surviving into
                    // an `Idle` rung would be a value nothing reads but
                    // everything has to reason about.
                    self.running_tool_since = None;
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
                // Stashed for the sibling `Event::PermissionDecision` (always
                // emitted right after this one, same call, same tick --
                // `PermissionBroker::decide`'s own emission order) to build
                // the transcript's dim decision line from -- see
                // `AppState::permission_decision_pending`'s own doc.
                self.permission_decision_pending
                    .insert(call_id.clone(), *decision);
                // Board item `01M2V60KWK9AYX3J7V5TPJZN7Q`: the wait is
                // over -- stop the clock the `awaiting permission…` rung
                // reads. Cleared for EVERY resolution kind, including the
                // ones that never reached the operator at all (a cached or
                // pattern-matched allow emits this event with no prompt
                // ever having opened, so there is no clock running to stop
                // and the clear is a no-op). Scoped to the focused agent
                // for the same reason the stamp above is: a background
                // agent resolving ITS prompt must not zero the clock the
                // operator is currently reading for a different one.
                if env.agent == self.focused_agent {
                    self.awaiting_permission_since = None;
                    // Board item `01M2X463TDVV5TG53M3X1M3M6V`, the other
                    // half: **what a DENIED call shows.** A denial emits no
                    // `ToolCallStarted` and no `ToolCallFinished` (the
                    // runner returns at its `Deny` arm), so this is the
                    // last event that call ever produces -- whatever rung
                    // is up now stays up until the next turn-level event.
                    // `awaiting permission` is false the moment the
                    // operator answers, and `running <tool>` is false for a
                    // call that will never run, so the rung comes down to
                    // `Idle`: precisely where the event stream left it
                    // before the proposal (the binding order `TurnFinished
                    // < ToolCallProposed*` means the turn is already over
                    // by then), and the follow-up turn the denial feedback
                    // provokes moves it on within a tick.
                    //
                    // Only a rung that is ABOUT THIS CALL comes down. A
                    // parallel batch keeps a sibling call's rung up --
                    // denying one call says nothing about the others, and
                    // blanking the bar while another tool genuinely runs
                    // would trade one false rung for another. The rung
                    // names a tool rather than a `call_id`, so "about this
                    // call" is: the wait rung (which only ever belongs to
                    // the call being asked about), or a running rung naming
                    // the denied call's own tool. Two concurrent calls of
                    // the SAME tool can make that match the wrong sibling;
                    // the cost is one `Idle` frame until the next event,
                    // which is why it is not worth a per-call rung stack.
                    if matches!(decision, Kind::Denied | Kind::DeniedWithFeedback) {
                        let denied_tool = self.tool_name_for_call(call_id);
                        let rung_is_about_this_call = match &self.activity {
                            Activity::AwaitingPermission => true,
                            Activity::RunningTool(name) => {
                                denied_tool.as_deref() == Some(name.as_str())
                            }
                            _ => false,
                        };
                        if rung_is_about_this_call {
                            self.activity = Activity::Idle;
                            self.running_tool_since = None;
                        }
                    }
                }
            }
            // The TUI renders a dim one-line entry under the tool call it
            // belongs to, for a PROMPTED decision only -- `waited_ms` is
            // `Some` exactly when this call reached `PermissionGate::check`
            // live (`crate::log::PermissionDecisionSource::Operator`'s own
            // doc, in `conway-core`): every other source resolves without
            // any wait, so a `None` here means this decision never reached
            // the operator at all, and nothing is rendered for it -- a
            // silent pattern/rule/hook/mode resolution would otherwise
            // clutter the transcript with a line for every one of the vast
            // majority of calls that never involve the operator.
            Event::PermissionDecision {
                call_id,
                waited_ms,
                feedback,
                ..
            } => {
                self.apply_permission_decision(call_id, *waited_ms, feedback.clone());
            }
            Event::ToolCallStarted { call_id } => {
                self.set_tool_status(call_id, ToolStatus::Running);
                // Board item `01M2X463TDVV5TG53M3X1M3M6V`: **this is the
                // event that puts the `running <tool>…` rung (back) up.**
                // A call that was prompted for permission executed under
                // `awaiting permission… 0s` for its whole run: the prompt
                // replaced the rung, `PermissionResolved` cleared only the
                // wait CLOCK, and nothing moved `activity` off
                // `AwaitingPermission` until the next turn-level event --
                // so the label said conway was blocked on an operator who
                // had already answered, wearing a figure frozen at zero
                // because `01M2V60KWK9AYX3J7V5TPJZN7Q` correctly stops that
                // clock on resolution.
                //
                // Carried HERE rather than on `PermissionResolved`, which
                // also knows the decision: this event fires if and only if
                // the call is actually about to run. `execute_one` returns
                // at its `PermissionOutcome::Deny` arm before emitting it
                // (`conway-runtime`'s `tools/runner.rs`), and it is emitted
                // AFTER the concurrency semaphore's permit is acquired, so
                // the rung means "executing", not "allowed and queued".
                // It is emitted for every allowed call, prompted or not, so
                // this is one unconditional rule and not a special case
                // bolted onto the permission path.
                //
                // The clock is RE-STAMPED with the name, exactly as the
                // `ToolCallProposed` arm stamps them together: the figure
                // measures execution, and the operator's deliberation is
                // separately (and durably) recorded as
                // `PermissionDecisionRecord::waited_ms` and shown in the
                // `allowed once · waited 53s` note. Name and clock are
                // still only ever written in one statement pair, so the row
                // can never show one call's name wearing another's age --
                // see `Self::running_tool_since`'s own doc.
                if env.agent == self.focused_agent {
                    if let Some(tool) = self.tool_name_for_call(call_id) {
                        self.activity = Activity::RunningTool(tool);
                        self.running_tool_since = Some(Instant::now());
                    }
                }
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
            // Board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`: a same-candidate
            // stream retry discarded the current attempt's partial deltas.
            // `apply_stream_restarted` truncates the in-progress Assistant/
            // Reasoning entries back to their pre-delta content and appends
            // a visible discard notice -- see that method's own doc.
            Event::StreamRestarted {
                attempt,
                discarded_text_chars,
                discarded_thinking_chars,
                ..
            } => {
                self.apply_stream_restarted(
                    *attempt,
                    *discarded_text_chars,
                    *discarded_thinking_chars,
                );
            }
            // A keep_alive root's turn-scoped budget trip (`max_steps`/
            // `max_tool_calls`) no longer ends the session
            // (`Event::AgentFinished`) -- it aborts just the current turn
            // and the harness returns to idling for the operator's next
            // prompt. Without this arm, that would fall into the wildcard
            // below and be silently invisible: the operator would see the
            // agent simply stop responding, the exact "the session ends
            // instead of just the turn" failure mode moved one layer down,
            // from the runtime into this renderer. Unconditional (not
            // gated on `env.agent == self.focused_agent`), matching
            // `Event::Error`/`Event::AgentProgress`'s own convention just
            // above: a root's turn ending is worth surfacing even if the
            // operator is currently focused elsewhere.
            Event::TurnAborted {
                agent_id, limit, ..
            } => {
                self.transcript.push(Entry::Notice {
                    text: format!("turn ended: {limit} reached; type to continue"),
                });
                // Board item `01M44PK0HNKWBXK86PWTHD6N7C`: this ends the
                // turn exactly like `Event::TurnFinished` does (same
                // `end_keep_alive_turn` reset, per this variant's own doc)
                // but used to fall through without the status-bar reset
                // `TurnFinished`'s own arm performs, leaving `activity`
                // wedged at whatever working rung was up (e.g. "thinking…
                // 31s") until the NEXT turn's first event overwrote it --
                // visibly idle to the harness, visibly busy to the
                // operator. Gated on the focused agent, mirroring
                // `TurnFinished`'s own gate, since `activity` only ever
                // describes the focused agent.
                if *agent_id == self.focused_agent {
                    self.activity = Activity::Idle;
                    self.clear_turn_state();
                    self.running_tool_since = None;
                }
            }
            // Board item `01M3XGPGT5W7GABVTC7F2NA0C9`: the operator-abort
            // sibling of `Event::TurnAborted` immediately above -- same
            // "the agent is still alive, just idling for the next prompt"
            // contract (the first `Ctrl-C` mid-turn, or `busy_input =
            // "interrupt"`'s own cancel-then-send), a different cause, so
            // this gets its own wording naming the operator's own reason
            // rather than reusing "reached" (a budget-dimension word that
            // would misdescribe an operator abort).
            //
            // Board item `01M44PK089DF2M9TM3C4P5CKMZ`: a bare `Ctrl-C` (the
            // ONLY caller that stamps a reason other than
            // [`INTERRUPT_RESEND_REASON`], today always `"user cancel"`)
            // genuinely leaves the operator with nothing queued -- "type to
            // continue" is accurate there. `App::interrupt_and_send`'s two
            // callers (`busy_input = "interrupt"` and `F2`/`send_now`) both
            // immediately resend the very message that triggered the abort
            // once the agent reports idle again (`app.rs`'s own doc on that
            // method), so telling the operator to "type to continue" after
            // THAT abort describes a step that already happened on its own
            // -- see that reason constant's own doc for why a literal
            // string match, not a new `Event` field, is the chosen seam
            // here.
            Event::TurnAbortedByUser { agent_id, reason } => {
                let text = if reason == super::app::INTERRUPT_RESEND_REASON {
                    "sent now -- the reply in progress was stopped".to_string()
                } else {
                    format!("turn aborted ({reason}); type to continue")
                };
                self.transcript.push(Entry::Notice { text });
                // Board item `01M44PK0HNKWBXK86PWTHD6N7C`: the Ctrl-C abort
                // case of the same bug fixed on `Event::TurnAborted` just
                // above -- see that arm's comment. A first Ctrl-C mid-turn
                // left the status bar reading "running · thinking… 31s"
                // indefinitely even though the harness was already idle and
                // accepting the next prompt.
                if *agent_id == self.focused_agent {
                    self.activity = Activity::Idle;
                    self.clear_turn_state();
                    self.running_tool_since = None;
                }
            }
            // Board item A5.6: a child (or the root) crossed 80% of one of
            // its own budget dimensions -- the model-facing wrap-up notice
            // this event's own doc says it mirrors already reached the
            // crossing agent's own transcript as a `SystemNote`/
            // `AgentProgress`; this arm is what lets an operator watching a
            // DIFFERENT agent (or the `/agents` panel as a whole) learn
            // about it too, without switching focus. Unconditional (not
            // gated on `env.agent == self.focused_agent`), matching
            // `Event::TurnAborted`'s own convention immediately above --
            // and `budget_warned_agents` records the crossing regardless
            // of focus, since the panel shows every agent's row at once.
            Event::BudgetWarning {
                agent_id, limit, ..
            } => {
                self.budget_warned_agents.insert(*agent_id);
                self.transcript.push(Entry::Notice {
                    text: format!("agent {agent_id} is nearing a budget limit ({limit})"),
                });
            }
            _ => {}
        }
    }
}

/// Board item `01M44PK0HNKWBXK86PWTHD6N7C`: `Event::TurnAborted` and
/// `Event::TurnAbortedByUser` both end the current turn exactly like
/// `Event::TurnFinished` (same `end_keep_alive_turn` reset, per both
/// variants' own doc), but their `apply` arms used to push only the
/// transcript notice and never reset `activity`/`turn_started_at` --
/// leaving the status bar reading "running · thinking… 31s" indefinitely
/// after a Ctrl-C abort, even though the harness was already idle and
/// accepting the operator's next prompt.
#[cfg(test)]
mod turn_abort_status_reset {
    use super::fixtures::envelope;
    use super::*;

    #[test]
    fn turn_aborted_by_user_resets_activity_to_idle_for_the_focused_agent() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        state.activity = Activity::Responding;
        assert!(state.turn_started_at.is_some());

        state.apply(&envelope(
            session,
            agent,
            Event::TurnAbortedByUser {
                agent_id: agent,
                reason: "user cancel".to_string(),
            },
        ));

        assert_eq!(
            state.activity,
            Activity::Idle,
            "a Ctrl-C abort must leave the status bar idle, not wedged at the \
             working rung it interrupted"
        );
        assert!(
            state.turn_started_at.is_none(),
            "the elapsed clock must stop along with the activity rung"
        );
    }

    /// Scoping companion: a background agent's own abort must not touch the
    /// status the operator is reading for a DIFFERENT focused agent.
    #[test]
    fn turn_aborted_by_user_for_a_non_focused_agent_leaves_activity_untouched() {
        let session = SessionId::new();
        let focused = AgentId::new();
        let other = AgentId::new();
        let mut state = AppState::new(focused);
        state.activity = Activity::Responding;

        state.apply(&envelope(
            session,
            other,
            Event::TurnAbortedByUser {
                agent_id: other,
                reason: "user cancel".to_string(),
            },
        ));

        assert_eq!(
            state.activity,
            Activity::Responding,
            "an unrelated agent's abort must not reset the focused agent's activity"
        );
    }

    #[test]
    fn turn_aborted_resets_activity_to_idle_for_the_focused_agent() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        state.activity = Activity::RunningTool("bash".to_string());
        assert!(state.turn_started_at.is_some());

        state.apply(&envelope(
            session,
            agent,
            Event::TurnAborted {
                agent_id: agent,
                limit: "max_steps=40".to_string(),
                steps_this_turn: 40,
            },
        ));

        assert_eq!(
            state.activity,
            Activity::Idle,
            "a turn-scoped budget trip must leave the status bar idle"
        );
        assert!(state.turn_started_at.is_none());
    }
}

/// Board item A1d: the one-line dim notice `apply`'s `Event::ModelDecision`
/// arm pushes onto the transcript when a turn routed past a named
/// candidate. `None` only when BOTH lists are empty -- an ordinary
/// primary/pinned selection, or a `Fallback` that genuinely passed nothing
/// over, says nothing rather than announcing a fallback with no content to
/// show.
///
/// Two sources, one line. `after` holds candidates that were ATTEMPTED and
/// failed; `admission_skips` holds candidates refused before any request
/// was sent (`RoutingReason::Fallback::skipped`), which have no attempt and
/// so can never appear in `after`. Reading only `after` is what made an
/// admission-time fallback silent: nothing was attempted, so there was
/// nothing to announce, and the operator discovered the model had changed
/// by reading the model name on the reply.
///
/// Every number rendered here is sourced, never recomputed: an attempted
/// failure's `error` text (`conway_plugin_routing::router::render_reason` /
/// the backend's own refusal `Display`) and a skip's
/// `RoutingReason::skip_detail` -- `conway-core`'s single skip renderer,
/// shared with `/why` and `conway routes explain`.
fn fallback_notice_text(
    chosen: &conway::ModelRef,
    after: &[conway::AttemptFailure],
    admission_skips: &[conway::RoutingReason],
) -> Option<String> {
    let mut parts: Vec<String> = after
        .iter()
        .map(|f| format!("{} skipped: {}", f.model, f.error))
        .collect();
    parts.extend(
        admission_skips
            .iter()
            .filter_map(|reason| reason.skip_detail())
            .map(|(model, detail)| format!("{model} skipped: {detail}")),
    );
    if parts.is_empty() {
        return None;
    }
    Some(format!("routed to {chosen} — {}", parts.join("; ")))
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

/// Board item `01M2V60KWK9AYX3J7V5TPJZN7Q`: the lifecycle of
/// [`AppState::awaiting_permission_since`], the clock the status line's
/// elapsed figure reads while a permission prompt is open.
///
/// These pin the STATE half of the fix. The render half -- that the figure
/// on screen is that clock's value and not a literal `0s` -- is pinned in
/// `view/status.rs`'s own tests, against the real rendered buffer.
#[cfg(test)]
mod awaiting_permission_clock {
    use super::fixtures::envelope;
    use super::*;
    use conway::backend::StopReason;
    use conway::{PermissionDecisionKind, SessionId};

    fn awaiting_state() -> (SessionId, AgentId, AppState) {
        let session = SessionId::new();
        let agent = AgentId::new();
        let state = AppState::new(agent);
        (session, agent, state)
    }

    /// The reproduction, at the state layer. The real event order that
    /// reaches a prompt is `TurnStarted` -> `TurnFinished` ->
    /// `PermissionRequested` (`conway-runtime`'s `agent_loop` module doc
    /// makes `TurnFinished < ToolCallProposed*` binding), so by the time
    /// the prompt opens `clear_turn_state` has ALREADY zeroed
    /// `turn_started_at`. That is the whole defect: the elapsed figure read
    /// that field, found `None`, and rendered `0s` for the entire wait.
    #[test]
    fn a_prompt_opens_with_turn_started_at_already_cleared_and_stamps_its_own_clock() {
        let (session, agent, mut state) = awaiting_state();

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        assert!(state.turn_started_at.is_some());

        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: StopReason::ToolUse,
            },
        ));
        state.apply(&envelope(
            session,
            agent,
            Event::PermissionRequested {
                call_id: "tc_1".into(),
                rendered: "write notes.md".into(),
            },
        ));

        assert_eq!(state.activity, Activity::AwaitingPermission);
        assert!(
            state.turn_started_at.is_none(),
            "board item `01M2V60KWK9AYX3J7V5TPJZN7Q`'s premise: no turn is in flight \
             while a prompt is open, so `turn_started_at` cannot be the clock the wait \
             is measured from"
        );
        assert!(
            state.awaiting_permission_since.is_some(),
            "the prompt must start its own wait clock"
        );
    }

    /// The decision ends the wait, for an allow and for a denial alike.
    #[test]
    fn resolving_the_prompt_stops_the_clock() {
        for decision in [
            PermissionDecisionKind::AllowOnce,
            PermissionDecisionKind::Denied,
        ] {
            let (session, agent, mut state) = awaiting_state();
            state.apply(&envelope(
                session,
                agent,
                Event::PermissionRequested {
                    call_id: "tc_1".into(),
                    rendered: "write notes.md".into(),
                },
            ));
            assert!(state.awaiting_permission_since.is_some());

            state.apply(&envelope(
                session,
                agent,
                Event::PermissionResolved {
                    call_id: "tc_1".into(),
                    decision,
                },
            ));
            assert!(
                state.awaiting_permission_since.is_none(),
                "the wait is over once the operator decides ({decision:?})"
            );
        }
    }

    /// Scoping. A background agent's prompt must neither start nor stop
    /// the clock the operator is reading for the FOCUSED agent -- the same
    /// `env.agent == self.focused_agent` discipline every other
    /// focused-agent-scoped field in `apply` already follows.
    #[test]
    fn a_background_agents_prompt_never_touches_the_focused_agents_clock() {
        let (session, focused, mut state) = awaiting_state();
        let other = AgentId::new();

        state.apply(&envelope(
            session,
            other,
            Event::PermissionRequested {
                call_id: "tc_other".into(),
                rendered: "write elsewhere.md".into(),
            },
        ));
        assert!(
            state.awaiting_permission_since.is_none(),
            "an unfocused agent's prompt must not start the focused clock"
        );

        state.apply(&envelope(
            session,
            focused,
            Event::PermissionRequested {
                call_id: "tc_mine".into(),
                rendered: "write notes.md".into(),
            },
        ));
        let mine = state.awaiting_permission_since;
        assert!(mine.is_some());

        state.apply(&envelope(
            session,
            other,
            Event::PermissionResolved {
                call_id: "tc_other".into(),
                decision: PermissionDecisionKind::AllowOnce,
            },
        ));
        assert_eq!(
            state.awaiting_permission_since, mine,
            "an unfocused agent's decision must not zero the wait the operator is reading"
        );
    }

    /// A focus switch resets it alongside the turn clock -- the animation
    /// counters are per focused-agent, and a wait belonging to the agent
    /// just switched away from is not this one's.
    #[test]
    fn focusing_a_different_agent_clears_the_clock() {
        let (session, agent, mut state) = awaiting_state();
        state.apply(&envelope(
            session,
            agent,
            Event::PermissionRequested {
                call_id: "tc_1".into(),
                rendered: "write notes.md".into(),
            },
        ));
        assert!(state.awaiting_permission_since.is_some());

        state.focus_agent(AgentId::new());

        assert!(state.awaiting_permission_since.is_none());
        assert_eq!(state.activity, Activity::Idle);
    }
}

/// Board item `01M2VDD6MQG7H73AHGX8HJGV2J`: the lifecycle of
/// [`AppState::running_tool_since`], the clock the status line's elapsed
/// figure reads while the `running <tool>…` rung is up.
///
/// These pin the STATE half of the fix -- above all the multi-call rule,
/// which is the decision the item existed for: the clock belongs to the
/// call the rung NAMES, so it is restamped exactly when the name is. The
/// render half -- that the figure on screen is that clock's value and not
/// a literal `0s` -- is pinned in `view/status.rs`'s own tests, against
/// the real rendered buffer.
#[cfg(test)]
mod running_tool_clock {
    use std::time::Duration;

    use super::fixtures::envelope;
    use super::*;
    use conway::backend::StopReason;
    use conway::{PermissionDecisionKind, SessionId, ToolName};

    fn running_state() -> (SessionId, AgentId, AppState) {
        let session = SessionId::new();
        let agent = AgentId::new();
        let state = AppState::new(agent);
        (session, agent, state)
    }

    /// `bash`/`grep` deliberately -- never `edit`/`write`, whose arm reads
    /// the proposed path off disk to seed the diff baseline.
    fn propose(call_id: &str, tool: &str) -> Event {
        Event::ToolCallProposed {
            call_id: call_id.to_string(),
            tool: ToolName::new(tool),
            args: serde_json::json!({}),
        }
    }

    /// The reproduction, at the state layer. The real order reaching a
    /// dispatch is `TurnStarted` -> `TurnFinished` -> `ToolCallProposed`
    /// (`conway-runtime`'s `agent_loop` module doc makes `TurnFinished <
    /// ToolCallProposed*` binding), so by the time the rung goes up
    /// `clear_turn_state` has ALREADY zeroed `turn_started_at`. That is the
    /// whole defect: the elapsed figure read that field, found `None`, and
    /// rendered `0s` for the entire execution.
    #[test]
    fn a_call_starts_with_turn_started_at_already_cleared_and_stamps_its_own_clock() {
        let (session, agent, mut state) = running_state();

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        assert!(state.turn_started_at.is_some());

        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: StopReason::ToolUse,
            },
        ));
        state.apply(&envelope(session, agent, propose("tc_1", "bash")));

        assert_eq!(state.activity, Activity::RunningTool("bash".to_string()));
        assert!(
            state.turn_started_at.is_none(),
            "board item `01M2VDD6MQG7H73AHGX8HJGV2J`'s premise: the turn is already over \
             when a tool call is dispatched, so `turn_started_at` cannot be the clock the \
             execution is measured from"
        );
        assert!(
            state.running_tool_since.is_some(),
            "the dispatched call must start its own clock"
        );
    }

    /// **The multi-call decision.** A second proposal in the same batch
    /// renames the rung, and the clock is restamped with it -- the number
    /// is the age of the call the rung names, never the batch's, and never
    /// an older sibling call's.
    ///
    /// The pre-set stale stamp is what makes this deterministic: asserting
    /// "the new `Instant` differs from the old one" would lean on clock
    /// resolution, whereas an implausibly old stamp either survives
    /// (get-or-insert semantics -- the behavior being ruled out) or does
    /// not.
    #[test]
    fn a_later_call_in_the_batch_restamps_the_clock_along_with_the_name() {
        let (session, agent, mut state) = running_state();

        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        assert_eq!(state.activity, Activity::RunningTool("bash".to_string()));
        state.running_tool_since = Some(Instant::now() - Duration::from_secs(600));

        state.apply(&envelope(session, agent, propose("tc_2", "grep")));

        assert_eq!(
            state.activity,
            Activity::RunningTool("grep".to_string()),
            "the rung names the most recently proposed call"
        );
        assert!(
            state
                .running_tool_since
                .expect("the rung is up, so its clock must be running")
                .elapsed()
                < Duration::from_secs(60),
            "the clock must be restamped with the name -- a rung reading `running grep…` \
             must not be wearing the age of the `bash` call proposed before it"
        );
    }

    /// A permission prompt REPLACES the rung, so it stops the rung's clock
    /// -- the operator's deliberation is not part of the call's execution
    /// time, and `awaiting_permission_since` is the clock that wait has.
    #[test]
    fn a_prompt_replacing_the_rung_stops_its_clock() {
        let (session, agent, mut state) = running_state();

        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        assert!(state.running_tool_since.is_some());

        state.apply(&envelope(
            session,
            agent,
            Event::PermissionRequested {
                call_id: "tc_1".into(),
                rendered: "bash -lc ls".into(),
            },
        ));

        assert_eq!(state.activity, Activity::AwaitingPermission);
        assert!(
            state.running_tool_since.is_none(),
            "nothing is executing while the prompt is open"
        );
        assert!(state.awaiting_permission_since.is_some());
    }

    /// The batch coming back and the model taking another turn is the
    /// normal end of the rung; a terminal `AgentFinished` ends it too.
    #[test]
    fn the_next_turn_and_a_finished_agent_both_stop_the_clock() {
        let (session, agent, mut state) = running_state();
        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        assert!(state.running_tool_since.is_some());
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 2 }));
        assert!(
            state.running_tool_since.is_none(),
            "the model only gets another turn once the calls are back"
        );

        let (session, agent, mut state) = running_state();
        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        assert!(state.running_tool_since.is_some());
        state.apply(&envelope(
            session,
            agent,
            Event::AgentFinished {
                result: AgentResult::new(agent, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));
        assert!(state.running_tool_since.is_none());
        assert_eq!(state.activity, Activity::Idle);
    }

    /// Scoping. A background agent's tool call must neither start nor stop
    /// the clock behind the rung the operator is reading for the FOCUSED
    /// agent -- the same `env.agent == self.focused_agent` discipline every
    /// other focused-agent-scoped field in `apply` follows.
    #[test]
    fn a_background_agents_tool_call_never_touches_the_focused_clock() {
        let (session, focused, mut state) = running_state();
        let other = AgentId::new();

        state.apply(&envelope(session, other, propose("tc_other", "bash")));
        assert!(
            state.running_tool_since.is_none(),
            "an unfocused agent's dispatch must not start the focused clock"
        );

        state.apply(&envelope(session, focused, propose("tc_mine", "bash")));
        let mine = state.running_tool_since;
        assert!(mine.is_some());

        state.apply(&envelope(session, other, propose("tc_other2", "grep")));
        assert_eq!(
            state.running_tool_since, mine,
            "an unfocused agent's dispatch must not restamp the clock the operator is reading"
        );
        assert_eq!(state.activity, Activity::RunningTool("bash".to_string()));
    }

    /// A focus switch resets it alongside the other two clocks -- a call
    /// belonging to the agent just switched away from is not this one's.
    #[test]
    fn focusing_a_different_agent_clears_the_clock() {
        let (session, agent, mut state) = running_state();
        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        assert!(state.running_tool_since.is_some());

        state.focus_agent(AgentId::new());

        assert!(state.running_tool_since.is_none());
        assert_eq!(state.activity, Activity::Idle);
    }

    /// A resolution that never reached the operator (cached/pattern allow)
    /// emits `PermissionResolved` with no prompt ever having opened. It
    /// must not disturb the rung's clock -- only the permission clock,
    /// which is a no-op clear in that case.
    #[test]
    fn a_silent_permission_resolution_leaves_the_rung_clock_alone() {
        let (session, agent, mut state) = running_state();
        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        let running = state.running_tool_since;
        assert!(running.is_some());

        state.apply(&envelope(
            session,
            agent,
            Event::PermissionResolved {
                call_id: "tc_1".into(),
                decision: PermissionDecisionKind::Cached,
            },
        ));

        assert_eq!(
            state.running_tool_since, running,
            "a resolution the operator never saw does not interrupt the execution it allows"
        );
    }
}

/// Board item `01M2X463TDVV5TG53M3X1M3M6V`: **which RUNG is up once a
/// prompted call has been answered.** The two sibling items
/// (`01M2V60KWK9AYX3J7V5TPJZN7Q`, `01M2VDD6MQG7H73AHGX8HJGV2J`) were both
/// about which CLOCK a rung reads; this one is about which rung is
/// displayed at all. `Event::PermissionResolved` stopped the wait clock
/// and left `activity` at [`Activity::AwaitingPermission`], so an approved
/// call executed under `awaiting permission… 0s` -- a rung that says
/// conway is blocked on the operator who has already answered, wearing a
/// figure frozen at zero because the clock behind it is correctly stopped.
///
/// These pin the STATE half. The render half -- that the row on screen
/// reads `running <tool>…` after the allow -- is pinned in
/// `view/status.rs`'s own tests, against a real `TestBackend` buffer.
#[cfg(test)]
mod approved_call_rung {
    use std::time::Duration;

    use super::fixtures::envelope;
    use super::*;
    use conway::{PermissionDecisionKind, SessionId, ToolName};

    /// `bash`/`grep` deliberately -- never `edit`/`write`, whose arm reads
    /// the proposed path off disk to seed the diff baseline.
    fn propose(call_id: &str, tool: &str) -> Event {
        Event::ToolCallProposed {
            call_id: call_id.to_string(),
            tool: ToolName::new(tool),
            args: serde_json::json!({}),
        }
    }

    fn requested(call_id: &str) -> Event {
        Event::PermissionRequested {
            call_id: call_id.to_string(),
            rendered: "bash -lc ls".to_string(),
        }
    }

    fn resolved(call_id: &str, decision: PermissionDecisionKind) -> Event {
        Event::PermissionResolved {
            call_id: call_id.to_string(),
            decision,
        }
    }

    fn started(call_id: &str) -> Event {
        Event::ToolCallStarted {
            call_id: call_id.to_string(),
        }
    }

    /// **The reproduction.** The full prompted-call sequence a real
    /// approval produces: `ToolCallProposed` -> `PermissionRequested` ->
    /// `PermissionResolved(allow)` -> `ToolCallStarted`. Against HEAD the
    /// last event only set the per-call tool status, so `activity` was
    /// still `AwaitingPermission` for the whole execution.
    #[test]
    fn an_allowed_call_returns_to_the_running_rung_when_it_starts() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        state.apply(&envelope(session, agent, requested("tc_1")));
        assert_eq!(
            state.activity,
            Activity::AwaitingPermission,
            "precondition -- the prompt replaced the running rung"
        );

        state.apply(&envelope(
            session,
            agent,
            resolved("tc_1", PermissionDecisionKind::AllowOnce),
        ));
        state.apply(&envelope(session, agent, started("tc_1")));

        assert_eq!(
            state.activity,
            Activity::RunningTool("bash".to_string()),
            "an approved call must execute under its own rung, not under the wait it is past"
        );
        assert!(
            state.awaiting_permission_since.is_none(),
            "the wait is over -- its clock stays stopped"
        );
        assert!(
            state
                .running_tool_since
                .expect("the running rung is up, so its clock must be running")
                .elapsed()
                < Duration::from_secs(60),
            "the rung's clock must be running again once its rung is back"
        );
    }

    /// **The re-stamp decision.** The execution figure is measured from
    /// the START, not from the proposal that preceded the prompt: the
    /// operator's deliberation is not execution time, and it is already
    /// recorded durably as `PermissionDecisionRecord::waited_ms` (and
    /// shown in the `allowed once · waited 53s` transcript note). The
    /// implausibly old pre-set stamp is what makes this deterministic --
    /// it either survives (the behavior being ruled out) or it does not.
    #[test]
    fn the_execution_figure_is_stamped_at_the_start_not_at_the_proposal() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        state.apply(&envelope(session, agent, requested("tc_1")));
        state.apply(&envelope(
            session,
            agent,
            resolved("tc_1", PermissionDecisionKind::AllowOnce),
        ));
        // As if the proposal's stamp had survived a ten-minute deliberation.
        state.running_tool_since = Some(Instant::now() - Duration::from_secs(600));

        state.apply(&envelope(session, agent, started("tc_1")));

        assert!(
            state
                .running_tool_since
                .expect("the running rung is up, so its clock must be running")
                .elapsed()
                < Duration::from_secs(60),
            "the figure must measure the execution, never the wait that preceded it"
        );
    }

    /// **The denial decision.** A denied call never starts, so `running`
    /// would be a lie and `awaiting permission` already is one -- the
    /// operator has answered. The rung goes back to exactly where the
    /// event stream left it before the proposal: `Idle` (the binding order
    /// `TurnFinished < ToolCallProposed*` means the turn itself is already
    /// over by then), with both clocks stopped. The next turn-level event
    /// -- the follow-up turn the denial feedback provokes -- moves it on.
    #[test]
    fn a_denied_call_drops_the_rung_to_idle_never_running() {
        for decision in [
            PermissionDecisionKind::Denied,
            PermissionDecisionKind::DeniedWithFeedback,
        ] {
            let session = SessionId::new();
            let agent = AgentId::new();
            let mut state = AppState::new(agent);

            state.apply(&envelope(session, agent, propose("tc_1", "bash")));
            state.apply(&envelope(session, agent, requested("tc_1")));
            state.apply(&envelope(session, agent, resolved("tc_1", decision)));

            assert_eq!(
                state.activity,
                Activity::Idle,
                "a denied call is neither running nor still awaiting an answer ({decision:?})"
            );
            assert!(state.running_tool_since.is_none(), "{decision:?}");
            assert!(state.awaiting_permission_since.is_none(), "{decision:?}");
        }
    }

    /// A denial that never reached the operator (a rule/hook/mode denial,
    /// no prompt ever opened) leaves the rung claiming `running <tool>`
    /// for a call that will never run. Same rule, same reason: the rung is
    /// about this call, so it comes down.
    #[test]
    fn a_silent_denial_also_takes_down_the_rung_naming_that_call() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        assert_eq!(state.activity, Activity::RunningTool("bash".to_string()));

        state.apply(&envelope(
            session,
            agent,
            resolved("tc_1", PermissionDecisionKind::Denied),
        ));

        assert_eq!(state.activity, Activity::Idle);
        assert!(state.running_tool_since.is_none());
    }

    /// The narrowness of that rule, stated as a test: a denial takes down
    /// only a rung that is ABOUT the denied call. A sibling call still
    /// executing keeps the rung it named -- the denial of one call in a
    /// parallel batch is not a statement about the others.
    #[test]
    fn a_denial_leaves_a_sibling_calls_rung_up() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, propose("tc_1", "bash")));
        state.apply(&envelope(session, agent, propose("tc_2", "grep")));
        assert_eq!(state.activity, Activity::RunningTool("grep".to_string()));
        let sibling_clock = state.running_tool_since;

        state.apply(&envelope(
            session,
            agent,
            resolved("tc_1", PermissionDecisionKind::Denied),
        ));

        assert_eq!(
            state.activity,
            Activity::RunningTool("grep".to_string()),
            "denying `bash` says nothing about the `grep` call the rung names"
        );
        assert_eq!(state.running_tool_since, sibling_clock);
    }

    /// Scoping, the same `env.agent == self.focused_agent` discipline every
    /// other focused-agent-scoped field in `apply` follows: a background
    /// agent's call starting must not move the rung the operator is
    /// reading.
    #[test]
    fn a_background_agents_call_starting_never_moves_the_focused_rung() {
        let session = SessionId::new();
        let focused = AgentId::new();
        let other = AgentId::new();
        let mut state = AppState::new(focused);

        state.apply(&envelope(session, focused, propose("tc_mine", "bash")));
        state.apply(&envelope(session, focused, requested("tc_mine")));
        state.apply(&envelope(session, other, propose("tc_other", "grep")));
        assert_eq!(
            state.activity,
            Activity::AwaitingPermission,
            "precondition -- an unfocused agent's proposal does not move the rung either"
        );

        state.apply(&envelope(session, other, started("tc_other")));

        assert_eq!(
            state.activity,
            Activity::AwaitingPermission,
            "the operator is still being asked about the FOCUSED agent's call"
        );
        assert!(state.running_tool_since.is_none());
    }

    /// A `ToolCallStarted` whose proposal this state never saw (a fresh
    /// subscription mid-batch, or a `--resume`/focus-switch replay, which
    /// synthesizes no `ToolCallProposed` at all) carries no tool name --
    /// `Event::ToolCallStarted` holds only a `call_id`. There is nothing
    /// to name the rung with, so the rung is left exactly as it was rather
    /// than being taken over by an anonymous `running …`.
    #[test]
    fn a_start_with_no_proposal_in_the_transcript_leaves_the_rung_alone() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        state.apply(&envelope(
            session,
            agent,
            Event::ThinkingDelta {
                text: "hmm".to_string(),
            },
        ));
        assert_eq!(state.activity, Activity::Thinking);

        state.apply(&envelope(session, agent, started("tc_never_seen")));

        assert_eq!(state.activity, Activity::Thinking);
        assert!(state.running_tool_since.is_none());
    }
}

#[cfg(test)]
mod exit_hint_focus_scope {
    use super::*;

    #[test]
    fn a_focus_switch_clears_the_pending_exit_hint() {
        let mut state = AppState::new(AgentId::new());
        state.pending_exit_word = Some("exit".to_string());
        state.focus_agent(AgentId::new());
        assert_eq!(
            state.pending_exit_word, None,
            "a first `exit` on a newly focused agent must show the hint, not send"
        );
    }
}

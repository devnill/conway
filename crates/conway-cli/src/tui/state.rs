//! `AppState`: the TUI's render model, derived purely from the same
//! `EventStream` the one-shot renderers consume (WI-114).
//!
//! `apply` is the single mutation entry point -- fed one [`Envelope`] at a
//! time by the app loop -- so this module can be unit-tested with no
//! terminal at all: construct an `AppState`, feed it a sequence of
//! `Envelope`s, and assert on the resulting `transcript`/`tree`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use chrono::{DateTime, Utc};
use conway::{
    config::schema::StatusLineConfig, AgentId, AgentIntent, AgentResult, Envelope, Event, LogSeq,
    PermissionMode, ResultStatus, SegmentId, SubagentMode, Usage,
};

use super::gate::PendingPrompt;

/// The focused agent's live activity, rendered as the status line's primary
/// "is it working?" signal (board item 01KYAGP11FF9YC3G60TWHHKKST).
/// Transitions live in [`AppState::apply`], driven by events on the
/// FOCUSED agent's own stream only (`ThinkingDelta`->`Thinking`,
/// `TextDelta`->`Responding`, `ToolCallProposed{tool}`->`RunningTool(name)`
/// -- the name is captured from `Proposed`, not `Started`, which carries
/// only a `call_id` -- `PermissionRequested`->`AwaitingPermission`,
/// `TurnFinished`/`AgentFinished`->`Idle`). Reset to `Idle` whenever the
/// focus itself changes ([`AppState::focus_agent`]) -- a freshly focused
/// agent shows no activity signal until its own next event arrives, rather
/// than carrying over whatever the PREVIOUS focus was doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Idle,
    Thinking,
    Responding,
    RunningTool(String),
    AwaitingPermission,
}

/// The braille spinner frame sequence (T2, 8 TPS animation tick). Advanced by
/// [`AppState::tick_animation`] only while [`AppState::activity`] is not
/// [`Activity::Idle`] (idle terminal stays flat-cost -- no animation tick
/// work, no redraw). The 10-glyph braille cycle is the same one `spinners`-
/// style CLI indicators use.
pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Whether `activity` should drive the 125ms animation tick (T2): true for
/// every variant but [`Activity::Idle`]. The app loop's animation-tick arm
/// calls this to decide whether to advance the spinner/frame counters and
/// mark the frame dirty -- an idle terminal is never redrawn by the animation
/// tick, keeping idle cost flat (the 16ms redraw tick still runs but is itself
/// dirty-gated).
pub fn should_animate(activity: &Activity) -> bool {
    !matches!(activity, Activity::Idle)
}

/// One line of the transcript pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    /// Assistant reply text. T4 adds three provenance fields:
    /// - `model` is the serving model's display name (e.g.
    ///   `anthropic/claude-sonnet-4-6`), stamped from
    ///   [`AppState::focused_model`] at the time the entry is created by
    ///   `TextDelta` -> [`AppState::append_assistant_text`]. `None` for
    ///   replayed entries (`record_to_event` maps a stored `Assistant` record
    ///   to a bare `TextDelta` carrying no model -- see that function's own
    ///   doc); the renderer then omits the `[modelname]> ` marker so a
    ///   replayed bubble renders as it originally streamed.
    /// - `summary` is the turn-end summary line (`1m 6s · 1.4k tok (88%
    ///   cached)`), stamped onto the last assistant/reasoning entry by
    ///   `TurnFinished` -> [`AppState::stamp_turn_summary`]. `None` until
    ///   the turn ends (and stays `None` if no assistant/reasoning block
    ///   exists to attach to).
    /// - `ts` is the per-entry timestamp, stamped from the envelope's `ts`
    ///   at apply time. The `/settings` menu's "show timestamps" toggle
    ///   (V4; formerly the standalone `/timestamps` command) prepends
    ///   `HH:MM ` to the entry's first rendered line.
    Assistant {
        text: String,
        model: Option<String>,
        summary: Option<String>,
        ts: Option<DateTime<Utc>>,
    },
    /// T4: reasoning-trace text, fed by `Event::ThinkingDelta` (previously
    /// dropped by `apply`'s wildcard arm -- only `activity` was flipped to
    /// `Thinking`). Mirrors [`Entry::Assistant`]: `ThinkingDelta` -> [
    /// `AppState::append_reasoning_text`] creates-or-appends, stamping the
    /// current serving model + envelope timestamp onto a freshly-created
    /// entry. Rendered dim+italic with a `thinking` prefix, EXPANDED by
    /// default (the `show_reasoning` flag -- toggled from the `/settings`
    /// menu, V4; formerly the standalone `/thinking` command -- defaults
    /// `true`, so reasoning is visible until the user hides it;
    /// when hidden, `build_lines` skips `Entry::Reasoning` entirely). The
    /// `summary` field is shared with `Entry::Assistant`: a turn-end
    /// summary attaches to whichever of the two was the LAST block under
    /// the turn.
    Reasoning {
        text: String,
        model: Option<String>,
        summary: Option<String>,
        ts: Option<DateTime<Utc>>,
    },
    Tool {
        call_id: String,
        name: String,
        status: ToolStatus,
        preview: String,
        /// T4: the tool call's arguments, stored from
        /// `Event::ToolCallProposed { args, .. }` (previously discarded --
        /// only `name` was stored). Serialized to a compact JSON string at
        /// apply time. Rendered as a one-line truncated `args: …` preview
        /// while collapsed and pretty-printed (multi-line) while expanded.
        /// Reuses the `expanded` flag + Ctrl-E toggle below -- args and
        /// output expand/collapse together (the single flag governs both).
        args: String,
        /// T4: accumulated `Event::ToolProgress { call_id, note }` notes
        /// (previously dropped by `apply`'s wildcard arm), appended to the
        /// matching in-flight tool entry by `call_id`. Joined with `\n` and
        /// rendered as dim `-> {note}` lines between the args line and the
        /// output block.
        progress: String,
        /// T5: whether this tool entry's preview is shown in full (`true`)
        /// or collapsed to the `tool_preview_lines` cap + a dim affordance
        /// (`false`, the default). Flipped on EVERY `Entry::Tool` at once by
        /// [`AppState::toggle_all_tool_entries_expanded`] (the `Ctrl-E`
        /// keybinding). The flag is kept on the entry itself -- not derived
        /// from a single global toggle -- so a future per-entry selective
        /// expand (T4's tool-args reuse, or a transcript-cursor selection)
        /// can flip individual entries without touching the rest. The render
        /// branch in `view/transcript.rs::tool_lines` reads this plus the
        /// stored `preview` (which is NEVER truncated -- the cap is
        /// render-time only) and emits either the first N lines + a `… (+M
        /// lines, Ctrl-E to expand)` affordance or the full content. T4
        /// reuses the same `expanded` flag + render branch for tool-args
        /// previews: a one-line-truncated args preview is the same shape
        /// (collapsed: cap lines + affordance; expanded: full), just with a
        /// different cap and content.
        expanded: bool,
        /// T4: per-entry timestamp, stamped from the envelope's `ts` at
        /// apply time. The `/settings` menu's "show timestamps" toggle (V4;
        /// formerly the standalone `/timestamps` command) prepends
        /// `HH:MM ` to the entry's first rendered line.
        ts: Option<DateTime<Utc>>,
    },
    /// A subagent's lifecycle, rendered inline in the conversation stream
    /// (WI-127 criterion: "inline subagent activity in the stream,
    /// Claude-Code-style") instead of only being reflected in the
    /// below-chat `/agents` panel. Pushed once at spawn time
    /// (`apply_agent_spawned`) and updated in place at finish time
    /// (`apply_agent_finished`) -- never a second entry for the same agent.
    Agent {
        agent_id: AgentId,
        label: String,
        status: NodeStatus,
    },
    Notice {
        text: String,
    },
    /// A runtime error surfaced via `Event::Error` (board item
    /// 01KYND6GCCKYSYD0VDGJD1ZCXG). Kept as its OWN variant rather than a
    /// field bolted onto [`Entry::Notice`]: a field would still let severity
    /// leak into an existing cyan-styled call site by accident, and (more
    /// concretely) a recon on this item found a field/constructor approach
    /// touches every one of `Entry::Notice`'s ~50 construction sites while a
    /// separate variant touches exactly three (this apply arm,
    /// `view/transcript.rs::entry_lines`, and the variant-enumerating
    /// clean-copy test). `fatal: true` renders in `theme.fatal_error`
    /// (Red+Bold) -- conway's loudest possible message, previously
    /// indistinguishable from a routine cyan notice save for the word
    /// "fatal" inside the string. `fatal: false` is a real, non-recoverable-
    /// looking-but-actually-recoverable error too, so it does not fall back
    /// to `theme.notice` either: it renders in `theme.error` (plain Red, the
    /// same slot the `/ask` modal's failed-fate line already uses), one step
    /// down from `fatal_error`'s bold. Red still means failure at both
    /// severities; only the loudest one gets the bold escalation. See
    /// `entry_lines`'s `Entry::Error` arm for the one place this severity
    /// decision is made.
    Error {
        text: String,
        fatal: bool,
    },
}

/// A tool call's lifecycle, as reflected in one [`Entry::Tool`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Proposed,
    AwaitingPermission,
    Running,
    Finished { is_error: bool },
}

/// The interactive mode's own agent-tree projection -- built from
/// `Event::AgentSpawned`/`Event::AgentFinished` alone, independent of
/// `conway::AgentTreeSnapshot` (which reflects the whole `Runtime`, not just
/// this session, and is unavailable to a purely event-driven state machine).
#[derive(Debug, Clone, Default)]
pub struct AgentTreeView {
    pub root: Option<AgentId>,
    /// Insertion order preserved (`Vec`) so the left pane renders
    /// deterministically; looked up by id via `index`.
    pub nodes: Vec<TreeNode>,
    index: HashMap<AgentId, usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TreeNode {
    pub agent_id: AgentId,
    pub parent: Option<AgentId>,
    pub agent_def: Option<String>,
    pub status: NodeStatus,
    /// How this agent was spawned (`Fork`/`Spawn`/...), from
    /// `Event::AgentSpawned::kind`. `None` for the root and for nodes
    /// seeded out-of-band (`ensure_agent_tracked`), which never saw a
    /// spawn event.
    pub kind: Option<SubagentMode>,
    /// Fork provenance: the parent's log position the child inherited up
    /// to, from `Event::AgentSpawned::inherited_upto`.
    pub inherited_upto: Option<LogSeq>,
    /// Whether this is an ephemeral `/ask`-style aside (it stays in
    /// the tree with its provenance attached; draw-time visibility
    /// filtering is a separate concern).
    pub ephemeral: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeStatus {
    Starting,
    Running,
    AwaitingPermission,
    Finished,
    Failed,
    Cancelled,
}

impl NodeStatus {
    /// Terminal statuses share one predicate between the `/agents` panel's
    /// visibility filter (item A2) and the live-agent checks below.
    fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeStatus::Finished | NodeStatus::Failed | NodeStatus::Cancelled
        )
    }
}

/// The `/agents` panel's draw-time visibility filter (item A2): which tree
/// nodes the panel's ROWS show. Lives entirely at draw time -- the
/// `AgentTreeView` itself stays unfiltered -- provenance is never destroyed, so
/// finished agents are hidden, never removed. The predicate is a pure function
/// of the node and the mode, unit-testable with no terminal, and flipping the
/// mode never mutates the tree. Cycled by `v` while the panel is open
/// (`input.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentVisibility {
    /// Terminal-status nodes (Finished/Failed/Cancelled) are hidden, so the
    /// panel reads as "what is still running".
    ///
    /// NOT the default (V5 reversal of item A2's original decision):
    /// dogfooding showed that hiding a node the instant it finishes reads
    /// as "the agents screen doesn't always list the same agents" -- the
    /// panel's *shape* changing on its own the moment a child completes,
    /// with no visible cause, is exactly what "agents randomly
    /// disappearing" looks like to someone who has not discovered `v`. The
    /// filter itself is still useful (a long session's "what's still
    /// running" view), so it stays -- it is simply no longer the first
    /// thing a user sees.
    ActiveOnly,
    /// The default (V5): every node, terminal or not. A stable list --
    /// finishing never removes a row -- beats a shorter one that reshapes
    /// itself underneath the user. `NodeStatus`'s own marker glyph (`v`/`x`/
    /// `-` vs `*`/`o`/`?`) already reads status at a glance per row, so
    /// nothing about "what's still running" is lost, unlike hiding it would
    /// lose "what happened." A dimmed-but-visible middle ground was also
    /// considered (V5 spec) and would work too, but `All` is the simpler
    /// change and needs no new theme slot (T1: no bare inline styles in
    /// `view/` outside `theme.rs`) -- the status glyph alone already
    /// carries "this one is done."
    All,
    /// Show ONLY terminal-status nodes -- the "what already ran" view.
    FinishedOnly,
}

impl AgentVisibility {
    /// The `v`-key cycle order: ActiveOnly -> All -> FinishedOnly -> ActiveOnly.
    /// Unchanged by V5's default flip (only the STARTING point of the cycle
    /// moved, not the order).
    pub fn next(self) -> Self {
        match self {
            Self::ActiveOnly => Self::All,
            Self::All => Self::FinishedOnly,
            Self::FinishedOnly => Self::ActiveOnly,
        }
    }

    /// The pure filter predicate: tree node + mode -> visible?
    pub fn shows(self, node: &TreeNode) -> bool {
        match self {
            Self::ActiveOnly => !node.status.is_terminal(),
            Self::All => true,
            Self::FinishedOnly => node.status.is_terminal(),
        }
    }

    /// The short label the panel's header shows for the current mode.
    pub fn label(self) -> &'static str {
        match self {
            Self::ActiveOnly => "active",
            Self::All => "all",
            Self::FinishedOnly => "finished",
        }
    }
}

impl AgentTreeView {
    fn get_mut(&mut self, id: AgentId) -> Option<&mut TreeNode> {
        self.index.get(&id).map(|&i| &mut self.nodes[i])
    }

    fn contains(&self, id: AgentId) -> bool {
        self.index.contains_key(&id)
    }

    fn insert(&mut self, node: TreeNode) {
        let id = node.agent_id;
        self.index.insert(id, self.nodes.len());
        self.nodes.push(node);
    }
}

/// The `/ask` modal's state (B5): one answered ephemeral fork-ask waiting
/// for the user to choose its fate. The modal opens only once the child's
/// single turn has COMPLETED (`app.rs` drives `SessionHandle::ask` +
/// `TurnHandle::text` to the finished reply, then `offer_ask_modal`), so
/// `answer` is always the final reply text -- never a pending placeholder.
/// `child` is the ephemeral fork child's [`AgentId`] (from
/// `TurnHandle::agent`), the value all three fates' facade ops take.
/// `error` is `Some` only after a fate attempt FAILED -- the modal stays
/// open with the error shown (the user still must choose; a failed fate
/// never silently falls through to another one).
#[derive(Debug, Clone, PartialEq)]
pub struct AskModal {
    pub question: String,
    pub child: AgentId,
    pub answer: String,
    pub error: Option<String>,
}

/// One of the three forced fates closing the `/ask` modal (B5) -- exactly one
/// of these runs; there is no fourth way out (quitting with the modal open
/// purges, wired in `app.rs`'s quit path). Each maps to exactly one facade op
/// (`commands::apply_ask_fate`): `Fork` -> `Conway::promote` (B3: keep -- the
/// node loses its `(ephemeral)` marking and becomes a session in its own
/// right), `PullIn` -> `Conway::pull_in` (B4: the question+answer merge into
/// the parent's own log, child purged), `Discard` -> `Conway::purge` (the
/// explicit exception to provenance being permanent and visible: the answer is
/// thrown away).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskFate {
    Fork,
    PullIn,
    Discard,
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
/// behalf. The card is a single modal slot like [`AskModal`].
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
#[derive(Debug, Clone, PartialEq)]
pub struct IntentConfirm {
    pub intent: AgentIntent,
    pub default_recipe: SubagentMode,
    pub raw_text: String,
    pub parent: AgentId,
}

/// `Normal` (the input line submits a prompt or a `/command`) or
/// `AwaitingPermission` (the input line is inert; `y`/`a`/`n`/`Esc` resolve
/// the pending prompt -- see `input.rs`). Only one prompt is shown at a
/// time; concurrent requests queue in `pending_prompts` (module notes:
/// "concurrent requests queue in arrival order").
pub enum Mode {
    Normal,
    AwaitingPermission(PendingPrompt),
    /// The `/ask` single-turn modal (B5). While this is the mode, the input
    /// line is inert and `/agents` is neither visible nor available --
    /// `input.rs::handle_ask_modal_key` swallows every key except the three
    /// fate keys (`f`/`p`/`Esc`) and the quit keys (`Ctrl-C`/`Ctrl-D`,
    /// which purge before exiting -- see `app.rs`). A permission prompt
    /// arriving while the modal is open queues in `queued_prompts` exactly
    /// as it does behind another prompt, and an ask answer arriving while a
    /// permission prompt is showing parks in `pending_ask_modal` until the
    /// prompt resolves -- the two modals never stack.
    AskModal(AskModal),
    /// The NL intent confirmation card (C2). While this is the mode, the
    /// input line is inert and `/agents` is neither visible nor available
    /// -- `input.rs::handle_intent_confirm_key` swallows every key except
    /// `Enter` (confirm), `e` (edit -- drops the classified prompt into the
    /// input line and closes the card) and `Esc` (manual fallback), plus
    /// the quit keys (`Ctrl-C`/`Ctrl-D`, which pass through -- unlike the
    /// `/ask` modal there is no live child to purge, since the card opens
    /// BEFORE any agent is created). A permission prompt arriving while the
    /// card is open queues in `queued_prompts` exactly as it does behind
    /// another prompt, and an intent card arriving while a permission prompt
    /// OR an `/ask` modal is showing parks in `pending_intent_confirm` until
    /// the surface clears -- the three modal-bearing surfaces
    /// (`AwaitingPermission`, `AskModal`, `IntentConfirm`) never stack.
    IntentConfirm(IntentConfirm),
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Normal => write!(f, "Normal"),
            Mode::AwaitingPermission(p) => {
                write!(f, "AwaitingPermission({})", p.request.call_id)
            }
            Mode::AskModal(m) => write!(f, "AskModal(child={})", m.child),
            Mode::IntentConfirm(ic) => {
                write!(f, "IntentConfirm(recipe={:?})", ic.intent.recipe)
            }
        }
    }
}

/// The TUI's whole render model. Every mutation goes through [`Self::apply`]
/// (event-driven) or the app loop's direct field writes for input-driven
/// state (`input`, `mode`, `scroll`) -- see `input.rs`/`app.rs`.
pub struct AppState {
    pub transcript: Vec<Entry>,
    pub tree: AgentTreeView,
    /// The last `Event::ModelDecision` envelope seen, for `/why`. Populated
    /// by `app.rs`'s run loop on `Event::ModelDecision` (WI-115) and read by
    /// `commands::render_why`; `apply` intentionally leaves it untouched so
    /// it stays pure.
    pub last_model_decision: Option<Envelope>,
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
    /// V2b: where a newly-granted pattern is persisted, in precedence
    /// order (project first, then global). Resolved once at `App::new`.
    /// Empty when neither scope resolves, in which case a grant applies
    /// to the session but is not written anywhere.
    pub permission_paths: Vec<std::path::PathBuf>,
    /// V2b: the active pattern ALLOW grants, for the settings review list.
    /// A mirror of the broker's `active_patterns()`, refreshed when
    /// `/settings` opens and after any revoke action — the broker remains
    /// the authority. Board item 01KYND4WGHSZXW5YQ6ZWHCDDNN: kept as the
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
    /// The scope the permission prompt's remembered-grant keys (`a` and
    /// `p`) grant at: `Session` (the default, and the only scope the prompt
    /// offered before this item), `Agent` (only the agent whose call is
    /// being asked about), or `AgentSubtree` (that agent's whole subtree).
    /// Cycled by the prompt's `s` key (`input.rs::handle_permission_key`),
    /// rendered by `view/mod.rs::draw_permission_overlay`, and reset to
    /// `Session` every time a NEW prompt becomes the active one (see
    /// [`Self::offer_prompt`]/[`Self::promote_next_surface`]) -- a scope
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
    /// Whether the below-chat agent-tree panel (WI-127 criterion 4) is
    /// currently shown. Toggled by `/agents` (handled in `app.rs`, since
    /// `commands.rs` -- out of this item's file scope -- owns no such
    /// command); never an always-on pane.
    pub agent_view_open: bool,
    /// Arrow-navigated row in the slash-command palette (WI-130), or `None`
    /// when the user has typed a `/` prefix but not yet pressed an arrow. The
    /// arrow keys move this and autofill [`AppState::input`] with the
    /// highlighted command (see `input.rs`); typing resets it to `None`.
    pub palette_selected: Option<usize>,
    /// The text the palette's match list stays anchored to: whatever the
    /// user last *typed*. Arrow navigation autofills `input` with a whole
    /// command but leaves this alone, so cycling the list does not collapse
    /// it to the single autofilled entry. Read via [`AppState::palette_source`].
    palette_stem: String,
    /// Arrow-selected row in the on-demand agent panel (WI-130). An index
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
    /// (WI-140). Distinct from `agent_selected` -- that field is only the
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
    /// The focused agent's current live activity (board item
    /// 01KYAGP11FF9YC3G60TWHHKKST), rendered by `view/status.rs`. See
    /// [`Activity`]'s own doc for the event-driven transitions.
    pub activity: Activity,
    /// The focused agent's cumulative token spend, rendered alongside
    /// `activity` in the status line (same board item). This field is
    /// live-incremented from `Event::TurnFinished{usage}` in
    /// [`Self::apply`] for immediate feedback, but is NOT authoritative on
    /// its own -- `app.rs`'s run loop re-fetches the true total via the
    /// `SessionHandle::session_usage` facade accessor (through the `Host`
    /// trait's `session_usage`) on focus change and after
    /// `TurnFinished`/`AgentFinished` for the focused agent, overwriting
    /// whatever this field held (replay carries no `Usage` at all -- WI-140's
    /// `record_to_event` maps a replayed `Assistant` record to `TextDelta`,
    /// not `TurnFinished` -- so this field alone would silently stay zero
    /// after any focus switch onto an agent with prior turns without that
    /// authoritative refresh).
    pub focused_agent_usage: Usage,
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
    /// all funnel through [`Self::promote_next_surface`] to drain the
    /// queued-prompts / `pending_ask_modal` / `pending_intent_confirm`
    /// slots in that fixed priority order, so the three modal-bearing
    /// surfaces never stack.
    pending_intent_confirm: Option<IntentConfirm>,
    /// Whether an `/ask` child's single turn is currently in flight (B5).
    /// Set by `app.rs` when it spawns the ask task, cleared when the result
    /// arrives -- while set, a second `/ask` is refused with a `Notice`
    /// (the modal is a single-question surface; concurrent asks would
    /// compete for the one [`Mode::AskModal`] slot).
    pub ask_in_flight: bool,
    /// The shared modal body-scroll offset (V1; originated as the
    /// permission-overlay-only `permission_scroll`, bug fix
    /// 01KYB0F7V65QAMZWWYH8K7DWDC: "no way to see the entire command" for a
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
    /// [`Self::offer_prompt`], [`Self::promote_next_surface`],
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
    /// [`Self::stamp_turn_summary`]'s reverse scan to entries THIS turn
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
    /// visibility). Set at `App::new` from `conway.config().tui.status_line`;
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
    /// `conway.config().tui.tool_preview_lines` via
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
}

impl AppState {
    pub fn new(root: AgentId) -> Self {
        let mut tree = AgentTreeView {
            root: Some(root),
            ..AgentTreeView::default()
        };
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
            input: String::new(),
            cursor: 0,
            mode: Mode::Normal,
            permission_mode: PermissionMode::default(),
            permission_paths: Vec::new(),
            permission_grants: Vec::new(),
            structured_allow_rules: Vec::new(),
            permission_denies: Vec::new(),
            permission_prompts: Vec::new(),
            structured_deny_rules: Vec::new(),
            structured_prompt_rules: Vec::new(),
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
            pending_ask_modal: None,
            ask_in_flight: false,
            modal_scroll: 0,
            pending_intent_confirm: None,
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
        }
    }

    /// The session's own root agent, as recorded in `tree.root` at
    /// construction (`AppState::new`) -- the target [`Self::focus_agent`]
    /// returns to (WI-140: "Esc, or focusing the root row" both resolve
    /// here). Falls back to `focused_agent` itself in the
    /// should-never-happen case that `tree.root` is `None` (only possible if
    /// some future `apply` change clears it -- `AppState::new` always seeds
    /// it `Some`), so this never panics.
    pub fn root_agent(&self) -> AgentId {
        self.tree.root.unwrap_or(self.focused_agent)
    }

    /// Whether the transcript pane is currently showing the session's own
    /// root conversation (as opposed to some other agent's) -- read by
    /// `view/status.rs` and `view/agents.rs` to indicate the focused agent.
    pub fn is_root_focused(&self) -> bool {
        self.focused_agent == self.root_agent()
    }

    /// Whether the FOCUSED agent can still be usefully prompted (review
    /// fix, SIGNIFICANT: prompting a finished agent silently loses the
    /// message and wedges the activity indicator on `Thinking` forever --
    /// see `app.rs::submit`'s own doc for the full mechanism). `true` for
    /// every non-terminal [`NodeStatus`] (`Starting`, `Running`,
    /// `AwaitingPermission`) -- a keep-alive root or an idle keep-alive
    /// child both sit in one of those, never a terminal one, so neither is
    /// ever blocked here. `false` only for `Finished`/`Failed`/`Cancelled`.
    ///
    /// **Fail-open when `focused_agent` has no tree node yet:** this
    /// happens for real, not just defensively -- a brand-new child from a
    /// bare `/spawn`/`/fork` is focused (`app.rs::try_focus_agent`) the
    /// SAME turn its own `Event::AgentSpawned` was broadcast, and that
    /// envelope may not have reached `Self::apply` yet (the app loop's
    /// `tokio::select!` arms are mutually exclusive per iteration -- see
    /// that module's own doc). Treating "not tracked yet" as blocked would
    /// wrongly reject the very first message to a freshly created session;
    /// `true` here is the correct default the same way an untracked agent
    /// is never treated as terminal anywhere else in this module.
    pub fn is_focused_agent_live(&self) -> bool {
        match self
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == self.focused_agent)
        {
            Some(node) => !node.status.is_terminal(),
            None => true,
        }
    }

    /// The guard `app.rs::submit` applies before prompting the focused
    /// agent (review fix, SIGNIFICANT): when [`Self::is_focused_agent_live`]
    /// is `false`, pushes the explanatory `Notice` and returns `true` so the
    /// caller can return early WITHOUT prompting and WITHOUT touching
    /// `activity` (leaving it exactly as it was -- typically `Idle`, never
    /// bumped to `Thinking` for a message that was never actually sent).
    /// Returns `false` (no mutation at all) when the focused agent is live,
    /// letting the caller proceed normally.
    ///
    /// Factored out of `app.rs::submit` as its own method -- rather than the
    /// equivalent `if !self.is_focused_agent_live() { .. }` inlined at that
    /// one call site -- so the exact `Notice` text is directly unit-testable
    /// with no live `SessionHandle`/`App` needed to reach it (`submit`
    /// itself owns a live facade call this module's own tests cannot make).
    pub fn block_message_if_focused_agent_finished(&mut self) -> bool {
        if self.is_focused_agent_live() {
            return false;
        }
        let agent = self.focused_agent;
        self.transcript.push(Entry::Notice {
            text: format!(
                "can't message {agent} -- it has finished; switch to a live session \
                 with /agents or open a new one with /spawn"
            ),
        });
        true
    }

    /// Switches the transcript pane to `agent`'s own conversation (WI-140).
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
        // CURRENTLY focused (board item 01KYAGP11FF9YC3G60TWHHKKST) -- a
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
        // still does not repopulate these (`record_to_event` (WI-140) maps
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

    /// T2 animation tick (125ms / 8 TPS): advances the braille spinner frame
    /// and the pulse-color index, both wrapping. The caller (the app loop's
    /// animation-tick arm) is responsible for only calling this while
    /// [`should_animate`] is true for [`Self::activity`], so an idle terminal
    /// never pays for animation. The frame index wraps modulo
    /// [`SPINNER_FRAMES`]' length.
    ///
    /// V6 removed the color-pulse half of this. T2 also advanced a palette
    /// index so the glyph and activity word cycled colors on every tick;
    /// that read as strobing rather than as liveness. The advancing frame
    /// already conveys "something is happening" -- adding color motion on
    /// top only competed with it.
    pub fn tick_animation(&mut self) {
        let frames = SPINNER_FRAMES.len();
        if frames != 0 {
            self.spinner_frame = (self.spinner_frame + 1) % frames;
        }
    }

    /// Clears the per-turn timing/token counters (T2). Called whenever
    /// `activity` transitions back to [`Activity::Idle`] -- the working
    /// indicator no longer shows elapsed/running tokens once the turn is
    /// done. The spinner counters themselves are zeroed by [`Self::focus_agent`]
    /// and otherwise left alone on idle (they simply stop advancing, which
    /// is fine -- the renderer only draws them while active).
    fn clear_turn_state(&mut self) {
        self.turn_started_at = None;
        self.turn_running_tokens = 0;
    }

    /// The text the slash-command palette's match list is anchored to
    /// (WI-130): the stem the user last *typed* when set, else the raw
    /// `input`. Arrow navigation autofills `input` with a whole command but
    /// leaves the stem alone, so cycling the list does not collapse it; the
    /// `input` fallback keeps the palette visible for callers/tests that set
    /// `input` directly without going through key handling.
    pub fn palette_source(&self) -> &str {
        if self.palette_stem.is_empty() {
            &self.input
        } else {
            &self.palette_stem
        }
    }

    /// Re-anchors the palette to whatever the user just typed and clears the
    /// arrow highlight (WI-130). Called after every edit to `input` in
    /// `input.rs`, so typing always re-filters live from the new text.
    pub fn sync_palette_stem(&mut self) {
        self.palette_stem = self.input.clone();
        self.palette_selected = None;
    }

    /// Closes the palette navigation state (WI-130): called when `input` is
    /// submitted, so a fresh line starts with no stem and no highlight.
    pub fn clear_palette(&mut self) {
        self.palette_stem.clear();
        self.palette_selected = None;
    }

    /// The cursor's (line, column) position within [`Self::input`], both
    /// char indices -- `line` counts `\n` characters before the cursor,
    /// `column` is the cursor's offset from that line's own start (T8:
    /// multi-line input, Alt/Shift-Enter). Used by `view/input_box.rs` to
    /// place the on-screen cursor and by `input.rs`'s `Up`/`Down`
    /// vertical-cursor-movement gating.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for c in self.input.chars().take(self.cursor) {
            if c == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Records a just-submitted line into [`Self::history`] (T8): pushed to
    /// the back (newest), then the front is evicted until the deque is back
    /// within [`Self::history_cap`] -- the circular-buffer behavior the
    /// item spec asks for. Always resets browsing state
    /// ([`Self::history_index`]/[`Self::history_draft`]) so the NEXT `Up`
    /// starts a fresh recall from the newest entry, not wherever a previous
    /// (now-stale) browse left off. `App::submit` calls this before
    /// dispatching the text, then persists `history` to disk (best-effort --
    /// this method itself does no I/O, so it can never fail a submit).
    pub fn push_history(&mut self, text: String) {
        self.history_index = None;
        self.history_draft.clear();
        if self.history_cap == 0 {
            self.history.clear();
            return;
        }
        self.history.push_back(text);
        while self.history.len() > self.history_cap {
            self.history.pop_front();
        }
    }

    /// `Up` while composing (T8): recalls the previous (older) history
    /// entry into `input`, saving whatever was already typed as
    /// [`Self::history_draft`] the FIRST time this starts browsing (`Up`
    /// from `history_index == None`) so [`Self::history_recall_next`] can
    /// restore it later. Returns whether it fired -- `false` (no mutation)
    /// when `history` is empty, letting the caller's `Up` fall through to
    /// whatever it would otherwise do. Repeated calls walk toward the
    /// oldest entry and simply stop there (still consuming the key,
    /// returning `true`) rather than wrapping.
    pub fn history_recall_prev(&mut self) -> bool {
        if self.history.is_empty() {
            return false;
        }
        match self.history_index {
            None => {
                self.history_draft = self.input.clone();
                self.set_input_from_history(self.history.len() - 1);
                true
            }
            Some(0) => true,
            Some(i) => {
                self.set_input_from_history(i - 1);
                true
            }
        }
    }

    /// `Down`'s counterpart (T8): recalls the next (newer) history entry,
    /// or -- once `Down` walks past the newest entry -- restores whatever
    /// unsent draft [`Self::history_recall_prev`] saved when browsing
    /// started, and stops browsing (`history_index` back to `None`).
    /// Returns whether it fired -- `false` when not currently browsing
    /// (`history_index` is already `None`), letting the caller's `Down`
    /// fall through, exactly mirroring [`Self::history_recall_prev`]'s
    /// empty-history case.
    pub fn history_recall_next(&mut self) -> bool {
        match self.history_index {
            None => false,
            Some(i) if i + 1 < self.history.len() => {
                self.set_input_from_history(i + 1);
                true
            }
            Some(_) => {
                self.history_index = None;
                self.input = std::mem::take(&mut self.history_draft);
                self.cursor = self.input.chars().count();
                true
            }
        }
    }

    /// Shared by [`Self::history_recall_prev`]/[`Self::history_recall_next`]:
    /// loads `history[idx]` into `input`, moves `history_index` to `idx`,
    /// and puts the cursor at the recalled line's end -- so the recalled
    /// prompt is immediately editable inline (item spec) starting from
    /// where you'd naturally continue typing.
    fn set_input_from_history(&mut self, idx: usize) {
        self.history_index = Some(idx);
        self.input = self.history[idx].clone();
        self.cursor = self.input.chars().count();
    }

    /// The panel's visible rows under the current [`Self::agent_visibility`]
    /// filter (item A2), in tree insertion order. This is the ONLY thing the
    /// filter affects -- `tree.nodes` itself stays unfiltered, so provenance
    /// survives -- and it is what the panel's row count, selection clamping,
    /// and Enter-to-focus all index into.
    pub fn visible_agent_nodes(&self) -> impl Iterator<Item = &TreeNode> {
        let mode = self.agent_visibility;
        self.tree.nodes.iter().filter(move |n| mode.shows(n))
    }

    /// Cycles the `/agents` panel's visibility filter (item A2, the `v` key
    /// while the panel is open) and re-clamps the selection against the NEW
    /// filtered row count -- e.g. cycling to `ActiveOnly` while a finished
    /// agent's row was selected must not leave the cursor pointing past the
    /// (now shorter) row list.
    pub fn cycle_agent_visibility(&mut self) {
        self.agent_visibility = self.agent_visibility.next();
        self.clamp_agent_selected();
    }

    /// Clamps [`Self::agent_selected`] to the filtered row count (or 0 when
    /// the filter hides every row).
    fn clamp_agent_selected(&mut self) {
        let n = self.visible_agent_nodes().count();
        self.agent_selected = if n == 0 {
            0
        } else {
            self.agent_selected.min(n - 1)
        };
    }

    /// Moves the agent-panel selection by `delta` rows, clamped to the
    /// FILTERED row list (WI-130 + item A2). No wrap -- a browsing list
    /// stops at its ends.
    pub fn agent_scroll(&mut self, delta: isize) {
        let n = self.visible_agent_nodes().count();
        if n == 0 {
            return;
        }
        let max = (n - 1) as isize;
        let cur = (self.agent_selected.min(n - 1)) as isize;
        self.agent_selected = (cur + delta).clamp(0, max) as usize;
    }

    /// Shows/hides the below-chat agent-tree panel (`/agents`, WI-127
    /// criterion 4). A pure toggle -- no facade call, no transcript entry --
    /// so it is unit-testable with no `Host`/`SessionHandle` at all.
    pub fn toggle_agent_view(&mut self) {
        self.agent_view_open = !self.agent_view_open;
    }

    /// T5: flips `expanded` on EVERY `Entry::Tool` in the transcript at once
    /// (the `Ctrl-E` keybinding). MVP is all-at-once -- there is no
    /// transcript-cursor/selection state, so "expand/collapse all" is the
    /// only meaningful toggle. Pure state mutation: does NOT touch
    /// `scroll`/`follow_tail`/`max_scroll` -- the next render's existing
    /// clamp in `view/transcript.rs::draw` (`state.scroll.min(max)`)
    /// re-clamps to the nearest valid position without snapping the
    /// viewport (a toggle that shrinks the content height clamps an
    /// overscrolled `scroll` down to the new `max`; a toggle that grows it
    /// back restores the original `scroll` since it was never overwritten).
    /// Factored as a method (not inlined in `input.rs`) so the all-at-once
    /// behavior + the no-snap contract are directly unit-testable with no
    /// terminal/key event at all.
    pub fn toggle_all_tool_entries_expanded(&mut self) {
        for entry in self.transcript.iter_mut() {
            if let Entry::Tool { expanded, .. } = entry {
                *expanded = !*expanded;
            }
        }
    }

    /// `PageUp`: scrolls the transcript up by `page` (wrapped) lines and
    /// disengages auto-follow -- the user is now reviewing history, so new
    /// output must not yank the view back down (the transcript-scrolling
    /// item's own criterion: "scrolled-up state is not yanked to the bottom
    /// by new output"). Starts from `max_scroll` (i.e. the bottom) when
    /// `follow_tail` was still on, since that IS the view's current
    /// position even though `scroll` itself hasn't been tracking it.
    /// `max_scroll` is caller-computed (`app.rs`, via `view::max_scroll`) --
    /// this struct has no terminal width/height of its own to derive the
    /// wrapped line count from.
    pub fn scroll_page_up(&mut self, page: u16, max_scroll: u16) {
        let from = if self.follow_tail {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        self.scroll = from.saturating_sub(page);
        self.follow_tail = false;
    }

    /// `PageDown`: scrolls the transcript down by `page` lines, clamped to
    /// `max_scroll` (never blank overscroll past the true bottom).
    /// Re-engages auto-follow once the view lands back on the bottom (the
    /// item's own criterion: "returning to bottom re-enables follow").
    pub fn scroll_page_down(&mut self, page: u16, max_scroll: u16) {
        let from = if self.follow_tail {
            max_scroll
        } else {
            self.scroll.min(max_scroll)
        };
        self.scroll = from.saturating_add(page).min(max_scroll);
        self.follow_tail = self.scroll >= max_scroll;
    }

    /// `End` (T6): snaps the transcript straight to its own tail --
    /// re-engages [`Self::follow_tail`] and resets the stored [`Self::scroll`]
    /// to 0. The stored `scroll` value is meaningless once `follow_tail` is
    /// set (the next render draws from `max_scroll` instead -- see
    /// `follow_tail`'s own doc), so 0 here is just the same tidy reset
    /// [`Self::new`] itself starts from, not a value anything actually reads
    /// while following. The complement to [`Self::jump_to_top`].
    pub fn jump_to_tail(&mut self) {
        self.follow_tail = true;
        self.scroll = 0;
    }

    /// `Home` (T6): jumps the transcript straight to its own TOP --
    /// disengages `follow_tail` (reviewing history, same as
    /// [`Self::scroll_page_up`]) and seats `scroll` at 0, the oldest wrapped
    /// line. [`Self::scroll`]'s own doc is explicit that it counts "wrapped
    /// lines from the top", and
    /// `transcript::tests::explicit_scroll_shows_history_when_not_following`
    /// pins `scroll == 0` as showing the OLDEST entry (`scroll == max_scroll`
    /// is the tail) -- so 0, not `max_scroll`, is what "the top" means in
    /// this codebase's established scroll direction. Takes `max_scroll` for
    /// call-site symmetry with `Self::scroll_page_up`/`Self::scroll_page_down`
    /// (`app.rs`'s caller already has it in hand from `view::max_scroll` --
    /// mirroring how it drives every other terminal-size-derived scroll
    /// mutation) even though jumping to the top needs no clamping of its own:
    /// 0 is always a valid offset regardless of `max_scroll`.
    pub fn jump_to_top(&mut self, max_scroll: u16) {
        let _ = max_scroll;
        self.follow_tail = false;
        self.scroll = 0;
    }

    /// T6: how many wrapped lines currently sit BELOW the bottom of the
    /// viewport (i.e. between the current scroll position and the tail) --
    /// the count the floating "jump to bottom" footer names (`↓ N lines
    /// above tail`). Always 0 while [`Self::follow_tail`] is set (the
    /// viewport already IS at the tail); while scrolled up, it is
    /// `max_scroll` minus the same clamped effective scroll
    /// `view/transcript.rs::draw` renders from, so this can never disagree
    /// with what is actually on screen.
    pub fn lines_above_tail(&self, max_scroll: u16) -> u16 {
        if self.follow_tail {
            0
        } else {
            max_scroll.saturating_sub(self.scroll.min(max_scroll))
        }
    }

    /// Opens the `/ask` modal for one answered ask (B5), parking it in
    /// `pending_ask_modal` instead whenever another modal surface (a
    /// permission prompt, an intent confirmation card, or another ask
    /// modal) currently owns `mode` -- mirroring [`Self::offer_prompt`]'s
    /// own queue-if-busy behavior, so the modal-bearing surfaces never
    /// stack. [`Self::promote_next_surface`] opens the parked modal once
    /// the surface ahead of it clears.
    pub fn offer_ask_modal(&mut self, modal: AskModal) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::AskModal(modal);
            // V1: a freshly opened modal starts scrolled to its own top --
            // see `Self::modal_scroll`'s own doc on why one field can serve
            // every modal-bearing surface.
            self.modal_scroll = 0;
        } else {
            self.pending_ask_modal = Some(modal);
        }
    }

    /// Drains a modal parked in `pending_ask_modal` (B5's M1 fix). Used by
    /// `app.rs::purge_open_ask_modal` so the quit path also discards a
    /// modal that was queued behind a permission prompt when the ask
    /// completed -- without this the parked modal's child leaks as residue
    /// (reaped by the next startup sweep, but still a fourth way out of an
    /// undecided ask). Returns the parked modal if one was waiting, else
    /// `None`; either way `pending_ask_modal` is cleared.
    pub fn take_pending_ask_modal(&mut self) -> Option<AskModal> {
        self.pending_ask_modal.take()
    }

    /// Closes the `/ask` modal after a fate SUCCEEDED (B5 --
    /// `commands::apply_ask_fate`'s success path), promoting the next
    /// parked/queued surface via [`Self::promote_next_surface`] (a queued
    /// permission prompt, a parked intent card, or a parked ask -- in that
    /// priority order). A no-op when no ask modal is open.
    pub fn close_ask_modal(&mut self) {
        if !matches!(self.mode, Mode::AskModal(_)) {
            return;
        }
        self.mode = Mode::Normal;
        self.promote_next_surface();
    }

    /// Records a fate attempt's FAILURE on the open modal (B5 --
    /// `commands::apply_ask_fate`'s error path): the modal STAYS OPEN with
    /// the error shown, so the user still must choose a fate -- a failed
    /// fate never silently falls through to another one. A no-op when no
    /// modal is open.
    pub fn fail_ask_modal(&mut self, error: String) {
        if let Mode::AskModal(modal) = &mut self.mode {
            modal.error = Some(error);
        }
    }

    /// Opens the NL intent confirmation card (C2), parking it in
    /// `pending_intent_confirm` instead whenever another modal surface (a
    /// permission prompt or an `/ask` modal) currently owns `mode` --
    /// mirroring [`Self::offer_ask_modal`]'s parking behavior, so the
    /// modal-bearing surfaces never stack. [`Self::promote_next_surface`]
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

    /// Drains a card parked in `pending_intent_confirm`. Used by
    /// `app.rs`'s quit path so a card parked behind a permission prompt
    /// when the user quits does not leave a dangling classified intent --
    /// unlike the `/ask` modal there is no live child to purge (the card
    /// opens BEFORE any agent is created), so "draining" here just means
    /// dropping it on the floor. Returns the parked card if one was
    /// waiting, else `None`; either way `pending_intent_confirm` is
    /// cleared.
    pub fn take_pending_intent_confirm(&mut self) -> Option<IntentConfirm> {
        self.pending_intent_confirm.take()
    }

    /// Closes the intent confirmation card (C2) after a `Confirm` or
    /// `Manual` choice, promoting the next parked/queued surface via
    /// [`Self::promote_next_surface`]. A no-op when no card is open.
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

    /// The shared "what surfaces gets promoted next after a modal/prompt
    /// closes" logic (C2 generalizes B5's two-surface version to three).
    /// Called with `mode` already reset to `Mode::Normal` by the caller
    /// ([`Self::close_ask_modal`], [`Self::close_intent_confirm`],
    /// [`Self::resolve_current_prompt`]). Priority order:
    /// 1. A queued permission prompt ([`Self::queued_prompts`]) -- the
    ///    gate's pending prompts are always the highest-priority surface
    ///    (a tool call is waiting on a decision).
    /// 2. A parked `/ask` modal ([`Self::pending_ask_modal`]) -- an ask
    ///    that completed while a prompt was showing.
    /// 3. A parked intent card ([`Self::pending_intent_confirm`]) -- a
    ///    classify that completed while a prompt or an ask was showing.
    /// 4. Nothing -- `mode` stays `Normal`.
    ///
    /// Exactly one surface (at most) is promoted per call; the next call
    /// happens when THAT surface closes.
    fn promote_next_surface(&mut self) {
        // V1: every branch below resets `modal_scroll` -- the newly
        // promoted surface starts scrolled to its own top, never carrying
        // over wherever a PREVIOUS, unrelated surface's content happened to
        // be scrolled (see `Self::modal_scroll`'s own doc on why one field
        // safely serves all of them).
        if let Some(next) = self.queued_prompts.pop_front() {
            self.mode = Mode::AwaitingPermission(next);
            self.modal_scroll = 0;
            // A scope chosen for the PREVIOUS prompt must not leak into
            // this one -- see `Self::permission_grant_scope`'s own doc.
            self.permission_grant_scope = conway::PermissionScope::Session;
            return;
        }
        if let Some(modal) = self.pending_ask_modal.take() {
            self.mode = Mode::AskModal(modal);
            self.modal_scroll = 0;
            return;
        }
        if let Some(card) = self.pending_intent_confirm.take() {
            self.mode = Mode::IntentConfirm(card);
            self.modal_scroll = 0;
        }
    }

    /// Enqueues a freshly arrived prompt from the gate channel, promoting it
    /// to `mode` immediately if nothing is currently showing.
    pub fn offer_prompt(&mut self, prompt: PendingPrompt) {
        if matches!(self.mode, Mode::Normal) {
            self.mode = Mode::AwaitingPermission(prompt);
            // A freshly promoted prompt starts scrolled to the top of its
            // own command -- never carries over wherever a PREVIOUS,
            // unrelated prompt's overlay happened to be scrolled.
            self.modal_scroll = 0;
            // ...and at the DEFAULT grant scope -- a narrower scope chosen
            // for an earlier, unrelated prompt must not silently apply to
            // this one (see `Self::permission_grant_scope`'s own doc).
            self.permission_grant_scope = conway::PermissionScope::Session;
        } else {
            self.queued_prompts.push_back(prompt);
        }
    }

    /// Cycles the scope the prompt's remembered-grant keys (`a`/`p`) grant
    /// at: `Session` -> `Agent` -> `AgentSubtree` -> `Session`. Bound to
    /// the prompt's `s` key in `input.rs`; the overlay states the current
    /// scope in words next to the grant keys so the operator never grants
    /// narrower or broader than they can see.
    pub fn cycle_permission_grant_scope(&mut self) {
        self.permission_grant_scope = match self.permission_grant_scope {
            conway::PermissionScope::Session => conway::PermissionScope::Agent,
            conway::PermissionScope::Agent => conway::PermissionScope::AgentSubtree,
            // `AgentSubtree`, and any future variant (`PermissionScope` is
            // `#[non_exhaustive]`): back to the default. A future scope
            // sorts itself into the cycle only by a deliberate edit here,
            // never by accident.
            _ => conway::PermissionScope::Session,
        };
    }

    /// The agent whose call the current permission prompt is asking about,
    /// if one is pending. This -- NOT `focused_agent` -- is the agent a
    /// per-agent or per-subtree grant must be recorded against: the broker
    /// narrows such a grant to the GRANTING agent's identity, and the call
    /// being decided belongs to the requester in the prompt, which need
    /// not be the agent whose transcript the operator is looking at.
    pub fn pending_permission_agent(&self) -> Option<conway::AgentId> {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return None;
        };
        Some(pending.request.agent_id)
    }

    /// Resolves the currently-shown prompt (if any) and promotes the next
    /// queued one, if there is one.
    pub fn resolve_current_prompt(&mut self, decision: conway::PermissionDecision) {
        let Mode::AwaitingPermission(_) = &self.mode else {
            return;
        };
        let Mode::AwaitingPermission(prompt) = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            unreachable!()
        };
        prompt.resolve(decision);
        // C2: the prompt queue drains first (highest priority -- a tool
        // call is waiting on a decision); then a parked `/ask` modal (B5);
        // then a parked intent card (C2). [`Self::promote_next_surface`]
        // encodes that fixed priority order so the three modal-bearing
        // surfaces never stack and never drift out of sync across the
        // close/resolve call sites.
        self.promote_next_surface();
    }

    /// Opens the `/help` keybinding overlay (T7). See [`Self::help_open`]'s
    /// own doc for why this is a plain flag flip rather than a `mode`
    /// transition/park -- `commands.rs`'s `SlashCommand::Help` arm can only
    /// ever reach this while `mode` is already `Normal` (the input line is
    /// inert otherwise), so there is nothing to park against. V1: also
    /// resets `modal_scroll` -- see that field's own doc on why one shared
    /// field serves every modal-bearing surface, `/help` included.
    pub fn open_help(&mut self) {
        self.help_open = true;
        self.modal_scroll = 0;
        // V4: mutually exclusive with the settings menu -- see
        // `Self::settings_open`'s own doc for why both flags clear each
        // other on open rather than relying on check-order to keep at most
        // one of them showing.
        self.settings_open = false;
    }

    /// Closes the `/help` keybinding overlay (T7's `Esc` binding, wired in
    /// `input.rs`). A no-op when it is already closed.
    pub fn close_help(&mut self) {
        self.help_open = false;
    }

    /// Opens the `/settings` menu (V4). Mirrors [`Self::open_help`] exactly
    /// -- see [`Self::settings_open`]'s own doc for the full "informational,
    /// gated ahead of `Mode::Normal`, mutually exclusive with `/help`"
    /// reasoning. Deliberately does NOT reset [`Self::settings_selected`] or
    /// [`Self::settings_collapsed_groups`] -- re-opening the menu within the
    /// same session restores wherever the cursor/collapse state was left,
    /// the same way re-opening the `/agents` panel does not reset
    /// `agent_selected`.
    /// V2b: the pattern grant Conway would offer for the pending
    /// permission prompt, if any.
    ///
    /// Shaped by the pending call's own `render_kind` (carried on the
    /// request from the broker -- the same declaration the evaluation side
    /// matched against): a `ShellCommand` tool gets the narrow two-token
    /// prefix, or no offer at all when the command carries shell
    /// metacharacters; a `Structured` tool gets the registerable wildcard
    /// (`tool:*`), the only rule shape F12's registration check admits
    /// against a JSON-dump rendering. See
    /// `permission_pattern::suggested_rule` for the full reasoning.
    pub fn offered_permission_rule(&self) -> Option<conway::PatternRule> {
        let Mode::AwaitingPermission(pending) = &self.mode else {
            return None;
        };
        conway::permission_pattern::suggested_rule(
            pending.request.tool.as_str(),
            &pending.request.rendered,
            pending.request.render_kind,
        )
    }

    pub fn open_settings(&mut self) {
        self.settings_open = true;
        self.help_open = false;
    }

    /// Closes the `/settings` menu (V4's `Esc` binding, wired in
    /// `input.rs`). A no-op when it is already closed. Cursor/collapse
    /// state is left untouched (see [`Self::open_settings`]'s own doc).
    pub fn close_settings(&mut self) {
        self.settings_open = false;
    }

    /// Seeds a `/agents` tree node for a freshly created interactive child
    /// (bare `/spawn` or `/fork`) immediately, so the panel shows it the
    /// instant the session exists -- WITHOUT waiting for `child`'s own
    /// `Event::AgentSpawned` to reach [`Self::apply`].
    ///
    /// That event never arrives on the stream the app switches to: the app
    /// swaps its subscription to `handle.agent_events(child)` the same turn
    /// the child is created, and that stream's replay is `child`'s own
    /// session records only -- which never contain its own spawn lifecycle
    /// event (`record_to_event` maps stored `LogRecord`s, and `AgentSpawned`
    /// is not one) -- while its live half was subscribed only AFTER
    /// `host.spawn`/`host.fork` already broadcast the event. The old
    /// (parent) subscription HAD buffered that `AgentSpawned`, but it is
    /// dropped, undrained, when the app replaces `events` with the child's
    /// stream. Seeding here closes that gap directly, from the id and parent
    /// the app already holds.
    ///
    /// Idempotent and safe against a later real `AgentSpawned` for the same
    /// agent: if the node already exists this is a no-op, and
    /// [`Self::apply_agent_spawned`] itself no-ops (only refreshes status)
    /// when the tree already contains the agent. No transcript entry is
    /// pushed -- unlike `apply_agent_spawned`, which would push an inline
    /// `Entry::Agent` under the (about-to-be-unfocused) parent, only for
    /// [`Self::focus_agent`] to clear it a moment later.
    pub fn ensure_agent_tracked(&mut self, agent: AgentId, parent: AgentId) {
        if self.tree.contains(agent) {
            return;
        }
        let attach = if self.tree.contains(parent) {
            Some(parent)
        } else {
            self.tree.root
        };
        self.tree.insert(TreeNode {
            agent_id: agent,
            parent: attach,
            agent_def: None,
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
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
                // Board item 01KYAGP11FF9YC3G60TWHHKKST: the focused
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
            // Bug 2 fix (01KYAN9EQ5BRZQ0V3DCW590YCZ): without this arm,
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
            Event::TextDelta { text } => {
                self.append_assistant_text(text, env.ts);
                if env.agent == self.focused_agent {
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
            // (WI-115) -- this arm only updates the display-name/max-context
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
                // Board item 01KYAGP11FF9YC3G60TWHHKKST: the live-increment
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
            // board item 01KYND6GCCKYSYD0VDGJD1ZCXG: was pushed as an
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
            // WI-140 review fix (finding 1, CRITICAL): this used to fall
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

    fn apply_agent_spawned(
        &mut self,
        agent: AgentId,
        kind: SubagentMode,
        parent: Option<AgentId>,
        agent_def: Option<String>,
        inherited_upto: Option<LogSeq>,
        ephemeral: bool,
    ) {
        if self.tree.contains(agent) {
            // Already seeded (e.g. the root, inserted by `new`).
            if let Some(node) = self.tree.get_mut(agent) {
                node.status = NodeStatus::Running;
            }
            return;
        }
        // Recognized-parent spawns get an inline `Entry::Agent` (WI-127
        // criterion 4: "subagent activity appears inline in the stream").
        // The unknown-parent case below is deliberately excluded: it already
        // gets its own `Notice`, and that `Notice` must stay the LAST entry
        // pushed here -- existing tests (this module's own
        // `agent_spawned_with_unknown_parent_attaches_under_root_and_notes_it`)
        // assert `transcript.last()` is that `Notice`.
        let attach = match parent {
            None => {
                if self.tree.root.is_none() {
                    self.tree.root = Some(agent);
                }
                None
            }
            Some(p) if self.tree.contains(p) => {
                // Inline `Entry::Agent` only when this spawn belongs to the
                // agent whose conversation is currently shown -- i.e. `p` is
                // the focused agent, so the spawned child is a direct child
                // of the focused view. Otherwise a sibling/unrelated
                // subtree's spawn would leak into a focused agent's
                // transcript, which is supposed to show only its own
                // conversation (the tree model itself is still updated
                // unconditionally by `self.tree.insert` below, so the
                // `/agents` panel stays complete regardless of focus).
                // Ephemeral `/ask`-style asides never get an inline entry:
                // their UI surface is the B5 single-turn modal
                // (`Mode::AskModal`, driven by `app.rs`'s `/ask` flow); the
                // tree insert below is still unconditional for them.
                if p == self.focused_agent && !ephemeral {
                    self.transcript.push(Entry::Agent {
                        agent_id: agent,
                        label: agent_def.clone().unwrap_or_else(|| "agent".to_string()),
                        status: NodeStatus::Running,
                    });
                }
                Some(p)
            }
            Some(p) => {
                // A tree-integrity diagnostic: only surface it in the root
                // view, never as noise inside a focused child's transcript.
                if self.is_root_focused() {
                    self.transcript.push(Entry::Notice {
                        text: format!(
                            "agent {agent} claimed unknown parent {p}; attached under root"
                        ),
                    });
                }
                self.tree.root
            }
        };
        self.tree.insert(TreeNode {
            agent_id: agent,
            parent: attach,
            agent_def,
            status: NodeStatus::Running,
            kind: Some(kind),
            inherited_upto,
            ephemeral,
        });
    }

    fn apply_agent_finished(&mut self, agent: AgentId, result: &AgentResult) {
        let status = match result.status {
            ResultStatus::Completed => NodeStatus::Finished,
            ResultStatus::Failed { .. } => NodeStatus::Failed,
            ResultStatus::Cancelled { .. } => NodeStatus::Cancelled,
            ResultStatus::Rejected { .. } => NodeStatus::Failed,
            ResultStatus::BudgetExceeded { .. } => NodeStatus::Finished,
            _ => NodeStatus::Finished,
        };
        self.set_tree_status(agent, status);
        // Updates the SAME `Entry::Agent` pushed at spawn time in place --
        // never appends -- so a non-root agent finishing does not grow the
        // transcript (this module's own
        // `non_root_agent_finished_does_not_push_a_session_ended_notice`
        // relies on exactly this). A no-op for the root (which never gets
        // an `Entry::Agent`, per `apply_agent_spawned`'s doc) and for the
        // unknown-parent case (same reason).
        self.set_agent_entry_status(agent, status);

        // A `keep_alive` root's task can end for any reason (budget/
        // deadline/cancel) with no other visible signal -- the TUI simply
        // stops getting replies otherwise, indistinguishable from a hang.
        // Surface the terminal reason as a transcript `Notice` whenever the
        // finishing agent is the ROOT specifically (a subagent/fork child
        // finishing is unremarkable and already reflected by its own tree
        // node status). `Completed` should never occur for a keep-alive
        // root in practice, but is handled the same way rather than
        // special-cased.
        // Only in the root view: the root session ending is about the root's
        // own conversation. A user focused on a child agent's transcript must
        // not have a "session ended" notice injected into it (the root's tree
        // node status already reflects the end for the /agents panel).
        if self.tree.root == Some(agent) && self.is_root_focused() {
            self.transcript.push(Entry::Notice {
                text: format!("session ended: {}", terminal_reason(&result.status)),
            });
        }
    }

    fn set_tree_status(&mut self, agent: AgentId, status: NodeStatus) {
        if let Some(node) = self.tree.get_mut(agent) {
            node.status = status;
        }
    }

    fn set_agent_entry_status(&mut self, agent: AgentId, status: NodeStatus) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Agent {
                agent_id: id,
                status: s,
                ..
            } = entry
            {
                if *id == agent {
                    *s = status;
                    return;
                }
            }
        }
    }

    fn append_assistant_text(&mut self, delta: &str, ts: DateTime<Utc>) {
        if let Some(Entry::Assistant { text, .. }) = self.transcript.last_mut() {
            text.push_str(delta);
        } else {
            self.transcript.push(Entry::Assistant {
                text: delta.to_string(),
                // T4: stamp the serving model from the live focus. Replay
                // (`record_to_event` maps a stored `Assistant` record to a
                // bare `TextDelta` carrying no model) leaves `focused_model`
                // as whatever the live focus happens to be -- but a replay
                // envelope is only ever applied on the focused agent's
                // stream, and the renderer omits the marker when `None`,
                // which is the backward-compatible shape for a replayed
                // bubble that has no model provenance.
                model: self.focused_model.clone(),
                summary: None,
                ts: Some(ts),
            });
        }
    }

    /// T4: append a reasoning-trace delta (from `Event::ThinkingDelta`),
    /// mirroring [`append_assistant_text`]. Creates a new
    /// [`Entry::Reasoning`] on the first delta of a run (stamping the
    /// serving model + envelope timestamp), or appends to the last
    /// `Reasoning` entry if one is already in progress. Reasoning is
    /// EXPANDED by default (the `show_reasoning` flag defaults `true`);
    /// `build_lines` skips `Entry::Reasoning` entirely when the flag is
    /// `false`, but the entries are still STORED, so toggling back on
    /// restores them without replay.
    fn append_reasoning_text(&mut self, delta: &str, ts: DateTime<Utc>) {
        if let Some(Entry::Reasoning { text, .. }) = self.transcript.last_mut() {
            text.push_str(delta);
        } else {
            self.transcript.push(Entry::Reasoning {
                text: delta.to_string(),
                model: self.focused_model.clone(),
                summary: None,
                ts: Some(ts),
            });
        }
    }

    /// T4: append a `ToolProgress { call_id, note }` note to the matching
    /// in-flight [`Entry::Tool`] by `call_id` (previously dropped by the
    /// wildcard arm). Joined with `\n` -- the renderer emits each as a dim
    /// `-> {note}` line. A no-op if no tool entry with that `call_id` exists
    /// (never panics on untrusted input).
    fn append_tool_progress(&mut self, call_id: &str, note: &str) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                progress,
                ..
            } = entry
            {
                if id == call_id {
                    if !progress.is_empty() {
                        progress.push('\n');
                    }
                    progress.push_str(note);
                    return;
                }
            }
        }
    }

    /// T4: toggle the `show_reasoning` flag. V4: the one caller of this is
    /// now the `/settings` menu's `Enter` key on the "show reasoning traces"
    /// leaf (`input::handle_settings_key`) -- the standalone `/thinking`
    /// slash command this originally backed is REMOVED, not aliased (see
    /// `commands.rs`'s module doc), but the toggle itself is unchanged: same
    /// field, same flip, same return value.
    pub fn toggle_thinking(&mut self) -> bool {
        self.show_reasoning = !self.show_reasoning;
        self.show_reasoning
    }

    /// T4: toggle the `show_timestamps` flag. V4: now called from the
    /// `/settings` menu's `Enter` key on the "show timestamps" leaf, exactly
    /// as [`Self::toggle_thinking`]'s doc describes for its own removed
    /// `/thinking` command -- the standalone `/timestamps` command is
    /// REMOVED, the toggle is not.
    pub fn toggle_timestamps(&mut self) -> bool {
        self.show_timestamps = !self.show_timestamps;
        self.show_timestamps
    }

    /// V4: adjusts `tool_preview_lines` by `delta` -- the `/settings` menu's
    /// Left(`-1`)/Right(`+1`) numeric stepper for the one non-boolean
    /// setting. Floors/caps at [`TOOL_PREVIEW_LINES_RANGE`]'s own bounds
    /// rather than routing the stepped value through
    /// [`clamp_tool_preview_lines`] directly: that function's job is
    /// validating an untrusted CONFIG value, where out-of-range means
    /// "malformed, fall back to the built-in default (3)" -- applying that
    /// same fallback to an interactive stepper would make pressing Left at
    /// the floor (1) bounce UP to 3 instead of simply stopping, which reads
    /// as broken, not as a safety net. Both functions still share the ONE
    /// range constant (no independently-typed-in second bounds check
    /// that could silently drift from it) -- only the OUT-OF-RANGE behavior
    /// differs, matched to what each caller actually needs. Never panics on
    /// any `delta` (`saturating_add` on a widened `i64` before the final
    /// clamp). Returns the new value.
    pub fn adjust_tool_preview_lines(&mut self, delta: i32) -> u32 {
        let stepped = i64::from(self.tool_preview_lines).saturating_add(i64::from(delta));
        let floor = i64::from(*TOOL_PREVIEW_LINES_RANGE.start());
        let ceil = i64::from(*TOOL_PREVIEW_LINES_RANGE.end());
        self.tool_preview_lines = stepped.clamp(floor, ceil) as u32;
        self.tool_preview_lines
    }

    /// T4: stamp the turn-end summary (`1m 6s · 1.4k tok (88% cached)`)
    /// onto the last `Entry::Assistant` or `Entry::Reasoning` block in the
    /// transcript. Called from the `TurnFinished` arm BEFORE
    /// [`clear_turn_state`] zeroes `turn_started_at` (the elapsed figure
    /// reads `turn_started_at.elapsed()`). A no-op if THIS TURN produced no
    /// Assistant/Reasoning block to attach to (e.g. a turn that produced
    /// only tool calls)
    /// -- the summary is genuinely about a model-emitted block, so attaching
    /// it to a bare tool entry would be misleading; the status line's own
    /// token figures still convey the spend. Stamps onto Reasoning if it is
    /// the last block (the trace is what the user sees last in that case),
    /// else onto the last Assistant block.
    ///
    /// The scan is bounded below by [`Self::turn_transcript_start`], the
    /// transcript length at `TurnStarted`. An UNBOUNDED scan would walk a
    /// tool-only turn's own entries and then keep going into the PREVIOUS
    /// turn, overwriting an already-settled bubble's summary with this
    /// turn's elapsed/token figures -- misattributing spend to an unrelated
    /// reply.
    fn stamp_turn_summary(&mut self, usage: &Usage) {
        let elapsed_secs = self
            .turn_started_at
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let summary = format_turn_summary(elapsed_secs, usage);
        // Clamp defensively: a `TurnFinished` with no preceding `TurnStarted`
        // (or a transcript cleared mid-turn) must never index out of range.
        let start = self.turn_transcript_start.min(self.transcript.len());
        for entry in self.transcript[start..].iter_mut().rev() {
            match entry {
                Entry::Reasoning { summary: s, .. } | Entry::Assistant { summary: s, .. } => {
                    *s = Some(summary);
                    return;
                }
                _ => {}
            }
        }
    }

    fn set_tool_status(&mut self, call_id: &str, status: ToolStatus) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                status: s,
                ..
            } = entry
            {
                if id == call_id {
                    *s = status;
                    return;
                }
            }
        }
    }

    fn finish_tool(&mut self, call_id: &str, is_error: bool, preview: String) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::Tool {
                call_id: id,
                status,
                preview: p,
                ..
            } = entry
            {
                if id == call_id {
                    *status = ToolStatus::Finished { is_error };
                    *p = preview;
                    return;
                }
            }
        }
    }
}

/// A short, human-readable rendering of a terminal `ResultStatus`, for the
/// root-session-ended `Notice` in [`AppState::apply_agent_finished`].
/// `ResultStatus` is `#[non_exhaustive]`; the wildcard arm is forward
/// compatibility, not a modeled case.
fn terminal_reason(status: &ResultStatus) -> String {
    match status {
        ResultStatus::Completed => "completed".to_string(),
        ResultStatus::Failed { error } => format!("failed: {error}"),
        ResultStatus::Cancelled { reason } => format!("cancelled: {reason}"),
        ResultStatus::BudgetExceeded { limit } => format!("budget exceeded ({limit})"),
        ResultStatus::Rejected { missing } => format!("rejected: {}", missing.join(", ")),
        _ => "unknown".to_string(),
    }
}

/// T4: compact token-count formatting for the turn-end summary. `< 1000`
/// renders as-is; `>= 1000` renders as `{k}.{tenths}k` (e.g. `12345` ->
/// `12.3k`). Mirrors [`crate::tui::view::status::compact_tokens`] (which is
/// private to the status module); duplicated here rather than made `pub` to
/// keep the status module's helpers private to the status line's own
/// rendering surface, matching the existing module boundaries.
fn compact_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n / 1000;
    let tenths = (n % 1000) / 100;
    format!("{k}.{tenths}k")
}

/// T4: format the turn-end summary line (`1m 6s · 1.4k tok (88% cached)`)
/// from the elapsed seconds (read from `turn_started_at` before
/// `clear_turn_state` zeroes it) and the turn's `Usage`. Elapsed is `1m 6s`
/// for >= 60s, else `{secs}s`. Tokens is the sum of every `Usage` field
/// (matching [`crate::tui::view::status::spent_tokens`]); the cache hit
/// rate is `cache_read / (input + cache_read + cache_write)`, omitted when
/// the denominator is zero or no cache read occurred (same formula as the
/// status line's `tokens` field). Never panics on untrusted input: no division by zero
/// -- the cache % is only computed when `denom != 0`.
fn format_turn_summary(elapsed_secs: u64, usage: &Usage) -> String {
    let elapsed = if elapsed_secs >= 60 {
        let m = elapsed_secs / 60;
        let s = elapsed_secs % 60;
        format!("{m}m {s}s")
    } else {
        format!("{elapsed_secs}s")
    };
    let total = u64::from(usage.input_tokens)
        + u64::from(usage.output_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens)
        + u64::from(usage.reasoning_tokens);
    let denom = u64::from(usage.input_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens);
    if denom == 0 || usage.cache_read_tokens == 0 {
        format!("{elapsed} · {} tok", compact_tokens(total))
    } else {
        let pct = (u64::from(usage.cache_read_tokens) * 100) / denom;
        format!("{elapsed} · {} tok ({pct}% cached)", compact_tokens(total))
    }
}

/// T5's valid range for `tool_preview_lines` (`1..=200`), factored out as a
/// named constant (V4) so [`clamp_tool_preview_lines`] (config validation,
/// which falls back to the built-in default on ANY out-of-range value) and
/// [`AppState::adjust_tool_preview_lines`] (the `/settings` menu's
/// interactive stepper, which floors/caps at the boundary instead) share
/// ONE source of truth for the bound -- no second, independently
/// typed-in bounds check that could silently drift from this one. The
/// `1..=200` range itself keeps the cap meaningful (a cap of 0 would
/// collapse every preview to zero content lines + the affordance; a cap of
/// `u32::MAX` would effectively disable folding, defeating T5's purpose).
const TOOL_PREVIEW_LINES_RANGE: std::ops::RangeInclusive<u32> = 1..=200;

/// T5: clamps a loaded `[tui.tool_preview_lines]` config value into a safe
/// render-time cap. `None` (the serde default for the `Option<u32>` field)
/// -> the built-in default of 3. A value in [`TOOL_PREVIEW_LINES_RANGE`] is
/// kept as-is. Any other value (0, > 200, or a value that failed to parse
/// as `u32` and so arrived as `None`) falls back to the default of 3.
/// Config is untrusted input -- this function never panics, and there is no
/// `unwrap`/`expect`/indexing on the config value (the `?`-shaped
/// `and_then` + `unwrap_or` chain is the entire bound on `n`).
pub fn clamp_tool_preview_lines(n: Option<u32>) -> u32 {
    n.and_then(|v| {
        if TOOL_PREVIEW_LINES_RANGE.contains(&v) {
            Some(v)
        } else {
            None
        }
    })
    .unwrap_or(3)
}

/// T8: [`AppState::new`]'s default [`AppState::history_cap`] -- overridden
/// at `App::new` by [`clamp_history_size`] against the loaded
/// `[tui.history_size]` config.
pub const DEFAULT_HISTORY_SIZE: usize = 500;

/// T8: clamps a loaded `[tui.history_size]` config value into a safe
/// history-cap, the same shape as [`clamp_tool_preview_lines`]: config is
/// untrusted input, so never a panic and no `unwrap`/`expect`/indexing on
/// the value. `None` -> [`DEFAULT_HISTORY_SIZE`]. A value in
/// `1..=100_000` is kept as-is (converted to `usize`, infallible on every
/// platform this project targets). Any other value (`0`, `> 100_000`) falls
/// back to the default -- `0` is technically a valid cap
/// ([`AppState::push_history`] handles it), but silently keeping NO history
/// is more likely a typo than an intent, so it is treated the same as an
/// out-of-range value here, matching `clamp_tool_preview_lines`'s own
/// zero-falls-back-to-default precedent.
pub fn clamp_history_size(n: Option<u32>) -> usize {
    n.and_then(|v| {
        if (1..=100_000).contains(&v) {
            Some(v as usize)
        } else {
            None
        }
    })
    .unwrap_or(DEFAULT_HISTORY_SIZE)
}

#[cfg(test)]
mod tests {
    use conway::{
        AgentResult, PermissionDecisionKind, ResultStatus, SessionId, SubagentMode, ToolName, Usage,
    };

    use super::*;

    fn envelope(session: SessionId, agent: AgentId, event: Event) -> Envelope {
        Envelope {
            seq: 0,
            ts: chrono::Utc::now(),
            session,
            agent,
            event,
        }
    }

    fn spawned(parent: Option<AgentId>) -> Event {
        Event::AgentSpawned {
            kind: SubagentMode::Spawn,
            parent,
            agent_def: None,
            inherited_upto: None,
            ephemeral: false,
        }
    }

    // ---- board item 01KYND6GCCKYSYD0VDGJD1ZCXG: `Event::Error` -> `Entry::Error` ----

    /// `Event::Error { fatal: true }` pushes a dedicated `Entry::Error`
    /// (never `Entry::Notice`), carrying `fatal: true` and the `"fatal "`
    /// text prefix through to the entry -- the prefix is kept even though
    /// severity is now structural, because a clean-copied transcript carries
    /// no style at all, so the word is the only surviving trace.
    #[test]
    fn fatal_error_event_pushes_dedicated_error_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::Error {
                error: conway_core::error::ConwayError::Config {
                    detail: "boom".to_string(),
                },
                fatal: true,
            },
        ));

        assert_eq!(state.transcript.len(), 1);
        match &state.transcript[0] {
            Entry::Error { text, fatal } => {
                assert!(*fatal);
                assert!(
                    text.starts_with("fatal error:"),
                    "expected the 'fatal ' prefix to survive into the entry text: {text:?}"
                );
            }
            other => panic!("expected Entry::Error, got {other:?}"),
        }
    }

    /// `Event::Error { fatal: false }` is a real, recoverable error -- it
    /// also gets `Entry::Error`, not `Entry::Notice`, just with `fatal:
    /// false` and no `"fatal "` prefix in the text.
    #[test]
    fn non_fatal_error_event_pushes_dedicated_error_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::Error {
                error: conway_core::error::ConwayError::Config {
                    detail: "retrying".to_string(),
                },
                fatal: false,
            },
        ));

        assert_eq!(state.transcript.len(), 1);
        match &state.transcript[0] {
            Entry::Error { text, fatal } => {
                assert!(!*fatal);
                assert!(
                    text.starts_with("error:") && !text.starts_with("fatal error:"),
                    "non-fatal must not carry the 'fatal ' prefix: {text:?}"
                );
            }
            other => panic!("expected Entry::Error, got {other:?}"),
        }
    }

    /// The exact event sequence from this item's own criterion: one
    /// coalesced "ab" assistant message, one completed tool-call entry, and
    /// a tree with the one (root) node in `Finished` state.
    #[test]
    fn full_turn_sequence_yields_coalesced_text_completed_tool_and_finished_tree() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        let events = vec![
            spawned(None),
            Event::TextDelta {
                text: "a".to_string(),
            },
            Event::TextDelta {
                text: "b".to_string(),
            },
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
            Event::PermissionRequested {
                call_id: "tc_1".to_string(),
                rendered: "bash: ls".to_string(),
            },
            Event::PermissionResolved {
                call_id: "tc_1".to_string(),
                decision: PermissionDecisionKind::AllowOnce,
            },
            Event::ToolCallFinished {
                call_id: "tc_1".to_string(),
                is_error: false,
                preview: "ok".to_string(),
            },
            Event::AgentFinished {
                result: AgentResult::new(root, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ];
        for event in events {
            state.apply(&envelope(session, root, event));
        }

        let assistant_texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            assistant_texts,
            vec!["ab"],
            "TextDeltas must coalesce into one Assistant entry"
        );

        let completed_tools = state
            .transcript
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Entry::Tool {
                        status: ToolStatus::Finished { is_error: false },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            completed_tools, 1,
            "expected exactly one completed tool-call entry"
        );

        assert_eq!(state.tree.nodes.len(), 1, "expected exactly one tree node");
        assert_eq!(state.tree.nodes[0].agent_id, root);
        assert_eq!(state.tree.nodes[0].status, NodeStatus::Finished);
    }

    #[test]
    fn lagged_appends_a_visible_notice_without_panicking() {
        let mut state = AppState::new(AgentId::new());
        let before = state.transcript.len();

        state.apply(&envelope(
            SessionId::new(),
            AgentId::new(),
            Event::Lagged { skipped: 7 },
        ));

        assert_eq!(state.transcript.len(), before + 1);
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { .. })
        ));
    }

    #[test]
    fn agent_spawned_with_known_parent_attaches_under_it() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();

        state.apply(&envelope(session, child, spawned(Some(root))));

        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("child node must be present");
        assert_eq!(node.parent, Some(root));
    }

    #[test]
    fn agent_spawned_with_unknown_parent_attaches_under_root_and_notes_it() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let unknown_parent = AgentId::new();
        let child = AgentId::new();
        let before = state.transcript.len();

        // Must not panic: the whole point of this criterion.
        state.apply(&envelope(session, child, spawned(Some(unknown_parent))));

        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("child node must still be attached, under root");
        assert_eq!(node.parent, Some(root));
        assert!(
            state.transcript.len() > before,
            "expected a diagnostic notice about the unknown parent"
        );
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { .. })
        ));
    }

    /// The critical companion fix: a keep-alive root session that ends for
    /// ANY reason must leave a visible trace in the transcript, never just
    /// stop responding with no explanation.
    #[test]
    fn root_agent_finished_pushes_a_visible_session_ended_notice() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let before = state.transcript.len();

        state.apply(&envelope(
            session,
            root,
            Event::AgentFinished {
                result: AgentResult::new(
                    root,
                    session,
                    ResultStatus::BudgetExceeded {
                        limit: "max_steps=3".to_string(),
                    },
                    "",
                ),
                ephemeral: false,
            },
        ));

        assert_eq!(state.transcript.len(), before + 1);
        match state.transcript.last() {
            Some(Entry::Notice { text }) => {
                assert!(
                    text.contains("session ended") && text.contains("budget exceeded"),
                    "expected a session-ended budget-exceeded notice, got: {text}"
                );
            }
            other => panic!("expected a Notice entry, got {other:?}"),
        }
    }

    /// A non-root agent (subagent/fork child) finishing must NOT emit the
    /// root-session-ended notice -- that would be misleading noise for
    /// every ordinary spawned/forked child completion.
    #[test]
    fn non_root_agent_finished_does_not_push_a_session_ended_notice() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.apply(&envelope(session, child, spawned(Some(root))));
        let before = state.transcript.len();

        state.apply(&envelope(
            session,
            child,
            Event::AgentFinished {
                result: AgentResult::new(child, session, ResultStatus::Completed, "child done"),
                ephemeral: false,
            },
        ));

        assert_eq!(
            state.transcript.len(),
            before,
            "a non-root AgentFinished must not push a session-ended notice"
        );
    }

    /// WI-127 criterion 4: a recognized-parent spawn must show up inline in
    /// the conversation stream, not only in `state.tree`.
    #[test]
    fn agent_spawned_with_known_parent_pushes_an_inline_agent_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();

        state.apply(&envelope(session, child, spawned(Some(root))));

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Agent { agent_id, status: NodeStatus::Running, .. }) if *agent_id == child
        ));
    }

    /// The finish half of the same criterion: the entry updates in place --
    /// no second entry for the same agent.
    #[test]
    fn agent_finished_updates_its_own_inline_entry_status_in_place() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.apply(&envelope(session, child, spawned(Some(root))));
        let agent_entries_before = state
            .transcript
            .iter()
            .filter(|e| matches!(e, Entry::Agent { .. }))
            .count();

        state.apply(&envelope(
            session,
            child,
            Event::AgentFinished {
                result: AgentResult::new(child, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));

        let agent_entries_after = state
            .transcript
            .iter()
            .filter(|e| matches!(e, Entry::Agent { .. }))
            .count();
        assert_eq!(
            agent_entries_after, agent_entries_before,
            "finishing must update the existing Agent entry, not append a new one"
        );
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Agent { agent_id, status: NodeStatus::Finished, .. }) if *agent_id == child
        ));
    }

    #[test]
    fn toggle_agent_view_flips_the_flag() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.agent_view_open);
        state.toggle_agent_view();
        assert!(state.agent_view_open);
        state.toggle_agent_view();
        assert!(!state.agent_view_open);
    }

    #[test]
    fn agent_scroll_moves_within_bounds_and_clamps_at_the_ends() {
        let root = AgentId::new();
        let mut state = AppState::new(root); // starts with the root node
        let child = AgentId::new();
        state.tree.insert(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: Some("child".to_string()),
            status: NodeStatus::Running,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        // Two nodes: 0..=1.
        state.agent_scroll(1);
        assert_eq!(state.agent_selected, 1);
        state.agent_scroll(1);
        assert_eq!(state.agent_selected, 1, "clamps at the last row");
        state.agent_scroll(-1);
        assert_eq!(state.agent_selected, 0);
        state.agent_scroll(-1);
        assert_eq!(state.agent_selected, 0, "clamps at the first row");
    }

    #[test]
    fn palette_source_prefers_the_stem_over_autofilled_input() {
        let mut state = AppState::new(AgentId::new());
        // No stem yet: the source mirrors `input` (covers direct-set callers).
        state.input = "/x".to_string();
        assert_eq!(state.palette_source(), "/x");
        // The user "typed" /a; an arrow then autofills `input` to a whole
        // command. The source stays the stem so the match list does not
        // collapse to the single autofilled entry.
        state.input = "/a".to_string();
        state.sync_palette_stem();
        state.input = "/agents".to_string();
        assert_eq!(state.palette_source(), "/a");
        // Submitting clears the stem; the source falls back to `input`.
        state.clear_palette();
        assert_eq!(state.palette_selected, None);
        assert_eq!(state.palette_source(), "/agents");
    }

    // ---- transcript scrolling: auto-follow + clamp ----

    #[test]
    fn new_state_follows_the_tail_by_default() {
        let state = AppState::new(AgentId::new());
        assert!(state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn scroll_page_up_from_following_starts_at_the_bottom_and_disengages_follow() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.scroll_page_up(5, 20);

        assert_eq!(
            state.scroll, 15,
            "must step up FROM the bottom (max_scroll)"
        );
        assert!(!state.follow_tail);
    }

    #[test]
    fn scroll_page_up_clamps_at_the_top() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 3;
        state.follow_tail = false;

        state.scroll_page_up(10, 20);

        assert_eq!(state.scroll, 0, "must not go negative / wrap");
        assert!(!state.follow_tail);
    }

    #[test]
    fn scroll_page_down_clamps_at_the_bottom_and_reengages_follow() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 15;
        state.follow_tail = false;

        state.scroll_page_down(10, 20);

        assert_eq!(state.scroll, 20, "must not overscroll past max_scroll");
        assert!(
            state.follow_tail,
            "landing back on the bottom must re-engage auto-follow"
        );
    }

    #[test]
    fn scroll_page_down_short_of_the_bottom_leaves_follow_disengaged() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 0;
        state.follow_tail = false;

        state.scroll_page_down(5, 20);

        assert_eq!(state.scroll, 5);
        assert!(
            !state.follow_tail,
            "must not re-engage follow until actually back at the bottom"
        );
    }

    #[test]
    fn scroll_page_up_then_down_round_trips_back_to_the_bottom() {
        let mut state = AppState::new(AgentId::new());
        state.scroll_page_up(4, 20); // 20 -> 16, follow off
        assert_eq!(state.scroll, 16);
        assert!(!state.follow_tail);

        state.scroll_page_down(4, 20); // 16 -> 20, follow re-engages
        assert_eq!(state.scroll, 20);
        assert!(state.follow_tail);
    }

    #[test]
    fn scroll_page_down_while_already_following_is_a_pinned_noop() {
        // If content grows (raising max_scroll) between renders, PageDown
        // while still following must not push `scroll` past the NEW bottom.
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.scroll_page_down(3, 20);

        assert_eq!(state.scroll, 20);
        assert!(state.follow_tail);
    }

    // ---- T6: End/Home jump keys + the floating footer's line count ----

    #[test]
    fn jump_to_tail_reengages_follow_and_resets_scroll() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 7;

        state.jump_to_tail();

        assert!(state.follow_tail, "End must re-engage follow_tail");
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn jump_to_tail_from_already_following_is_a_noop_in_effect() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.jump_to_tail();

        assert!(state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn jump_to_top_disengages_follow_and_seats_scroll_at_zero() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.jump_to_top(20);

        assert!(
            !state.follow_tail,
            "Home must disengage follow_tail -- the user is reviewing history"
        );
        assert_eq!(
            state.scroll, 0,
            "Home must land on the transcript's own top: scroll == 0 is the \
             oldest wrapped line in this codebase's scroll direction (see \
             transcript::tests::explicit_scroll_shows_history_when_not_following)"
        );
    }

    #[test]
    fn jump_to_top_from_mid_scroll_still_lands_at_zero() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 12;

        state.jump_to_top(20);

        assert!(!state.follow_tail);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn lines_above_tail_is_zero_while_following() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);
        assert_eq!(state.lines_above_tail(20), 0);

        // Even a nonzero stale `scroll` is irrelevant while following.
        state.scroll = 5;
        assert_eq!(state.lines_above_tail(20), 0);
    }

    #[test]
    fn lines_above_tail_counts_from_the_current_scroll_to_max() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 12;

        assert_eq!(state.lines_above_tail(20), 8);
    }

    #[test]
    fn lines_above_tail_is_zero_at_the_true_bottom_even_if_follow_is_off() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = 20;

        assert_eq!(state.lines_above_tail(20), 0);
    }

    #[test]
    fn lines_above_tail_clamps_an_overscrolled_value() {
        let mut state = AppState::new(AgentId::new());
        state.follow_tail = false;
        state.scroll = u16::MAX;

        assert_eq!(
            state.lines_above_tail(20),
            0,
            "an overscrolled `scroll` must clamp, not underflow/wrap"
        );
    }

    // ---- WI-140: focused-agent switch ----

    #[test]
    fn new_state_focuses_the_root_by_default() {
        let root = AgentId::new();
        let state = AppState::new(root);
        assert_eq!(state.focused_agent, root);
        assert!(state.is_root_focused());
        assert_eq!(state.root_agent(), root);
    }

    #[test]
    fn focus_agent_switches_the_focused_agent_and_clears_the_transcript() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "root said hi".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        // Scrolled up, reviewing history -- must not leak into the new
        // agent's view.
        state.follow_tail = false;
        state.scroll = 7;

        state.focus_agent(child);

        assert_eq!(state.focused_agent, child);
        assert!(!state.is_root_focused());
        assert!(
            state.transcript.is_empty(),
            "switching focus must clear the previous agent's transcript"
        );
        assert!(
            state.follow_tail,
            "a freshly focused agent's view must start following its own tail"
        );
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn focus_agent_back_to_root_restores_root_focus() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.focus_agent(child);
        assert!(!state.is_root_focused());

        state.focus_agent(root);

        assert!(state.is_root_focused());
        assert_eq!(state.focused_agent, root);
    }

    #[test]
    fn root_agent_falls_back_to_focused_agent_if_tree_root_is_somehow_none() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.tree.root = None; // should-never-happen guard path
        assert_eq!(state.root_agent(), state.focused_agent);
    }

    // ---- Review fix (SIGNIFICANT): prompting a finished focused agent
    // must never happen -- `is_focused_agent_live`/
    // `block_message_if_focused_agent_finished` are what `app.rs::submit`
    // guards on before ever calling `prompt_agent`. ----

    #[test]
    fn is_focused_agent_live_is_true_for_every_non_terminal_status() {
        let root = AgentId::new();
        let child = AgentId::new();
        for status in [
            NodeStatus::Starting,
            NodeStatus::Running,
            NodeStatus::AwaitingPermission,
        ] {
            let mut state = AppState::new(root);
            state.tree.insert(TreeNode {
                agent_id: child,
                parent: Some(root),
                agent_def: None,
                status,
                kind: None,
                inherited_upto: None,
                ephemeral: false,
            });
            state.focus_agent(child);
            assert!(
                state.is_focused_agent_live(),
                "status {status:?} must be treated as live"
            );
        }
    }

    #[test]
    fn is_focused_agent_live_is_false_for_every_terminal_status() {
        let root = AgentId::new();
        let child = AgentId::new();
        for status in [
            NodeStatus::Finished,
            NodeStatus::Failed,
            NodeStatus::Cancelled,
        ] {
            let mut state = AppState::new(root);
            state.tree.insert(TreeNode {
                agent_id: child,
                parent: Some(root),
                agent_def: None,
                status,
                kind: None,
                inherited_upto: None,
                ephemeral: false,
            });
            state.focus_agent(child);
            assert!(
                !state.is_focused_agent_live(),
                "status {status:?} must be treated as terminal (not live)"
            );
        }
    }

    #[test]
    fn ensure_agent_tracked_seeds_a_running_node_under_the_given_parent() {
        // The panel-population fix: a bare /spawn or /fork child must appear
        // in the tree immediately, since its own `AgentSpawned` never reaches
        // the stream the app switches to.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();

        state.ensure_agent_tracked(child, root);

        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("the child must have been seeded into the tree");
        assert_eq!(node.parent, Some(root));
        assert_eq!(node.status, NodeStatus::Running);
    }

    #[test]
    fn ensure_agent_tracked_is_idempotent_and_never_downgrades_an_existing_node() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        // A real `AgentSpawned` already put the child in the tree, and it has
        // since finished. A late seed must not resurrect it as `Running`.
        state.apply(&envelope(
            SessionId::new(),
            child,
            Event::AgentSpawned {
                kind: SubagentMode::Spawn,
                parent: Some(root),
                agent_def: None,
                inherited_upto: None,
                ephemeral: false,
            },
        ));
        state.apply(&envelope(
            SessionId::new(),
            child,
            Event::AgentFinished {
                result: AgentResult::new(child, SessionId::new(), ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));
        let before = state.tree.nodes.len();

        state.ensure_agent_tracked(child, root);

        assert_eq!(
            state.tree.nodes.len(),
            before,
            "seeding an already-tracked agent must not add a duplicate node"
        );
        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .unwrap();
        assert_eq!(
            node.status,
            NodeStatus::Finished,
            "an existing finished node must not be downgraded to Running"
        );
    }

    #[test]
    fn ensure_agent_tracked_attaches_under_root_when_the_parent_is_unknown() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        let unknown_parent = AgentId::new();

        state.ensure_agent_tracked(child, unknown_parent);

        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("the child must still be seeded");
        assert_eq!(
            node.parent,
            Some(root),
            "an unknown parent falls back to attaching under the tree root"
        );
    }

    #[test]
    fn is_focused_agent_live_fails_open_when_the_focused_agent_has_no_tree_node_yet() {
        // A brand-new bare-spawn/-fork child can be focused before its own
        // `AgentSpawned` has reached `apply` (see the method's own doc) --
        // this must never be mistaken for "finished".
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let not_yet_tracked = AgentId::new();
        state.focused_agent = not_yet_tracked;
        assert!(state.is_focused_agent_live());
    }

    #[test]
    fn block_message_if_focused_agent_finished_pushes_a_notice_and_returns_true_when_finished() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.tree.insert(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Finished,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.focus_agent(child);
        assert_eq!(state.activity, Activity::Idle);

        let blocked = state.block_message_if_focused_agent_finished();

        assert!(blocked);
        assert!(
            matches!(
                state.transcript.last(),
                Some(Entry::Notice { text }) if text.contains(&child.to_string()) && text.contains("finished")
            ),
            "expected a Notice naming the finished agent, got {:?}",
            state.transcript.last()
        );
        // Fix 2's whole point: `activity` must be left exactly as it was
        // (never bumped to `Thinking`) since nothing was actually prompted.
        assert_eq!(state.activity, Activity::Idle);
    }

    #[test]
    fn block_message_if_focused_agent_finished_is_a_noop_when_live() {
        let root = AgentId::new();
        let mut state = AppState::new(root); // root starts `Starting` -- live
        let before = state.transcript.len();

        let blocked = state.block_message_if_focused_agent_finished();

        assert!(!blocked);
        assert_eq!(
            state.transcript.len(),
            before,
            "a live focused agent must not get a Notice pushed"
        );
    }

    #[test]
    fn a_finished_focused_agents_status_line_stays_idle_after_a_blocked_message_render_check() {
        // Review fix (SIGNIFICANT): drives the render harness, not just
        // `AppState` fields directly -- proves the REAL status line
        // (`view::status`) shows `idle`, never `thinking`, and the
        // transcript pane shows the blocking Notice, after the exact
        // sequence `app.rs::submit` performs for a finished focused agent.
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.tree.insert(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Finished,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        state.focus_agent(child);

        let blocked = state.block_message_if_focused_agent_finished();
        assert!(blocked);

        let rendered = crate::tui::test_support::render_text(&state, 100, 24);
        assert!(
            rendered.contains("finished"),
            "the blocking Notice must be visible in the rendered transcript: {rendered}"
        );
        assert!(
            rendered.contains("idle"),
            "the status line must still read idle, never thinking: {rendered}"
        );
        assert!(
            !rendered.contains("thinking"),
            "activity must not have been bumped to thinking for a blocked message: {rendered}"
        );
    }

    // ---- Cycle-3 review fix: an unrelated agent's lifecycle event must not
    // pollute a focused non-root agent's transcript. Lifecycle events bypass
    // the stream's session/agent filter (panel-fix), so `apply` receives the
    // whole tree's spawns/finishes even while focused on one child. ----

    #[test]
    fn a_sibling_spawn_does_not_pollute_a_focused_childs_transcript() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let focused = AgentId::new();
        let sibling = AgentId::new();

        // Both children exist under root; focus the first one.
        state.apply(&envelope(session, focused, spawned(Some(root))));
        state.focus_agent(focused);
        assert!(state.transcript.is_empty());

        // A sibling (also a child of root, NOT of the focused agent) spawns.
        state.apply(&envelope(session, sibling, spawned(Some(root))));

        assert!(
            state.transcript.is_empty(),
            "a sibling's spawn must not appear in the focused child's transcript"
        );
        // The tree model is still updated regardless of focus (panel needs it).
        assert!(state.tree.nodes.iter().any(|n| n.agent_id == sibling));
    }

    #[test]
    fn root_finishing_does_not_pollute_a_focused_childs_transcript() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let focused = AgentId::new();

        state.apply(&envelope(session, focused, spawned(Some(root))));
        state.focus_agent(focused);
        assert!(state.transcript.is_empty());

        // The root session ends while a child is focused.
        state.apply(&envelope(
            session,
            root,
            Event::AgentFinished {
                result: AgentResult::new(
                    root,
                    session,
                    ResultStatus::BudgetExceeded {
                        limit: "max_steps=3".to_string(),
                    },
                    "",
                ),
                ephemeral: false,
            },
        ));

        assert!(
            state.transcript.is_empty(),
            "a root 'session ended' notice must not be injected into a focused child's view"
        );
    }

    #[test]
    fn the_focused_agents_own_child_still_shows_inline() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let focused = AgentId::new();
        let grandchild = AgentId::new();

        state.apply(&envelope(session, focused, spawned(Some(root))));
        state.focus_agent(focused);
        assert!(state.transcript.is_empty());

        // A DIRECT child of the focused agent spawns -- this IS part of the
        // focused agent's own conversation and must show inline.
        state.apply(&envelope(session, grandchild, spawned(Some(focused))));

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Agent { agent_id, .. }) if *agent_id == grandchild
        ));
    }

    // ---- Review fix (finding 1, CRITICAL): a focus-switch replay must
    // show BOTH sides of the conversation, not just tool/lifecycle
    // activity. This is the regression guard: it feeds `apply` the exact
    // envelope shape `record_to_event`'s replay batch now produces for a
    // `UserTurn` followed by an `Assistant` record (`Event::UserTurn{text,
    // prov}` then `TextDelta{..}`, per that function's own mapping -- see
    // this item's own doc for why `UserTurn` is no longer a stringly-typed
    // `AgentProgress` fallback) and asserts both land as real, visible
    // transcript content. ----

    #[test]
    fn agent_progress_pushes_a_visible_notice() {
        // A genuine free-text `AgentProgress` (e.g. a `SystemNote`/
        // `ContextReportRecord` replay, or a live runtime-authored note) --
        // NOT a user turn, which now has its own typed `Event::UserTurn`
        // variant and its own arm/tests below.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::AgentProgress {
                note: "repeated step detected".to_string(),
            },
        ));

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "repeated step detected"
        ));
    }

    #[test]
    fn user_turn_event_pushes_entry_user_not_a_notice() {
        // This item's acceptance test: a consumer (here, the TUI's own
        // `apply`) can identify a user turn from the typed `Event::UserTurn`
        // variant alone -- no `"user turn: "` string-matching -- and it
        // renders as a real `Entry::User`, not `Entry::Notice`.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::UserTurn {
                text: "hi".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
        ));

        assert!(
            matches!(state.transcript.last(), Some(Entry::User(text)) if text == "hi"),
            "expected exactly one Entry::User(\"hi\"), got {:?}",
            state.transcript
        );
    }

    #[test]
    fn a_single_user_turn_event_appears_in_the_transcript_exactly_once() {
        // The regression the local-push removal (`app.rs`'s `submit`/
        // `deliver_first_message`) risks: not zero, not twice.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::UserTurn {
                text: "only once".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
        ));

        let user_entries: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::User(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            user_entries,
            vec!["only once"],
            "the prompt must appear exactly once, got {:?}",
            state.transcript
        );
    }

    #[test]
    fn replayed_user_turn_and_assistant_reply_both_render_in_the_transcript() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        // Exactly the envelope sequence `record_to_event` now synthesizes
        // for one `UserTurn` record followed by one `Assistant` record on
        // replay (`SessionHandle::agent_events`/`events_from`'s replay
        // batch): `Event::UserTurn{text, prov}`, then `TextDelta{text}`
        // carrying the assistant's full reply.
        state.apply(&envelope(
            session,
            agent,
            Event::UserTurn {
                text: "hi".to_string(),
                prov: conway::Provenance::UserPrompt,
            },
        ));
        state.apply(&envelope(
            session,
            agent,
            Event::TextDelta {
                text: "hello there".to_string(),
            },
        ));

        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::User(text) if text == "hi"
            )),
            "the replayed user prompt must render as a real Entry::User, not be dropped or \
             turned into a Notice: {:?}",
            state.transcript
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Assistant { text, .. } if text == "hello there"
            )),
            "the replayed assistant reply must render as a real Entry::Assistant, not be \
             dropped: {:?}",
            state.transcript
        );
    }

    #[test]
    fn a_notice_between_two_replayed_assistant_turns_keeps_them_as_separate_entries() {
        // The consecutive-turns concern from the review: since each
        // replayed user turn now pushes a non-`Assistant` `Entry::User`
        // first, `append_assistant_text`'s existing "start fresh unless the
        // last entry is already an Assistant" check keeps two different
        // assistant replies from coalescing into one bubble.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        for (prompt, reply) in [("first", "reply one"), ("second", "reply two")] {
            state.apply(&envelope(
                session,
                agent,
                Event::UserTurn {
                    text: prompt.to_string(),
                    prov: conway::Provenance::UserPrompt,
                },
            ));
            state.apply(&envelope(
                session,
                agent,
                Event::TextDelta {
                    text: reply.to_string(),
                },
            ));
        }

        let assistant_texts: Vec<&str> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Assistant { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            assistant_texts,
            vec!["reply one", "reply two"],
            "two separate replayed assistant turns must stay as two separate entries"
        );
    }

    // ---- Board item 01KYAGP11FF9YC3G60TWHHKKST: the live activity
    // indicator's `apply` transitions, all scoped to the focused agent. ----

    #[test]
    fn new_state_starts_idle_with_zero_usage() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, Usage::default());
    }

    #[test]
    fn thinking_delta_sets_activity_thinking() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::ThinkingDelta {
                text: "hmm".to_string(),
            },
        ));

        assert_eq!(state.activity, Activity::Thinking);
    }

    #[test]
    fn text_delta_sets_activity_responding() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::TextDelta {
                text: "hi".to_string(),
            },
        ));

        assert_eq!(state.activity, Activity::Responding);
    }

    #[test]
    fn tool_call_proposed_sets_activity_running_tool_captured_from_proposed() {
        // The tool name must come from `ToolCallProposed`, not
        // `ToolCallStarted` -- the latter carries only a `call_id`, per this
        // item's own binding note.
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({}),
            },
        ));

        assert_eq!(state.activity, Activity::RunningTool("bash".to_string()));
    }

    #[test]
    fn permission_requested_sets_activity_awaiting_permission() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::PermissionRequested {
                call_id: "tc_1".to_string(),
                rendered: "bash: ls".to_string(),
            },
        ));

        assert_eq!(state.activity, Activity::AwaitingPermission);
    }

    #[test]
    fn turn_finished_resets_activity_to_idle_and_live_increments_usage() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.activity = Activity::Responding;

        let usage = Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        };
        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage,
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, usage);

        // A second turn accumulates on top of the first (live-increment,
        // not overwrite).
        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage,
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));
        assert_eq!(state.focused_agent_usage, usage + usage);
    }

    #[test]
    fn agent_finished_resets_activity_to_idle_only_for_the_focused_agent() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let sibling = AgentId::new();
        state.apply(&envelope(session, sibling, spawned(Some(root))));
        state.activity = Activity::Responding;

        // The SIBLING finishing (not the focused root) must not touch
        // `activity`.
        state.apply(&envelope(
            session,
            sibling,
            Event::AgentFinished {
                result: AgentResult::new(sibling, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));
        assert_eq!(
            state.activity,
            Activity::Responding,
            "an unrelated agent's finish must not reset the focused agent's activity"
        );

        // The focused (root) agent finishing DOES reset it.
        state.apply(&envelope(
            session,
            root,
            Event::AgentFinished {
                result: AgentResult::new(root, session, ResultStatus::Completed, "done"),
                ephemeral: false,
            },
        ));
        assert_eq!(state.activity, Activity::Idle);
    }

    /// Bug 2 fix (01KYAN9EQ5BRZQ0V3DCW590YCZ): `TurnStarted` for the FOCUSED
    /// agent, BEFORE any delta arrives, must already mark the activity
    /// indicator as working -- this is the whole point of the fix (the
    /// submit->model-latency window used to show `Idle` with no `TurnStarted`
    /// arm at all).
    #[test]
    fn turn_started_sets_activity_thinking_for_the_focused_agent() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        assert_eq!(state.activity, Activity::Idle);

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));

        assert_eq!(state.activity, Activity::Thinking);
    }

    /// Companion case: a `TurnStarted` for a NON-focused agent (sibling/other
    /// subtree) must not mislabel the focused agent as working.
    #[test]
    fn turn_started_for_a_non_focused_agent_leaves_activity_unchanged() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let other = AgentId::new();
        assert_eq!(state.activity, Activity::Idle);

        state.apply(&envelope(session, other, Event::TurnStarted { turn: 1 }));

        assert_eq!(
            state.activity,
            Activity::Idle,
            "an unrelated agent's TurnStarted must not touch the focused agent's activity"
        );
    }

    /// The render-level companion (board item 01KYAN75MXECX9M2WJWF4KRYM9's
    /// harness): `TurnStarted` fed BEFORE any delta must already show a
    /// working phrase on screen, not "idle" -- this is the actual bug report
    /// ("I only saw ready and awaiting permission"), reproduced through the
    /// real `view::draw` render pass rather than only asserting on the
    /// `Activity` enum directly.
    #[test]
    fn turn_started_renders_a_working_status_before_any_delta_arrives() {
        use crate::tui::test_support::render_text;

        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));

        let screen = render_text(&state, 80, 24).to_lowercase();
        assert!(
            screen.contains("thinking") || screen.contains("working"),
            "expected a working/thinking status phrase right after TurnStarted, got:\n{screen}"
        );
        assert!(
            !screen.contains("idle"),
            "must not still show idle right after TurnStarted, got:\n{screen}"
        );
    }

    #[test]
    fn focus_agent_resets_activity_and_usage() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.activity = Activity::Thinking;
        state.focused_agent_usage = Usage {
            input_tokens: 42,
            ..Usage::default()
        };

        state.focus_agent(AgentId::new());

        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, Usage::default());
    }

    // ---- /agents surface rework item A1: ephemeral `/ask`-style forks
    // ENTER the tree like any other agent (provenance stays attached;
    // draw-time visibility filtering is item A2, NOT here) carrying their
    // spawn metadata (`kind`/`inherited_upto`/`ephemeral`) on the
    // `TreeNode`. The ONLY thing an ephemeral spawn suppresses is the
    // inline `Entry::Agent` transcript entry (the B5 single-turn modal --
    // `Mode::AskModal` -- is its UI surface). `AgentTreeView` itself stays
    // unfiltered. ----

    #[test]
    fn ephemeral_spawn_appears_in_appstate_tree_with_metadata() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        let before = state.transcript.len();

        state.apply(&envelope(
            session,
            child,
            Event::AgentSpawned {
                kind: SubagentMode::Fork,
                parent: Some(root),
                agent_def: None,
                inherited_upto: Some(LogSeq(7)),
                ephemeral: true,
            },
        ));

        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("an ephemeral spawn must insert a tree node");
        assert!(
            node.ephemeral,
            "the ephemeral flag must land on the TreeNode"
        );
        assert_eq!(
            node.kind,
            Some(SubagentMode::Fork),
            "the spawn kind must land on the TreeNode"
        );
        assert_eq!(
            node.inherited_upto,
            Some(LogSeq(7)),
            "the fork provenance (inherited_upto) must land on the TreeNode"
        );
        assert_eq!(node.parent, Some(root));
        assert_eq!(node.status, NodeStatus::Running);
        assert_eq!(
            state.transcript.len(),
            before,
            "an ephemeral spawn must not push any transcript entry"
        );
        assert!(
            !state
                .transcript
                .iter()
                .any(|e| matches!(e, Entry::Agent { agent_id, .. } if *agent_id == child)),
            "an ephemeral spawn must not push an Entry::Agent for the child"
        );
    }

    #[test]
    fn non_ephemeral_spawn_still_appears_in_appstate_tree() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        // `parent == focused_agent (root)` so the spawn is a direct child of
        // the focused view and pushes an inline `Entry::Agent` (the existing
        // behavior the ephemeral gating must preserve for `ephemeral: false`).
        assert_eq!(state.focused_agent, root);

        state.apply(&envelope(
            session,
            child,
            Event::AgentSpawned {
                kind: SubagentMode::Fork,
                parent: Some(root),
                agent_def: None,
                inherited_upto: None,
                ephemeral: false,
            },
        ));

        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("a non-ephemeral spawn must still insert a tree node");
        assert!(!node.ephemeral);
        assert_eq!(node.kind, Some(SubagentMode::Fork));
        assert!(
            matches!(
                state.transcript.last(),
                Some(Entry::Agent { agent_id, status: NodeStatus::Running, .. })
                    if *agent_id == child
            ),
            "a non-ephemeral direct child of the focused agent must still push an Entry::Agent"
        );
    }

    #[test]
    fn ephemeral_finished_updates_tree_status_without_resetting_focused_activity() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        // The focused agent is the parent (`root`): the finish's
        // `if env.agent == self.focused_agent` guard does NOT fire (it's the
        // child finishing, not the parent), so the focused activity must
        // survive untouched -- but `apply_agent_finished` DOES run now, so
        // the ephemeral child's tree node advances to `Finished`.
        state.focused_agent = root;
        state.activity = Activity::Responding;
        state.tree.insert(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Running,
            kind: Some(SubagentMode::Fork),
            inherited_upto: None,
            ephemeral: true,
        });
        let transcript_before = state.transcript.clone();

        state.apply(&envelope(
            session,
            child,
            Event::AgentFinished {
                result: AgentResult::new(child, session, ResultStatus::Completed, "x"),
                ephemeral: true,
            },
        ));

        assert_eq!(
            state.activity,
            Activity::Responding,
            "an ephemeral child's finish must not touch the focused (parent) agent's activity"
        );
        assert_eq!(
            state.transcript, transcript_before,
            "an ephemeral finish must not push any transcript entry"
        );
        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("the pre-seeded child node must still be present");
        assert_eq!(
            node.status,
            NodeStatus::Finished,
            "an ephemeral finish must update the child's tree node status (the event is no longer dropped)"
        );
    }

    #[test]
    fn ephemeral_spawn_event_does_not_affect_other_tree_nodes() {
        // `AppState::tree` is unfiltered at the data-structure level: a
        // node `insert`ed directly (mimicking what the runtime `tree()`
        // snapshot would carry) must survive an unrelated ephemeral
        // spawn event flowing through `apply`.
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let ephemeral_in_tree = AgentId::new();
        let other = AgentId::new();
        state.tree.insert(TreeNode {
            agent_id: ephemeral_in_tree,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Running,
            kind: Some(SubagentMode::Fork),
            inherited_upto: None,
            ephemeral: true,
        });

        state.apply(&envelope(
            session,
            other,
            Event::AgentSpawned {
                kind: SubagentMode::Fork,
                parent: Some(root),
                agent_def: None,
                inherited_upto: None,
                ephemeral: true,
            },
        ));

        assert!(
            state
                .tree
                .nodes
                .iter()
                .any(|n| n.agent_id == ephemeral_in_tree),
            "a directly-seeded ephemeral node must stay"
        );
        let other_node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == other)
            .expect("the ephemeral spawn event itself must now insert a node for `other`");
        assert!(other_node.ephemeral);
    }

    // ---- /agents surface rework item A2: draw-time visibility filter.
    // The filter is a pure function over (tree node, mode); the tree itself is
    // NEVER filtered -- provenance is never destroyed, so finished agents are
    // hidden, not removed. Selection indexes the filtered rows and is
    // re-clamped when the filter changes. ----

    fn tracked_node(state: &mut AppState, parent: AgentId, status: NodeStatus) -> AgentId {
        let id = AgentId::new();
        state.tree.insert(TreeNode {
            agent_id: id,
            parent: Some(parent),
            agent_def: None,
            status,
            kind: None,
            inherited_upto: None,
            ephemeral: false,
        });
        id
    }

    #[test]
    fn agent_visibility_defaults_to_all_and_cycles_in_order() {
        // V5: the default flipped from ActiveOnly to All (a finished agent
        // must not vanish from the panel the instant it finishes). The
        // cycle ORDER itself is unchanged -- only the starting point moved.
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.agent_visibility, AgentVisibility::All);
        state.cycle_agent_visibility();
        assert_eq!(state.agent_visibility, AgentVisibility::FinishedOnly);
        state.cycle_agent_visibility();
        assert_eq!(state.agent_visibility, AgentVisibility::ActiveOnly);
        state.cycle_agent_visibility();
        assert_eq!(
            state.agent_visibility,
            AgentVisibility::All,
            "the cycle must wrap back to All"
        );
    }

    /// V5 acceptance: the panel's default listing is stable -- a finished
    /// agent stays represented under the default filter rather than
    /// vanishing the instant it finishes.
    #[test]
    fn default_visibility_still_shows_a_finished_agent() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        assert_eq!(state.agent_visibility, AgentVisibility::All);
        let finished = tracked_node(&mut state, root, NodeStatus::Finished);

        assert!(
            state.visible_agent_nodes().any(|n| n.agent_id == finished),
            "a Finished node must still be represented under the default filter"
        );
    }

    #[test]
    fn active_only_hides_every_terminal_status_and_keeps_every_live_one() {
        let root = AgentId::new();
        let mut state = AppState::new(root); // root itself is Starting (live)
        let finished = tracked_node(&mut state, root, NodeStatus::Finished);
        let failed = tracked_node(&mut state, root, NodeStatus::Failed);
        let cancelled = tracked_node(&mut state, root, NodeStatus::Cancelled);
        let running = tracked_node(&mut state, root, NodeStatus::Running);
        let starting = tracked_node(&mut state, root, NodeStatus::Starting);
        let awaiting = tracked_node(&mut state, root, NodeStatus::AwaitingPermission);

        state.agent_visibility = AgentVisibility::ActiveOnly;
        let visible: Vec<AgentId> = state.visible_agent_nodes().map(|n| n.agent_id).collect();

        for terminal in [finished, failed, cancelled] {
            assert!(
                !visible.contains(&terminal),
                "ActiveOnly must hide the terminal node {terminal}"
            );
        }
        for live in [root, running, starting, awaiting] {
            assert!(
                visible.contains(&live),
                "ActiveOnly must keep the live node {live}"
            );
        }
    }

    #[test]
    fn finished_only_shows_only_terminal_nodes() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let finished = tracked_node(&mut state, root, NodeStatus::Finished);
        let failed = tracked_node(&mut state, root, NodeStatus::Failed);
        let running = tracked_node(&mut state, root, NodeStatus::Running);
        state.agent_visibility = AgentVisibility::FinishedOnly;

        let visible: Vec<AgentId> = state.visible_agent_nodes().map(|n| n.agent_id).collect();

        assert!(visible.contains(&finished));
        assert!(visible.contains(&failed));
        assert!(
            !visible.contains(&running),
            "FinishedOnly must hide Running nodes"
        );
        assert!(
            !visible.contains(&root),
            "FinishedOnly must hide the Starting root"
        );
    }

    #[test]
    fn visibility_all_shows_every_node() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        for status in [
            NodeStatus::Finished,
            NodeStatus::Failed,
            NodeStatus::Cancelled,
            NodeStatus::Running,
        ] {
            tracked_node(&mut state, root, status);
        }
        state.agent_visibility = AgentVisibility::All;

        assert_eq!(
            state.visible_agent_nodes().count(),
            state.tree.nodes.len(),
            "All must show exactly the whole tree"
        );
    }

    #[test]
    fn filtering_never_changes_the_tree_itself() {
        // Finished agents are HIDDEN, not removed -- provenance survives --
        // cycling through every mode must leave `tree.nodes` exactly as it was.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        tracked_node(&mut state, root, NodeStatus::Finished);
        tracked_node(&mut state, root, NodeStatus::Running);
        let count_before = state.tree.nodes.len();
        let nodes_before = state.tree.nodes.clone();

        state.cycle_agent_visibility(); // -> FinishedOnly
        state.cycle_agent_visibility(); // -> ActiveOnly
        state.cycle_agent_visibility(); // -> All

        assert_eq!(state.tree.nodes.len(), count_before);
        assert_eq!(
            state.tree.nodes, nodes_before,
            "the filter must be draw-time only; the tree itself never changes"
        );
    }

    #[test]
    fn agent_scroll_uses_the_filtered_row_count() {
        // root(Starting), done(Finished), live(Running). Under ActiveOnly
        // the visible rows are [root, live]: selection must clamp at index
        // 1 even though the raw tree has 3 nodes.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        tracked_node(&mut state, root, NodeStatus::Finished);
        tracked_node(&mut state, root, NodeStatus::Running);
        state.agent_visibility = AgentVisibility::ActiveOnly;

        state.agent_scroll(1);
        assert_eq!(state.agent_selected, 1);
        state.agent_scroll(1);
        assert_eq!(
            state.agent_selected, 1,
            "selection must clamp at the last FILTERED row, not the raw tree's"
        );

        // Under All (3 rows) the same key reaches further.
        state.agent_visibility = AgentVisibility::All;
        state.agent_scroll(1);
        assert_eq!(state.agent_selected, 2);
    }

    #[test]
    fn cycling_the_filter_reclamps_a_selection_past_the_new_row_count() {
        // root(Starting), a(Finished), b(Finished). Under All: 3 rows,
        // select the last (b). Cycling to FinishedOnly leaves 2 rows:
        // selection must re-clamp from 2 to 1.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        tracked_node(&mut state, root, NodeStatus::Finished);
        tracked_node(&mut state, root, NodeStatus::Finished);
        state.agent_visibility = AgentVisibility::All;
        state.agent_selected = 2;

        state.cycle_agent_visibility(); // All -> FinishedOnly

        assert_eq!(state.agent_visibility, AgentVisibility::FinishedOnly);
        assert_eq!(
            state.agent_selected, 1,
            "a selection past the new filtered row count must re-clamp"
        );

        // Cycling again to ActiveOnly leaves only the root: clamp to 0.
        state.cycle_agent_visibility();
        assert_eq!(state.agent_visibility, AgentVisibility::ActiveOnly);
        assert_eq!(state.agent_selected, 0);
    }

    #[test]
    fn cycling_to_a_filter_with_no_visible_rows_resets_the_selection_to_zero() {
        // Everything terminal: FinishedOnly shows rows, ActiveOnly shows
        // none -- the selection must land on 0, not dangle.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        tracked_node(&mut state, root, NodeStatus::Finished);
        // Make the root itself terminal too, so ActiveOnly hides all.
        state.tree.nodes[0].status = NodeStatus::Finished;
        state.agent_visibility = AgentVisibility::All;
        state.agent_selected = 1;

        state.cycle_agent_visibility(); // All -> FinishedOnly (2 rows, still in range)
        assert_eq!(state.agent_selected, 1);

        state.cycle_agent_visibility(); // FinishedOnly -> ActiveOnly (0 rows)
        assert_eq!(state.agent_selected, 0);
    }

    /// B3: `AgentPromoted` is the ONLY signal for the ephemeral→persistent
    /// flip (no optimistic TUI-side flip) — applying it flips the cached
    /// `TreeNode.ephemeral` to false.
    #[test]
    fn agent_promoted_flips_the_cached_tree_node_flag() {
        let session = SessionId::new();
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);

        // An ephemeral `/ask`-style child enters the tree via its spawn.
        state.apply(&envelope(
            session,
            child,
            Event::AgentSpawned {
                kind: SubagentMode::Fork,
                parent: Some(root),
                agent_def: None,
                inherited_upto: None,
                ephemeral: true,
            },
        ));
        assert!(
            state
                .tree
                .nodes
                .iter()
                .find(|n| n.agent_id == child)
                .expect("child node")
                .ephemeral,
            "precondition: the child node is cached as ephemeral"
        );

        state.apply(&envelope(session, child, Event::AgentPromoted {}));

        assert!(
            !state
                .tree
                .nodes
                .iter()
                .find(|n| n.agent_id == child)
                .expect("child node")
                .ephemeral,
            "AgentPromoted must flip the cached node's ephemeral flag to false"
        );
    }

    /// B3, never-panic contract: an `AgentPromoted` for an agent the tree
    /// does not know degrades to a `Notice` (same contract the
    /// unknown-parent `AgentSpawned` arm honors) — never a panic, never a
    /// silent drop.
    #[test]
    fn agent_promoted_for_an_unknown_agent_degrades_to_a_notice() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let before = state.transcript.len();

        state.apply(&envelope(session, AgentId::new(), Event::AgentPromoted {}));

        assert_eq!(state.transcript.len(), before + 1);
        assert!(
            matches!(state.transcript.last(), Some(Entry::Notice { .. })),
            "an unknown-agent AgentPromoted must degrade to a Notice, got: {:?}",
            state.transcript.last()
        );
    }

    // ---- B5: the /ask single-turn modal -- offer/park/close/fail. The
    // fate dispatch itself (which facade op each fate invokes) is covered
    // in `commands.rs`'s `apply_ask_fate` tests; these cover the modal's
    // own mode bookkeeping. ----

    fn ask_modal(question: &str) -> AskModal {
        AskModal {
            question: question.to_string(),
            child: AgentId::new(),
            answer: "the answer".to_string(),
            error: None,
        }
    }

    fn permission_prompt(rendered: &str) -> crate::tui::gate::PendingPrompt {
        let (prompt, _rx) =
            crate::tui::gate::PendingPrompt::new_for_test(conway::PermissionRequest {
                agent_id: AgentId::new(),
                agent_path: Vec::new(),
                tool: ToolName::new("bash"),
                category: conway::ToolCategory::Execute,
                arguments: serde_json::json!({}),
                rendered: rendered.to_string(),
                call_id: "tc_1".to_string(),
                render_kind: conway::RenderKind::ShellCommand,
            });
        prompt
    }

    #[test]
    fn offer_ask_modal_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_ask_modal(ask_modal("q"));

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "q" && m.error.is_none()),
            "the modal must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_ask_modal_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_ask_modal(ask_modal("q"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the modal parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "q"),
            "the parked modal must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_ask_modal_drains_a_parked_modal_and_clears_the_slot() {
        // M1's quit-path fix: `purge_open_ask_modal` must be able to reach a
        // modal that was parked behind a permission prompt when the ask
        // completed, or its child leaks as residue. `take_pending_ask_modal`
        // is the accessor that lets app.rs drain it without `pending_ask_modal`
        // being pub.
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_ask_modal(ask_modal("parked"));

        let drained = state.take_pending_ask_modal();
        assert!(
            matches!(&drained, Some(m) if m.question == "parked"),
            "the parked modal must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_ask_modal().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn take_pending_ask_modal_is_none_when_nothing_is_parked() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.take_pending_ask_modal().is_none());
        // An open (live) modal is in `mode`, not in the parking slot.
        state.offer_ask_modal(ask_modal("live"));
        assert!(
            state.take_pending_ask_modal().is_none(),
            "a live modal is not parked -- take must return None"
        );
    }

    #[test]
    fn close_ask_modal_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));

        state.close_ask_modal();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_ask_modal_promotes_a_prompt_queued_while_the_modal_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        // A permission request arrives WHILE the modal owns the floor --
        // `offer_prompt` queues it, exactly as behind another prompt.
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.close_ask_modal();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the modal must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn fail_ask_modal_keeps_the_modal_open_with_the_error_shown() {
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));

        state.fail_ask_modal("pull_in refused".to_string());

        match &state.mode {
            Mode::AskModal(m) => {
                assert_eq!(m.error.as_deref(), Some("pull_in refused"));
                assert_eq!(m.question, "q", "the modal's content is untouched");
            }
            other => panic!("a failed fate must KEEP the modal open, got: {other:?}"),
        }
    }

    // ---- C2: the NL intent confirmation card -- offer/park/close, and the
    // three-way key routing (Confirm/Edit/Manual) lives in `input.rs`'s
    // tests; the facade dispatch (which SlashCommand each choice rebuilds)
    // is covered in `commands.rs`'s `execute_intent_confirm` tests. These
    // cover the card's own mode bookkeeping and the parking priority. ----

    fn intent_card(prompt: &str) -> IntentConfirm {
        IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: None,
                prompt: prompt.to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: prompt.to_string(),
            parent: AgentId::new(),
        }
    }

    #[test]
    fn offer_intent_confirm_opens_immediately_in_normal_mode() {
        let mut state = AppState::new(AgentId::new());
        assert!(matches!(state.mode, Mode::Normal));

        state.offer_intent_confirm(intent_card("refactor the parser"));

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "refactor the parser"),
            "the card must open immediately, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_intent_confirm_parks_behind_a_permission_prompt_and_opens_once_it_resolves() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::AwaitingPermission(_)));

        state.offer_intent_confirm(intent_card("parked"));

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "the permission prompt must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.resolve_current_prompt(conway::PermissionDecision::AllowOnce);

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "parked"),
            "the parked card must open once the prompt queue drains, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn offer_intent_confirm_parks_behind_an_ask_modal_and_opens_once_it_closes() {
        // The three modal-bearing surfaces never stack: an intent card
        // arriving while an /ask modal owns the floor parks in
        // `pending_intent_confirm`, and `close_ask_modal` promotes it via
        // `promote_next_surface`.
        let mut state = AppState::new(AgentId::new());
        state.offer_ask_modal(ask_modal("q"));
        assert!(matches!(state.mode, Mode::AskModal(_)));

        state.offer_intent_confirm(intent_card("parked-behind-ask"));

        assert!(
            matches!(state.mode, Mode::AskModal(_)),
            "the ask modal must keep the floor; the card parks, got: {:?}",
            state.mode
        );

        state.close_ask_modal();

        assert!(
            matches!(&state.mode, Mode::IntentConfirm(ic) if ic.intent.prompt == "parked-behind-ask"),
            "the parked card must open once the ask modal closes, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn take_pending_intent_confirm_drains_a_parked_card_and_clears_the_slot() {
        let mut state = AppState::new(AgentId::new());
        state.offer_prompt(permission_prompt("bash: ls"));
        state.offer_intent_confirm(intent_card("parked"));

        let drained = state.take_pending_intent_confirm();
        assert!(
            matches!(&drained, Some(ic) if ic.intent.prompt == "parked"),
            "the parked card must be returned, got: {drained:?}"
        );
        assert!(
            state.take_pending_intent_confirm().is_none(),
            "the slot must be cleared after the take"
        );
        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "take must NOT clobber the surface currently owning `mode`"
        );
    }

    #[test]
    fn close_intent_confirm_returns_to_normal() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));

        state.close_intent_confirm();

        assert!(matches!(state.mode, Mode::Normal));
    }

    #[test]
    fn close_intent_confirm_promotes_a_prompt_queued_while_the_card_was_open() {
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        state.offer_prompt(permission_prompt("bash: ls"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.close_intent_confirm();

        assert!(
            matches!(state.mode, Mode::AwaitingPermission(_)),
            "closing the card must promote the queued prompt, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn close_intent_confirm_promotes_a_parked_ask_modal() {
        // Priority order: queued prompts, then parked ask modal, then
        // parked intent card. With no queued prompt, closing the card
        // promotes a parked ask modal.
        let mut state = AppState::new(AgentId::new());
        state.offer_intent_confirm(intent_card("q"));
        // Park an ask behind the card (offer_ask_modal parks when mode !=
        // Normal).
        state.offer_ask_modal(ask_modal("parked-ask"));
        assert!(matches!(state.mode, Mode::IntentConfirm(_)));

        state.close_intent_confirm();

        assert!(
            matches!(&state.mode, Mode::AskModal(m) if m.question == "parked-ask"),
            "closing the card must promote the parked ask modal, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn begin_intent_confirm_edit_drops_the_classified_prompt_into_the_input_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "stale text".to_string();
        state.cursor = state.input.chars().count();
        state.offer_intent_confirm(IntentConfirm {
            intent: AgentIntent {
                recipe: SubagentMode::Spawn,
                agent_def: Some("reviewer".to_string()),
                prompt: "review the diff carefully".to_string(),
            },
            default_recipe: SubagentMode::Spawn,
            raw_text: "review the diff".to_string(),
            parent: AgentId::new(),
        });

        state.begin_intent_confirm_edit();

        assert_eq!(
            state.input, "review the diff carefully",
            "the classified prompt (not the raw text) must land in the input line"
        );
        assert_eq!(
            state.cursor,
            state.input.chars().count(),
            "the cursor must be at the end of the dropped prompt"
        );
        assert!(
            matches!(state.mode, Mode::Normal),
            "the card must close after edit, got: {:?}",
            state.mode
        );
    }

    #[test]
    fn begin_intent_confirm_edit_is_a_noop_when_no_card_is_open() {
        let mut state = AppState::new(AgentId::new());
        state.input = "keep me".to_string();

        state.begin_intent_confirm_edit();

        assert_eq!(
            state.input, "keep me",
            "a no-card edit must not touch the input line"
        );
        assert!(matches!(state.mode, Mode::Normal));
    }

    // ---- T2: activity spinner + animation tick ----

    #[test]
    fn spinner_frame_cycles_the_braille_sequence_and_wraps() {
        // Advance one full cycle plus one: the frame index must wrap back to
        // 1 (frame 0 is the starting position, so a full `len` ticks lands
        // back on 0, and one more lands on 1).
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.spinner_frame, 0);
        let n = SPINNER_FRAMES.len();
        for _ in 0..n {
            state.tick_animation();
        }
        assert_eq!(state.spinner_frame, 0, "frame must wrap modulo {}", n);
        state.tick_animation();
        assert_eq!(state.spinner_frame, 1);
        // The glyph lookup itself never panics on any in-range frame.
        let glyph = SPINNER_FRAMES[state.spinner_frame % SPINNER_FRAMES.len()];
        assert!(SPINNER_FRAMES.contains(&glyph));
    }

    #[test]
    fn should_animate_is_false_for_idle_true_otherwise() {
        assert!(!should_animate(&Activity::Idle));
        assert!(should_animate(&Activity::Thinking));
        assert!(should_animate(&Activity::Responding));
        assert!(should_animate(&Activity::RunningTool("bash".to_string())));
        assert!(should_animate(&Activity::AwaitingPermission));
    }

    #[test]
    fn turn_started_records_the_start_instant_and_resets_running_tokens() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.turn_running_tokens = 999;

        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));

        assert!(
            state.turn_started_at.is_some(),
            "TurnStarted for the focused agent must stamp the start instant"
        );
        assert_eq!(
            state.turn_running_tokens, 0,
            "a new turn must reset the new-segment-token count"
        );
    }

    #[test]
    fn turn_started_for_a_non_focused_agent_does_not_stamp_or_reset() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let other = AgentId::new();

        state.apply(&envelope(session, other, Event::TurnStarted { turn: 1 }));

        assert!(
            state.turn_started_at.is_none(),
            "a non-focused agent's TurnStarted must not stamp the focused agent's clock"
        );
    }

    #[test]
    fn context_segment_added_accumulates_running_tokens_for_the_focused_agent() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        // A turn must be in flight for the accumulator to engage.
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        assert_eq!(state.turn_running_tokens, 0);

        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 120,
            },
        ));
        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 200,
            },
        ));

        assert_eq!(state.turn_running_tokens, 320);
    }

    #[test]
    fn context_segment_added_outside_a_turn_does_not_accumulate() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        // No TurnStarted yet -- `turn_started_at` is None.
        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 120,
            },
        ));
        assert_eq!(state.turn_running_tokens, 0);
    }

    #[test]
    fn turn_finished_clears_the_turn_state() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);
        state.apply(&envelope(session, agent, Event::TurnStarted { turn: 1 }));
        state.apply(&envelope(
            session,
            agent,
            Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 50,
            },
        ));
        assert!(state.turn_started_at.is_some());
        assert_eq!(state.turn_running_tokens, 50);

        state.apply(&envelope(
            session,
            agent,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        assert!(state.turn_started_at.is_none());
        assert_eq!(state.turn_running_tokens, 0);
        assert_eq!(state.activity, Activity::Idle);
    }

    #[test]
    fn focus_agent_resets_the_spinner_and_turn_state() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.spinner_frame = 5;
        state.turn_started_at = Some(Instant::now());
        state.turn_running_tokens = 42;
        state.activity = Activity::Responding;

        state.focus_agent(child);

        assert_eq!(state.spinner_frame, 0);
        assert!(state.turn_started_at.is_none());
        assert_eq!(state.turn_running_tokens, 0);
        assert_eq!(state.activity, Activity::Idle);
    }

    // ---- T3: ModelDecision -> focused_model/max_context, cumulative ctx
    // tokens, and the focus-switch reset of all three. ----

    fn model_decision_env(agent: AgentId, chosen: &str) -> Envelope {
        Envelope {
            seq: 0,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent,
            event: Event::ModelDecision {
                role: conway::RoleAlias::new("coder"),
                chosen: chosen.parse().expect("valid ModelRef"),
                reason: conway::RoutingReason::PinnedByApi,
                attempt: 0,
            },
        }
    }

    fn context_segment_env(agent: AgentId, tokens_est: u32) -> Envelope {
        Envelope {
            seq: 0,
            ts: chrono::Utc::now(),
            session: SessionId::new(),
            agent,
            event: Event::ContextSegmentAdded {
                segment: conway_core::ids::SegmentId::new(),
                provenance: conway::Provenance::UserPrompt,
                tokens_est,
            },
        }
    }

    #[test]
    fn model_decision_sets_focused_model_and_max_context() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state
            .model_max_context
            .insert("anthropic/claude-sonnet-4-6".to_string(), 200_000);

        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));

        assert_eq!(
            state.focused_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(state.focused_model_max_context, Some(200_000));
    }

    #[test]
    fn model_decision_with_unknown_model_leaves_max_context_none() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        // Metadata has a different model; the chosen one is unknown.
        state
            .model_max_context
            .insert("anthropic/claude-haiku-4-5".to_string(), 32_768);

        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));

        assert_eq!(
            state.focused_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert!(
            state.focused_model_max_context.is_none(),
            "unknown model -> no max context (renderer falls back to raw tokens)"
        );
    }

    #[test]
    fn model_decision_for_non_focused_agent_does_not_touch_focused_model() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state.focus_agent(child);
        state
            .model_max_context
            .insert("anthropic/claude-sonnet-4-6".to_string(), 200_000);

        // A ModelDecision on the root (not focused) must not overwrite the
        // focused child's model fields.
        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));

        assert!(
            state.focused_model.is_none(),
            "non-focused ModelDecision must not set focused_model"
        );
    }

    #[test]
    fn context_segment_added_accumulates_cumulative_ctx_tokens_across_turns() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        // Turn 1.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::TurnStarted { turn: 1 },
        ));
        state.apply(&context_segment_env(root, 1_000));
        state.apply(&context_segment_env(root, 500));
        // Turn 1 ends.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));
        assert_eq!(state.turn_running_tokens, 0, "per-turn counter resets");
        assert_eq!(
            state.focused_ctx_tokens, 1_500,
            "cumulative counter persists across turns"
        );

        // Turn 2 -- only genuinely new segments fire.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::TurnStarted { turn: 2 },
        ));
        state.apply(&context_segment_env(root, 200));
        assert_eq!(state.turn_running_tokens, 200, "per-turn counter restarts");
        assert_eq!(
            state.focused_ctx_tokens, 1_700,
            "cumulative counter keeps growing across turns"
        );
    }

    #[test]
    fn context_segment_added_dedups_cumulative_ctx_tokens_by_segment_id() {
        // T3 code-review fix 1: a non-keep-alive focused agent's second
        // run re-emits `ContextSegmentAdded` for EVERY existing context
        // segment (its fresh `AgentLoop`'s local `seen_segments` is
        // empty). The renderer must dedup by `SegmentId` so the
        // cumulative `focused_ctx_tokens` counts each segment ONCE per
        // focused session, not once per run.
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let segment = conway_core::ids::SegmentId::new();

        // First emission of `segment` -- counted.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 1_000,
            },
        ));
        assert_eq!(state.focused_ctx_tokens, 1_000);

        // Re-emit the SAME segment id (simulating the second run of a
        // non-keep-alive agent re-emitting its existing context). The
        // cumulative figure must NOT double-count it.
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 1_000,
            },
        ));
        assert_eq!(
            state.focused_ctx_tokens, 1_000,
            "re-emitted segment id must not double-count into focused_ctx_tokens"
        );

        // A DISTINCT segment id is genuinely new -- counted.
        let other = conway_core::ids::SegmentId::new();
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment: other,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 250,
            },
        ));
        assert_eq!(
            state.focused_ctx_tokens, 1_250,
            "a distinct segment id is counted alongside the deduped one"
        );
    }

    #[test]
    fn focus_agent_resets_focused_seen_segments() {
        // T3 code-review fix 1: `focused_seen_segments` is per focused
        // session -- a freshly focused agent starts with an empty
        // seen-set, so a segment id seen under the PREVIOUS focus is
        // correctly counted again under the new focus (it is a different
        // session's dedup window).
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        let segment = conway_core::ids::SegmentId::new();
        state.apply(&envelope(
            SessionId::new(),
            root,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 800,
            },
        ));
        assert_eq!(state.focused_ctx_tokens, 800);
        assert!(
            state.focused_seen_segments.contains(&segment),
            "segment id recorded for the root focus"
        );

        state.focus_agent(child);
        assert!(
            state.focused_seen_segments.is_empty(),
            "focus switch clears the seen-set"
        );
        assert_eq!(state.focused_ctx_tokens, 0);

        // The same segment id, re-emitted under the new focus, counts
        // again -- it is new to THIS focused session's dedup window.
        state.apply(&envelope(
            SessionId::new(),
            child,
            Event::ContextSegmentAdded {
                segment,
                provenance: conway::Provenance::UserPrompt,
                tokens_est: 800,
            },
        ));
        assert_eq!(
            state.focused_ctx_tokens, 800,
            "segment id counts again under the new focused session"
        );
    }

    #[test]
    fn focus_agent_resets_focused_model_and_ctx_tokens() {
        let root = AgentId::new();
        let child = AgentId::new();
        let mut state = AppState::new(root);
        state
            .model_max_context
            .insert("anthropic/claude-sonnet-4-6".to_string(), 200_000);
        state.apply(&model_decision_env(root, "anthropic/claude-sonnet-4-6"));
        state.focused_ctx_tokens = 5_000;

        state.focus_agent(child);

        assert!(
            state.focused_model.is_none(),
            "focus switch resets focused_model"
        );
        assert!(
            state.focused_model_max_context.is_none(),
            "focus switch resets focused_model_max_context"
        );
        assert_eq!(
            state.focused_ctx_tokens, 0,
            "focus switch resets focused_ctx_tokens"
        );
    }

    // ---- T5: Ctrl-E toggle_all_tool_entries_expanded ----

    fn tool_entry(call_id: &str, preview: &str, expanded: bool) -> Entry {
        Entry::Tool {
            call_id: call_id.to_string(),
            name: "bash".to_string(),
            status: ToolStatus::Finished { is_error: false },
            preview: preview.to_string(),
            args: String::new(),
            progress: String::new(),
            expanded,
            ts: None,
        }
    }

    #[test]
    fn toggle_flips_expanded_on_every_tool_entry() {
        let mut state = AppState::new(AgentId::new());
        // Three tool entries: two collapsed, one already expanded. Plus a
        // non-tool entry to confirm the toggle only touches `Entry::Tool`.
        state.transcript.push(Entry::Assistant {
            text: "hi".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        state.transcript.push(tool_entry("c1", "out1\nout2", false));
        state.transcript.push(tool_entry("c2", "x\ny\nz", false));
        state.transcript.push(tool_entry("c3", "p", true));

        state.toggle_all_tool_entries_expanded();

        let expanded_flags: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(expanded_flags, vec![true, true, false]);
        // The assistant entry is untouched (still an Assistant, not a Tool).
        assert!(matches!(state.transcript[0], Entry::Assistant { .. }));
    }

    #[test]
    fn toggle_is_an_involution_round_trips_back_to_the_original_state() {
        let mut state = AppState::new(AgentId::new());
        state.transcript.push(tool_entry("c1", "out1\nout2", false));
        state.transcript.push(tool_entry("c2", "x\ny\nz", true));

        state.toggle_all_tool_entries_expanded();
        let after_first: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(after_first, vec![true, false]);

        state.toggle_all_tool_entries_expanded();
        let after_second: Vec<bool> = state
            .transcript
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { expanded, .. } => Some(*expanded),
                _ => None,
            })
            .collect();
        assert_eq!(after_second, vec![false, true]);
    }

    /// The no-snap contract: toggling `expanded` must NOT touch `scroll` or
    /// `follow_tail`. The next render's clamp (`state.scroll.min(max)`)
    /// re-clamps to the nearest valid position without jumping the viewport.
    #[test]
    fn toggle_does_not_touch_scroll_or_follow_tail() {
        let mut state = AppState::new(AgentId::new());
        state
            .transcript
            .push(tool_entry("c1", "a\nb\nc\nd\ne", false));
        state.scroll = 7;
        state.follow_tail = false;

        state.toggle_all_tool_entries_expanded();

        assert_eq!(
            state.scroll, 7,
            "toggle must not change `scroll` -- the render clamp re-clamps"
        );
        assert!(!state.follow_tail, "toggle must not change `follow_tail`");
    }

    /// T5 default: a freshly-constructed `Entry::Tool` (via `apply`'s
    /// `ToolCallProposed` arm) starts collapsed.
    #[test]
    fn new_tool_entry_from_apply_starts_collapsed() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { expanded, .. }) => assert!(
                !*expanded,
                "a freshly-proposed tool entry must start collapsed"
            ),
            other => panic!("expected a Tool entry, got {other:?}"),
        }
    }

    /// T5 config default: `AppState::new` defaults `tool_preview_lines` to
    /// 3 (the documented default).
    #[test]
    fn new_state_defaults_tool_preview_lines_to_3() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.tool_preview_lines, 3);
    }

    // ---- T5: `clamp_tool_preview_lines` never panics on bad input ----

    #[test]
    fn clamp_none_falls_back_to_default_3() {
        assert_eq!(clamp_tool_preview_lines(None), 3);
    }

    #[test]
    fn clamp_in_range_value_is_kept() {
        assert_eq!(clamp_tool_preview_lines(Some(1)), 1);
        assert_eq!(clamp_tool_preview_lines(Some(3)), 3);
        assert_eq!(clamp_tool_preview_lines(Some(50)), 50);
        assert_eq!(clamp_tool_preview_lines(Some(200)), 200);
    }

    #[test]
    fn clamp_zero_falls_back_to_default() {
        assert_eq!(clamp_tool_preview_lines(Some(0)), 3);
    }

    #[test]
    fn clamp_above_max_falls_back_to_default() {
        assert_eq!(clamp_tool_preview_lines(Some(201)), 3);
        assert_eq!(clamp_tool_preview_lines(Some(u32::MAX)), 3);
    }

    // ---- T4: transcript provenance ----

    /// `ThinkingDelta` creates an `Entry::Reasoning` on the first delta and
    /// appends to it on subsequent deltas (mirroring `TextDelta` ->
    /// `Entry::Assistant`). The entry is stored EXPANDED-by-default --
    /// `show_reasoning` defaults `true`; `build_lines` is the gate that
    /// hides it when the flag is off, not the apply path.
    #[test]
    fn thinking_delta_creates_and_appends_reasoning_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());

        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "think".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "ing".to_string(),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Reasoning { text, model, .. }) => {
                assert_eq!(text, "thinking", "deltas coalesce");
                assert_eq!(
                    model.as_deref(),
                    Some("anthropic/claude-sonnet-4-6"),
                    "model stamped from focused_model"
                );
            }
            other => panic!("expected a Reasoning entry, got {other:?}"),
        }
        assert!(
            state.show_reasoning,
            "show_reasoning defaults true (EXPANDED by default)"
        );
    }

    /// `ToolProgress` notes append to the matching in-flight `Entry::Tool`
    /// by `call_id` (previously dropped by the wildcard arm).
    #[test]
    fn tool_progress_appends_to_matching_tool_entry() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls"}),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolProgress {
                call_id: "tc_1".to_string(),
                note: "step 1".to_string(),
            },
        ));
        state.apply(&envelope(
            session,
            root,
            Event::ToolProgress {
                call_id: "tc_1".to_string(),
                note: "step 2".to_string(),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { progress, .. }) => {
                assert_eq!(progress, "step 1\nstep 2", "notes joined with newline");
            }
            other => panic!("expected a Tool entry, got {other:?}"),
        }
    }

    /// `ToolProgress` for an unknown `call_id` is a no-op (never panics on
    /// an id it has no record of).
    #[test]
    fn tool_progress_for_unknown_call_id_is_a_noop() {
        let mut state = AppState::new(AgentId::new());
        state.append_tool_progress("nope", "note");
        assert!(state.transcript.is_empty());
    }

    /// `ToolCallProposed` stores the `args` as a compact JSON string
    /// (previously discarded).
    #[test]
    fn tool_call_proposed_stores_args() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({"command": "ls", "path": "/tmp"}),
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { args, .. }) => {
                assert!(
                    args.contains("\"command\":\"ls\""),
                    "args stored compact: {args}"
                );
                assert!(
                    args.contains("\"path\":\"/tmp\""),
                    "args stored compact: {args}"
                );
            }
            other => panic!("expected a Tool entry, got {other:?}"),
        }
    }

    /// `TurnFinished` stamps a turn-end summary onto the last
    /// `Entry::Assistant` (or `Entry::Reasoning` if that was the last
    /// block). The summary reads `turn_started_at` BEFORE
    /// `clear_turn_state` zeroes it.
    #[test]
    fn turn_finished_stamps_summary_on_last_assistant() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.apply(&envelope(
            session,
            root,
            Event::TextDelta {
                text: "hello".to_string(),
            },
        ));
        state.turn_started_at = Some(Instant::now() - std::time::Duration::from_secs(66));

        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 300,
                    cache_read_tokens: 800,
                    cache_write_tokens: 100,
                    reasoning_tokens: 0,
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Assistant { summary, .. }) => {
                let s = summary.as_ref().expect("summary stamped");
                assert!(s.contains("1m 6s"), "elapsed in m/s form: {s}");
                assert!(s.contains("tok"), "token count present: {s}");
                assert!(s.contains("% cached"), "cache pct present: {s}");
            }
            other => panic!("expected an Assistant entry, got {other:?}"),
        }
    }

    /// When the last block is `Entry::Reasoning`, the summary attaches to
    /// IT instead (the trace is what the user sees last in that case).
    #[test]
    fn turn_finished_stamps_summary_on_last_reasoning_when_last() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.apply(&envelope(
            session,
            root,
            Event::ThinkingDelta {
                text: "pondering".to_string(),
            },
        ));
        state.turn_started_at = Some(Instant::now() - std::time::Duration::from_secs(5));

        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Reasoning { summary, .. }) => {
                let s = summary.as_ref().expect("summary stamped");
                assert!(s.contains("5s"), "elapsed in seconds form: {s}");
                // No cache read -> no "(n% cached)" suffix.
                assert!(
                    !s.contains("cached"),
                    "no cache pct when no cache read: {s}"
                );
            }
            other => panic!("expected a Reasoning entry, got {other:?}"),
        }
    }

    /// A turn with no assistant/reasoning block (only tool calls) gets no
    /// summary -- the summary is genuinely about a model-emitted block.
    #[test]
    fn turn_finished_with_no_assistant_or_reasoning_attaches_no_summary() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({}),
            },
        ));
        state.turn_started_at = Some(Instant::now());

        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage::default(),
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        match state.transcript.last() {
            Some(Entry::Tool { .. }) => {}
            other => panic!("expected a Tool entry unchanged, got {other:?}"),
        }
    }

    /// T4 review regression (significant): a tool-only turn must NOT reach
    /// back past its own entries and re-stamp the PREVIOUS turn's settled
    /// assistant bubble with this turn's elapsed/token figures.
    ///
    /// The companion test above only covers a transcript with no prior
    /// Assistant entry at all, so it cannot catch the walk-into-the-previous
    /// -turn path. Here turn 1 produces a real reply that gets a correct
    /// summary; turn 2 is a tool-only agentic round. Turn 2's `TurnFinished`
    /// must be a no-op, leaving turn 1's summary exactly as it was.
    #[test]
    fn tool_only_turn_does_not_restamp_the_previous_turns_summary() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);

        // --- Turn 1: a real model reply, correctly summarized. ---
        state.apply(&envelope(session, root, Event::TurnStarted { turn: 1 }));
        state.apply(&envelope(
            session,
            root,
            Event::TextDelta {
                text: "hi".to_string(),
            },
        ));
        state.turn_started_at = Some(Instant::now());
        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 20,
                    ..Usage::default()
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        let turn_one_summary = state
            .transcript
            .iter()
            .find_map(|e| match e {
                Entry::Assistant { summary, .. } => summary.clone(),
                _ => None,
            })
            .expect("turn 1 must stamp a summary on its assistant block");

        // --- Turn 2: a tool-only round (no model text of its own). ---
        state.apply(&envelope(session, root, Event::TurnStarted { turn: 2 }));
        state.apply(&envelope(
            session,
            root,
            Event::ToolCallProposed {
                call_id: "tc_1".to_string(),
                tool: ToolName::new("bash"),
                args: serde_json::json!({}),
            },
        ));
        state.turn_started_at = Some(Instant::now());
        state.apply(&envelope(
            session,
            root,
            Event::TurnFinished {
                usage: Usage {
                    input_tokens: 9_000,
                    output_tokens: 900,
                    ..Usage::default()
                },
                stop: conway_core::content::StopReason::EndTurn,
            },
        ));

        let after = state
            .transcript
            .iter()
            .find_map(|e| match e {
                Entry::Assistant { summary, .. } => summary.clone(),
                _ => None,
            })
            .expect("turn 1's assistant block must still exist");

        assert_eq!(
            after, turn_one_summary,
            "a tool-only turn must not overwrite the previous turn's summary \
             (turn 2's 9.9k-token figures leaked onto turn 1's 120-token reply)"
        );
    }

    /// `toggle_thinking` flips `show_reasoning` and returns the new value.
    #[test]
    fn toggle_thinking_flips_show_reasoning() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.show_reasoning, "defaults true");
        assert!(!state.toggle_thinking(), "toggles to false");
        assert!(state.toggle_thinking(), "toggles back to true");
    }

    /// `toggle_timestamps` flips `show_timestamps` (default false) and
    /// returns the new value.
    #[test]
    fn toggle_timestamps_flips_show_timestamps() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.show_timestamps, "defaults false");
        assert!(state.toggle_timestamps(), "toggles to true");
        assert!(!state.toggle_timestamps(), "toggles back to false");
    }

    // ---- V4: the `/settings` menu's own AppState surface ----

    #[test]
    fn open_settings_and_help_are_mutually_exclusive() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();
        assert!(state.help_open);

        state.open_settings();
        assert!(state.settings_open, "/settings must open");
        assert!(!state.help_open, "opening settings must close help");

        state.open_help();
        assert!(state.help_open, "/help must open");
        assert!(!state.settings_open, "opening help must close settings");
    }

    #[test]
    fn close_settings_is_a_noop_when_already_closed() {
        let mut state = AppState::new(AgentId::new());
        assert!(!state.settings_open);
        state.close_settings();
        assert!(!state.settings_open);
    }

    #[test]
    fn adjust_tool_preview_lines_steps_by_delta() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.tool_preview_lines, 3, "the built-in default");
        assert_eq!(state.adjust_tool_preview_lines(1), 4);
        assert_eq!(state.adjust_tool_preview_lines(1), 5);
        assert_eq!(state.adjust_tool_preview_lines(-2), 3);
    }

    /// Stepping below the floor stops AT the floor -- it must not
    /// bounce up to `clamp_tool_preview_lines`'s config-validation fallback
    /// (3), which would read as broken for an interactive stepper.
    #[test]
    fn adjust_tool_preview_lines_floors_at_one_without_bouncing_to_the_default() {
        let mut state = AppState::new(AgentId::new());
        state.tool_preview_lines = 1;

        assert_eq!(
            state.adjust_tool_preview_lines(-1),
            1,
            "must stop at the floor"
        );
        assert_eq!(
            state.adjust_tool_preview_lines(-1000),
            1,
            "a huge negative step must still land on the floor, not panic or wrap"
        );
    }

    #[test]
    fn adjust_tool_preview_lines_caps_at_two_hundred() {
        let mut state = AppState::new(AgentId::new());
        state.tool_preview_lines = 200;

        assert_eq!(
            state.adjust_tool_preview_lines(1),
            200,
            "must stop at the cap"
        );
        assert_eq!(
            state.adjust_tool_preview_lines(1_000_000),
            200,
            "a huge positive step must still land on the cap, not panic or wrap"
        );
    }

    #[test]
    fn adjust_tool_preview_lines_never_panics_at_either_i32_extreme() {
        let mut state = AppState::new(AgentId::new());
        assert_eq!(state.adjust_tool_preview_lines(i32::MIN), 1);
        assert_eq!(state.adjust_tool_preview_lines(i32::MAX), 200);
    }

    /// `format_turn_summary` formats elapsed >= 60s as `1m 6s` and < 60s
    /// as `{n}s`; cache pct only when `cache_read > 0` and the denominator
    /// is non-zero.
    #[test]
    fn format_turn_summary_shapes() {
        let with_cache = Usage {
            input_tokens: 100,
            output_tokens: 400,
            cache_read_tokens: 800,
            cache_write_tokens: 100,
            reasoning_tokens: 0,
        };
        // 800 / (100+800+100) = 80%.
        assert_eq!(
            format_turn_summary(66, &with_cache),
            "1m 6s · 1.4k tok (80% cached)"
        );
        assert_eq!(
            format_turn_summary(5, &with_cache),
            "5s · 1.4k tok (80% cached)"
        );

        let no_cache = Usage {
            input_tokens: 100,
            output_tokens: 400,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
        };
        assert_eq!(format_turn_summary(5, &no_cache), "5s · 500 tok");
    }

    /// `compact_tokens` mirrors the status line's helper: `<1000` as-is,
    /// `>=1000` as `{k}.{tenths}k`.
    #[test]
    fn compact_tokens_formats() {
        assert_eq!(compact_tokens(0), "0");
        assert_eq!(compact_tokens(999), "999");
        assert_eq!(compact_tokens(1000), "1.0k");
        assert_eq!(compact_tokens(12345), "12.3k");
        assert_eq!(compact_tokens(1400), "1.4k");
    }

    // ---- T8: input history (push, circular cap, Up/Down recall, draft
    // preservation) ----

    #[test]
    fn new_state_defaults_history_cap_to_500() {
        let state = AppState::new(AgentId::new());
        assert_eq!(state.history_cap, DEFAULT_HISTORY_SIZE);
        assert!(state.history.is_empty());
    }

    #[test]
    fn push_history_appends_newest_at_the_back() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("first".to_string());
        state.push_history("second".to_string());
        assert_eq!(
            state.history,
            std::collections::VecDeque::from(vec!["first".to_string(), "second".to_string()])
        );
    }

    #[test]
    fn push_history_evicts_the_oldest_once_the_cap_is_exceeded() {
        let mut state = AppState::new(AgentId::new());
        state.history_cap = 2;
        state.push_history("a".to_string());
        state.push_history("b".to_string());
        state.push_history("c".to_string());
        assert_eq!(
            state.history,
            std::collections::VecDeque::from(vec!["b".to_string(), "c".to_string()]),
            "the oldest entry ('a') must be evicted once the 2-entry cap is exceeded"
        );
    }

    #[test]
    fn push_history_with_a_zero_cap_keeps_no_history() {
        let mut state = AppState::new(AgentId::new());
        state.history_cap = 0;
        state.push_history("a".to_string());
        assert!(state.history.is_empty());
    }

    #[test]
    fn history_recall_prev_on_empty_history_does_not_fire() {
        let mut state = AppState::new(AgentId::new());
        state.input = "typing".to_string();
        assert!(!state.history_recall_prev());
        assert_eq!(
            state.input, "typing",
            "an empty history must not touch input"
        );
    }

    #[test]
    fn up_then_up_walks_from_newest_toward_oldest() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.push_history("newest".to_string());

        assert!(state.history_recall_prev());
        assert_eq!(state.input, "newest");
        assert!(state.history_recall_prev());
        assert_eq!(state.input, "older");
        // At the oldest entry: further `Up` still "fires" (consumes the
        // key) but stops moving.
        assert!(state.history_recall_prev());
        assert_eq!(state.input, "older");
    }

    #[test]
    fn down_after_up_walks_back_toward_the_newest() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.push_history("newest".to_string());

        state.history_recall_prev(); // -> "newest"
        state.history_recall_prev(); // -> "older"
        assert!(state.history_recall_next());
        assert_eq!(state.input, "newest");
    }

    #[test]
    fn down_past_the_newest_entry_restores_the_in_progress_draft() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("older".to_string());
        state.push_history("newest".to_string());
        state.input = "unsent draft".to_string();
        state.cursor = state.input.chars().count();

        assert!(state.history_recall_prev());
        assert_eq!(state.input, "newest");

        assert!(state.history_recall_next());
        assert_eq!(
            state.input, "unsent draft",
            "Down past the newest entry must restore the pre-recall draft"
        );
        assert_eq!(state.cursor, "unsent draft".chars().count());
    }

    #[test]
    fn history_recall_next_while_not_browsing_does_not_fire() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("only".to_string());
        state.input = "typing".to_string();

        assert!(!state.history_recall_next());
        assert_eq!(state.input, "typing");
    }

    #[test]
    fn a_recalled_prompt_is_editable_inline_before_resubmit() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("hello world".to_string());

        assert!(state.history_recall_prev());
        assert_eq!(state.input, "hello world");
        assert_eq!(state.cursor, "hello world".chars().count());

        // The recalled text is ordinary `input`/`cursor` state -- editing it
        // (simulated directly here; `input.rs`'s key handlers do the same
        // mutation for a real keypress) just works, with no special
        // "recalled" mode to escape first.
        state.input.push_str("!!!");
        state.cursor = state.input.chars().count();
        assert_eq!(state.input, "hello world!!!");
    }

    #[test]
    fn push_history_resets_browsing_state_for_the_next_recall() {
        let mut state = AppState::new(AgentId::new());
        state.push_history("first".to_string());
        state.history_recall_prev();
        assert_eq!(state.input, "first");

        // Submitting resets browsing -- the next `Up` starts fresh from the
        // newest entry again, not wherever the previous browse left off.
        state.push_history("second".to_string());
        assert!(state.history_recall_prev());
        assert_eq!(state.input, "second");
    }

    // ---- T8: `clamp_history_size` never panics on bad input ----

    #[test]
    fn clamp_history_size_none_falls_back_to_default() {
        assert_eq!(clamp_history_size(None), DEFAULT_HISTORY_SIZE);
    }

    #[test]
    fn clamp_history_size_in_range_value_is_kept() {
        assert_eq!(clamp_history_size(Some(1)), 1);
        assert_eq!(clamp_history_size(Some(500)), 500);
        assert_eq!(clamp_history_size(Some(100_000)), 100_000);
    }

    #[test]
    fn clamp_history_size_zero_falls_back_to_default() {
        assert_eq!(clamp_history_size(Some(0)), DEFAULT_HISTORY_SIZE);
    }

    #[test]
    fn clamp_history_size_above_max_falls_back_to_default() {
        assert_eq!(clamp_history_size(Some(100_001)), DEFAULT_HISTORY_SIZE);
        assert_eq!(clamp_history_size(Some(u32::MAX)), DEFAULT_HISTORY_SIZE);
    }

    // ---- T8: `cursor_line_col` (multi-line input) ----

    #[test]
    fn cursor_line_col_on_a_single_line() {
        let mut state = AppState::new(AgentId::new());
        state.input = "hello".to_string();
        state.cursor = 3;
        assert_eq!(state.cursor_line_col(), (0, 3));
    }

    #[test]
    fn cursor_line_col_after_embedded_newlines() {
        let mut state = AppState::new(AgentId::new());
        state.input = "abc\ndef\ngh".to_string();
        // Cursor at the very end (char index 10): line 2 ("gh"), column 2.
        state.cursor = state.input.chars().count();
        assert_eq!(state.cursor_line_col(), (2, 2));

        // Cursor right after the first newline (char index 4): line 1
        // ("def"), column 0.
        state.cursor = 4;
        assert_eq!(state.cursor_line_col(), (1, 0));
    }
}

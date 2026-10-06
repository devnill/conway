//! `conway.goal`: a single standing-goal sentence the operator sets with
//! the built-in `/goal` TUI command, kept in front of the model every turn
//! so a long run does not drift away from the objective it started with.
//!
//! This crate contributes no tool and no slash command of its own: `/goal`
//! is a BUILT-IN command (`conway-cli`'s `tui::commands` module), kept
//! operator-typed and un-namespaced on purpose rather than becoming
//! `/conway.goal.set` -- see that module's own doc for the split. What
//! lives here instead is the part that must stay removable: one
//! [`conway::plugin::ContextHook`] that renders the current goal as a
//! single compact segment near the end of the assembled request (only
//! while a goal is set), and one status-line contribution (`goal: <first
//! words>`). Uninstalling `conway.goal` turns `/goal` into a command that
//! says so instead of silently writing a note nothing reads back.
//!
//! **In `DEFAULT_OPINION_SET`, deliberately.** Unlike `conway.todo` (whose
//! own module doc argues the opposite case for a MODEL-authored plan),
//! this plugin is entirely operator-driven and inert until the operator
//! themselves types `/goal <text>` -- a fresh install that never uses the
//! command pays nothing: no extra segment, no status-line entry, because
//! `GoalContextHook::before_request` only ever appends a segment once a
//! goal has actually been set.
//!
//! # How the goal survives a process restart -- not a new mechanism
//!
//! Every past [`conway::LogRecord::SystemNote`] record is replayed back
//! into context on every later request by context
//! assembly itself, unconditionally -- the exact mechanism `conway.todo`'s
//! own module doc describes as "no new persistence primitive," reused here
//! unchanged. `/goal <text>` persists one such note (via the new,
//! narrowest-possible `SessionHandle::append_system_note` facade call --
//! see that method's own doc) with `reason` exactly [`NOTE_REASON`];
//! `/goal clear` persists another, with the SAME reason but an EMPTY
//! `text` -- a clear marker, not the absence of a note, so a later resume
//! does not resurrect whatever goal was set before it (see
//! `reconstruct_from_history`'s own doc for exactly how that distinction
//! is preserved). `GoalContextHook::before_request` reads that replayed
//! history directly: when this plugin's own in-memory cache has nothing
//! for the requesting agent (true on the very first request a freshly
//! started process builds for it -- exactly the shape a resume leaves
//! behind), it finds the MOST RECENT matching note in the request's own
//! history and decodes it, refilling the cache so the status-line
//! contribution sees the same value immediately after, without waiting for
//! another `/goal` call.
//!
//! # Context cost stays flat, no matter how many `/goal` changes preceded it
//!
//! Context assembly replays EVERY past [`NOTE_REASON`]-tagged system-note
//! record back as its own segment on every later request, unconditionally.
//! Left alone, a session with N `/goal` changes would carry N stale copies
//! of the goal into every request from the N-th change onward.
//! `GoalContextHook::before_request` closes that off itself, in the one
//! place it owns: before appending its own freshly rendered segment (if
//! any), it removes every segment whose
//! [`conway::Provenance::SystemNote`] reason is exactly [`NOTE_REASON`]
//! from the payload it was handed, and appends at most one -- the live
//! segment it builds from whatever `reconstruct_from_history` just
//! found. One request therefore ever carries at most ONE `conway.goal`
//! segment, appended strictly after every segment that remains, so the
//! part of a request a backend might cache never moves -- mirroring
//! `conway.todo`'s own identical claim and the test that exercises it here.
//!
//! # What this plugin does NOT do
//!
//! No auto-continuation and no token budget tied to the goal -- it is
//! purely a reminder, rendered once per turn, read by nobody but the
//! model and the operator's own status line. A goal set on a parent is
//! inherited by a forked child (it is already part of the log a fork
//! inherits); a spawned child starts clean, since nothing of the parent's
//! log is inherited by a spawn either.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use conway::plugin::{
    async_trait, ContentBlock, ContextHook, ContextHookCtx, ContextPayload, Plugin,
    PluginDescription, PluginManifest, PluginStatusContribution, PromptSegment, Provenance,
    ResultStatus, Role, Tool,
};
use conway::AgentId;

/// The install id an operator names in `[plugins].install` -- also a
/// member of `conway_cli::first_party_plugins::DEFAULT_OPINION_SET` (not
/// linkable from here: that constant lives in a binary-only crate), see
/// this crate's own module doc, "In `DEFAULT_OPINION_SET`, deliberately."
pub const PLUGIN_ID: &str = "conway.goal";

/// The [`PluginStatusContribution::key`] this plugin's status-line entry
/// is filed under (rendered on the status line as `goal: ship the thing`).
pub const STATUS_KEY: &str = "goal";

/// `SystemNote::reason` on every note this plugin reads -- both the
/// goal-setting kind (`text` holds the sentence) and the clear-marker kind
/// (`text` is empty) share this ONE reason, so `is_persisted_note` and
/// `reconstruct_from_history` can never drift onto two different strings.
/// The exact reason the built-in `/goal` command's own `SessionHandle::
/// append_system_note` call is given for both forms.
pub const NOTE_REASON: &str = "conway.goal";

/// Sane status-line/segment truncation bound, in `char`s (never bytes --
/// see `truncate_for_display`'s own doc for why).
const DISPLAY_MAX_CHARS: usize = 60;

/// Review finding 2 (board item `01M1YVVT9RYWZWAZC4YH21T3HN`): the hard
/// cap on a `/goal <text>` sentence, in `char`s (never bytes, matching
/// `DISPLAY_MAX_CHARS`'s own convention), enforced at the moment
/// `conway_cli::tui::commands::execute`'s `SlashCommand::Goal` arm would
/// otherwise persist it -- BEFORE `SessionHandle::append_system_note` ever
/// writes it, not a silent truncation afterward. Chosen generously above
/// `DISPLAY_MAX_CHARS` (a goal is meant to be read back in full by the
/// model and by `/goal`'s own bare show form, not merely glanced at on the
/// status line, which is `truncate_for_display`'s separate job): long
/// enough for a genuine one-sentence objective, short enough that it
/// cannot grow into a second system prompt re-sent, unbounded, on every
/// later request (this crate's own module doc, "Context cost stays
/// flat" -- flat COUNT, not flat SIZE, without this cap).
pub const MAX_GOAL_CHARS: usize = 300;

/// Truncates `text` to at most `DISPLAY_MAX_CHARS` characters, appending a
/// single `…` when it was cut -- operates on `char`s, not bytes, so a
/// multi-byte UTF-8 goal sentence is never sliced mid-codepoint. First
/// sanitizes (review finding 5): a literal newline/tab is collapsed to a
/// single space (a goal sentence wrapping onto a second status-line row,
/// or a tab skewing its alignment, is never a real possibility the
/// operator meant), and every OTHER control/format character
/// [`conway::is_laundered_char`] recognizes (a raw ANSI escape, a
/// bidirectional override, â¦) is replaced via
/// [`conway::sanitize_control_chars`] -- the same shared launderer the
/// permission gate and the runtime's own tool-output rendering already
/// depend on, reused here rather than a third, independently-tuned
/// control-character table. `pub`: review fix 1 (`AppState::focused_goal`,
/// `conway_cli`'s own `view/status.rs`) renders the TUI-computed,
/// focus-correct goal value through this SAME function, so the two paths
/// that ever put a goal sentence on screen (this plugin's own
/// `status_contributions`, for the single-agent/non-TUI case, and the
/// TUI's focus-aware override) can never sanitize or truncate differently.
pub fn truncate_for_display(text: &str) -> String {
    let collapsed: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    let sanitized = conway::sanitize_control_chars(&collapsed);
    let mut chars = sanitized.chars();
    let head: String = chars.by_ref().take(DISPLAY_MAX_CHARS).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// True when `segment` is a note this plugin wrote (via the built-in
/// `/goal` command's `SessionHandle::append_system_note` call) -- carries
/// [`conway::Provenance::SystemNote`] with `reason` exactly [`NOTE_REASON`].
/// Shared by `reconstruct_from_history` (which reads the most recent one)
/// and `GoalContextHook::before_request` (which removes every one of them
/// from the payload before appending its own freshly rendered segment --
/// see this crate's own module doc, "Context cost stays flat") -- one
/// predicate, so the two halves of that mechanism can never drift onto two
/// different notions of "is this one of ours."
fn is_persisted_note(segment: &PromptSegment) -> bool {
    matches!(
        &segment.provenance,
        Provenance::SystemNote { reason } if reason == NOTE_REASON
    )
}

/// The single two-step decode every reader of a `conway.goal`-reasoned
/// system note shares: given the TEXT of every matching note found, in
/// CHRONOLOGICAL order (oldest first) -- `reconstruct_from_history` below
/// (reading replayed `PromptSegment`s) and `conway_cli::tui::commands::
/// latest_goal_text` (reading raw `LogRecord`s) each extract that sequence
/// their own way, from two different record shapes, then both hand it to
/// THIS function rather than re-deriving the interpretation step a second
/// time -- finds the single MOST RECENT entry, then interprets an empty
/// string as a clear marker (`None`), not the absence of a note.
///
/// **Must find the single MOST RECENT matching note first, then decide --
/// never skip past a clear marker looking for an earlier goal.** A clear
/// marker is itself a real, persisted fact (see this crate's own module
/// doc): once one exists, it is always the right answer for every read
/// after it, until a later `/goal <text>` persists a fresh goal-setting
/// note of its own. Resolving this in two steps -- take the latest entry's
/// raw text, THEN interpret empty-vs-non-empty -- is what keeps that true;
/// filtering for non-empty text directly would instead silently skip the
/// clear marker and resurrect whatever goal preceded it, exactly the
/// resume defect this mechanism exists to avoid.
pub fn decode_latest_goal_note<'a>(
    matching_texts: impl Iterator<Item = &'a str>,
) -> Option<String> {
    let latest = matching_texts.last()?;
    if latest.is_empty() {
        None
    } else {
        Some(latest.to_string())
    }
}

/// Scans `segments` for every one `is_persisted_note` accepts, in their
/// existing (chronological) order, and decodes the result via
/// [`decode_latest_goal_note`]: `Some(text)` for a goal-setting note
/// (non-empty `text`), `None` for a clear marker (empty `text`) OR when no
/// matching note exists at all.
fn reconstruct_from_history(segments: &[PromptSegment]) -> Option<String> {
    let texts = segments
        .iter()
        .filter(|segment| is_persisted_note(segment))
        .filter_map(|segment| match segment.content.first() {
            Some(ContentBlock::Text { text }) => Some(text.as_str()),
            _ => None,
        });
    decode_latest_goal_note(texts)
}

/// This plugin's whole mutable state: the current goal per agent (`None`
/// meaning "cleared, or never set"), plus which agent's context was most
/// recently assembled -- the single value [`Plugin::status_contributions`]
/// (which carries no per-agent context of its own) reports against,
/// mirroring `conway-plugin-todo`'s identical `last_touched` shape, except
/// updated on every context build rather than only on a write: this
/// plugin has no write-triggering tool call to hook a `ToolObserver` onto,
/// so "most recently active agent" is the closest analogous signal
/// available here.
///
/// **Known, accepted gap -- worked around at the TUI layer, not here.**
/// `Plugin::status_contributions` takes no argument, so this method's
/// answer is only ever correct when exactly one agent's context is ever
/// built (the single-agent/scripting case). In a multi-agent TUI session,
/// a BACKGROUND subagent's own context build updates `last_touched` just
/// the same, which would blank or replace the FOCUSED agent's `goal`
/// status-line entry with the background one's -- board item
/// `01M1YVVT9RYWZWAZC4YH21T3HN` review finding 1. `conway_cli`'s own
/// `tui::view::status::effective_contributions` is the fix: it overrides
/// whatever THIS method reports under [`STATUS_KEY`] with a value read
/// directly from the focused agent's own transcript
/// (`AppState::focused_goal`), never from this struct. This struct and
/// this method are NOT changed to chase that fix -- adding a
/// focused-agent parameter to `Plugin::status_contributions` would widen
/// every OTHER implementor of this trait (subprocess, MCP, statusline,
/// todo) for a problem only this one plugin has, which is a materially
/// bigger change than the TUI-side override actually needed.
#[derive(Default)]
struct GoalState {
    goals: HashMap<AgentId, Option<String>>,
    last_touched: Option<AgentId>,
}

/// Shared between `GoalContextHook` and read directly by
/// [`GoalPlugin::status_contributions`] -- one `Arc<Mutex<_>>` per
/// [`GoalPlugin`] instance, the same shape `conway-plugin-todo`'s own
/// per-agent state takes.
type Shared = Arc<Mutex<GoalState>>;

/// Reads the current goal for `agent_id`: the in-memory cache when
/// present, otherwise `reconstruct_from_history` against `segments` --
/// refilling the cache with whatever it found (including `None`, a
/// genuinely resolved "no goal" answer, not merely "not looked up yet") so
/// a later read in the SAME process (the status-line contribution) sees it
/// too, with no further rescans. Always updates `GoalState::last_touched`
/// to `agent_id` -- see that field's own doc for why every context build
/// (not only a write) is this plugin's "touched" signal.
fn resolve_current(
    state: &Shared,
    agent_id: AgentId,
    segments: &[PromptSegment],
) -> Option<String> {
    let mut state = state.lock().expect("goal state lock poisoned");
    state.last_touched = Some(agent_id);
    if let Some(cached) = state.goals.get(&agent_id) {
        return cached.clone();
    }
    let reconstructed = reconstruct_from_history(segments);
    state.goals.insert(agent_id, reconstructed.clone());
    reconstructed
}

/// Removes every stale `conway.goal` note, then renders the current goal
/// (if any) as one compact segment, appended after every segment that
/// remains -- see this crate's own module doc, "Context cost stays flat",
/// for why a PERSISTED note is never left in the payload this hands
/// onward, and why the one segment this method does add is strictly an
/// append, never a mutation of anything else that was there.
struct GoalContextHook {
    state: Shared,
}

#[async_trait]
impl ContextHook for GoalContextHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        // Reads whichever persisted note matters BEFORE the filter below
        // removes it from the payload -- `resolve_current`'s own
        // `reconstruct_from_history` call needs the UNFILTERED segments to
        // find it at all.
        let goal = resolve_current(&self.state, ctx.agent_id, &payload.segments);
        let ContextPayload { segments, tools } = payload;
        // Strip every PERSISTED `conway.goal` note already in the payload
        // (one per past `/goal`/`/goal clear` call) so the segment appended
        // below is the ONLY `conway.goal` segment this request ever
        // carries, no matter how many changes preceded it. This can never
        // orphan a tool call/result pair: a persisted note's `ContentBlock`
        // is always a single `Text` block (the built-in `/goal` command
        // never writes anything else), never a `ToolUse`/`ToolResultBlock`.
        let mut segments: Vec<PromptSegment> = segments
            .into_iter()
            .filter(|segment| !is_persisted_note(segment))
            .collect();
        let Some(text) = goal else {
            return ContextPayload { segments, tools };
        };
        let segment = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: format!("Standing goal: {text}"),
            }],
            // A reason distinct from `NOTE_REASON` itself (mirroring
            // `conway-plugin-todo`'s identical precaution): this freshly
            // rendered segment must never be mistaken for (and re-filtered
            // as) one of the PERSISTED notes `is_persisted_note` strips
            // above, on any later call within the same request-building
            // pass.
            Provenance::SystemNote {
                reason: "conway.goal.segment".to_string(),
            },
        );
        segments.push(segment);
        ContextPayload { segments, tools }
    }
}

/// The plugin itself: one shared `GoalState` behind the `ContextHook` and
/// status contribution it hands out.
#[derive(Default)]
pub struct GoalPlugin {
    state: Shared,
}

impl GoalPlugin {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Plugin for GoalPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    /// Honest about why this entry sits in the opinion set unprompted --
    /// see this crate's own module doc, "In `DEFAULT_OPINION_SET`,
    /// deliberately."
    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "a one-sentence standing goal the operator sets with /goal, kept in front \
                      of the model every turn -- in the default opinion set, inert until /goal \
                      is used"
                .to_string(),
            you_get: format!(
                "the built-in `/goal` command actually stores and shows something: a compact \
                 segment near the end of context whenever a goal is set, and a `{STATUS_KEY}` \
                 status-line entry"
            ),
            you_lose: "nothing else -- with this uninstalled, /goal says so instead of \
                       silently storing a goal nothing reads"
                .to_string(),
            costs: "exactly one short extra segment near the end of every request once a goal \
                    is set -- never more, no matter how many /goal changes preceded it; one \
                    system-note record persisted per /goal change (for resume, not replayed \
                    into requests as more than that one segment)"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        Vec::new()
    }

    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        vec![Arc::new(GoalContextHook {
            state: self.state.clone(),
        })]
    }

    /// The most recently touched agent's current goal, or nothing at all
    /// before any agent's context has been built even once -- see
    /// `GoalState::last_touched`'s own doc for why this method (which
    /// carries no per-agent context of its own) reports a single value
    /// rather than one per agent.
    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        let state = self.state.lock().expect("goal state lock poisoned");
        let Some(agent) = state.last_touched else {
            return Vec::new();
        };
        let Some(Some(text)) = state.goals.get(&agent) else {
            return Vec::new();
        };
        vec![PluginStatusContribution {
            key: STATUS_KEY.to_string(),
            status: ResultStatus::Completed,
            value: truncate_for_display(text),
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::ArtifactWriteHandle;
    use conway::SessionId;

    fn hook_ctx(agent_id: AgentId) -> ContextHookCtx {
        ContextHookCtx {
            agent_id,
            agent_path: vec![agent_id],
            session_id: SessionId::new(),
            turn: 0,
            model: None,
            estimated_tokens: 100,
            artifacts: ArtifactWriteHandle::noop(agent_id),
            tag: None,
        }
    }

    fn user_prompt(text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::User,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::UserPrompt,
        )
    }

    fn persisted_note(text: &str) -> PromptSegment {
        PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: text.to_string(),
            }],
            Provenance::SystemNote {
                reason: NOTE_REASON.to_string(),
            },
        )
    }

    // ------------------------------------------------------------------
    // Sanity: manifest/description, the plugin browser's own read surface.
    // ------------------------------------------------------------------

    #[test]
    fn manifest_id_matches_the_published_constant() {
        assert_eq!(GoalPlugin::new().manifest().id, PLUGIN_ID);
    }

    #[test]
    fn manifest_names_no_tools() {
        assert!(GoalPlugin::new().manifest().tools.is_empty());
    }

    #[test]
    fn description_is_non_empty() {
        let description = GoalPlugin::new().description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
        assert!(!description.costs.is_empty());
    }

    #[test]
    fn plugin_declares_one_context_hook_and_no_tools_or_commands() {
        let plugin = GoalPlugin::new();
        assert!(plugin.tools().is_empty());
        assert_eq!(plugin.context_hooks().len(), 1);
        assert!(plugin.commands().is_empty());
    }

    // ------------------------------------------------------------------
    // The segment appears after a note is set, and disappears after a
    // clear marker -- the core acceptance criterion.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn the_segment_appears_after_a_goal_note_and_disappears_after_a_clear_marker() {
        let plugin = GoalPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];

        // A goal has been set: one persisted note with a non-empty text.
        let prefix = vec![user_prompt("hello"), persisted_note("ship the thing")];
        let payload = ContextPayload {
            segments: prefix.clone(),
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;
        assert_eq!(
            out.segments.len(),
            2,
            "the persisted note is removed and exactly one live segment is appended: {:?}",
            out.segments
        );
        let appended = out.segments.last().unwrap();
        let ContentBlock::Text { text } = &appended.content[0] else {
            panic!("expected a text block");
        };
        assert!(text.contains("ship the thing"), "{text}");
        assert!(text.starts_with("Standing goal:"), "{text}");

        // A BRAND NEW plugin instance (no in-memory cache), carrying a
        // clear marker AFTER the goal-setting note in its own history --
        // the shape `/goal clear` leaves behind.
        let plugin2 = GoalPlugin::new();
        let hook2 = &plugin2.context_hooks()[0];
        let prefix2 = vec![
            user_prompt("hello"),
            persisted_note("ship the thing"),
            persisted_note(""),
        ];
        let payload2 = ContextPayload {
            segments: prefix2,
            tools: vec![],
        };
        let out2 = hook2.before_request(&hook_ctx(agent), payload2).await;
        assert_eq!(
            out2.segments.len(),
            1,
            "both persisted notes are removed and NO live segment is appended once the latest \
             one is a clear marker: {:?}",
            out2.segments
        );
    }

    // ------------------------------------------------------------------
    // A resumed session (fresh plugin instance, reconstruct from history
    // only) still shows the goal.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn a_resumed_session_with_a_fresh_plugin_instance_still_shows_the_goal() {
        let plugin = GoalPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];
        let payload = ContextPayload {
            segments: vec![persisted_note("finish the migration")],
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;
        assert_eq!(out.segments.len(), 1);
        let ContentBlock::Text { text } = &out.segments[0].content[0] else {
            panic!("expected a text block");
        };
        assert!(text.contains("finish the migration"));

        // The status contribution (a SEPARATE read of the SAME fresh
        // instance) must agree, proving the hook backfilled the in-memory
        // cache, not merely rendered a one-off segment.
        let status = plugin.status_contributions();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].key, STATUS_KEY);
        assert_eq!(status[0].value, "finish the migration");
    }

    // ------------------------------------------------------------------
    // Clear, then resume: a fresh instance shows no goal at all.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn clear_then_resume_shows_no_goal() {
        let plugin = GoalPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];
        let payload = ContextPayload {
            segments: vec![persisted_note("old goal"), persisted_note("")],
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;
        assert!(
            out.segments.is_empty(),
            "no live segment, and no persisted note survives either: {:?}",
            out.segments
        );
        assert!(
            plugin.status_contributions().is_empty(),
            "no status contribution once the most recent note is a clear marker"
        );
    }

    // ------------------------------------------------------------------
    // The appended segment is last and only one exists after N goal
    // changes.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn exactly_one_segment_survives_after_n_goal_changes_and_it_is_last() {
        let plugin = GoalPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];
        let history: Vec<PromptSegment> = (0..5)
            .map(|n| persisted_note(&format!("revision {n}")))
            .collect();
        let payload = ContextPayload {
            segments: history,
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;

        let goal_segments: Vec<&PromptSegment> = out
            .segments
            .iter()
            .filter(|s| matches!(&s.provenance, Provenance::SystemNote { reason } if reason.contains("conway.goal")))
            .collect();
        assert_eq!(
            goal_segments.len(),
            1,
            "exactly one conway.goal segment must survive, regardless of how many persisted \
             notes preceded it: {:?}",
            out.segments
        );
        assert!(
            std::ptr::eq(goal_segments[0], out.segments.last().unwrap()),
            "the surviving segment must be the LAST one in the payload"
        );
        let ContentBlock::Text { text } = &goal_segments[0].content[0] else {
            panic!("expected a text block");
        };
        assert!(
            text.contains("revision 4"),
            "must reflect the MOST RECENT persisted note: {text}"
        );
    }

    // ------------------------------------------------------------------
    // Removing persisted notes can never orphan a tool call/result pair.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn removing_persisted_notes_leaves_every_tool_call_result_pair_intact() {
        use conway::plugin::ToolName;

        fn tool_use(call_id: &str) -> PromptSegment {
            PromptSegment::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    call_id: call_id.to_string(),
                    name: ToolName::new("read"),
                    arguments: serde_json::json!({}),
                }],
                Provenance::Assistant,
            )
        }
        fn tool_result(call_id: &str) -> PromptSegment {
            PromptSegment::new(
                Role::ToolResult,
                vec![ContentBlock::ToolResultBlock {
                    call_id: call_id.to_string(),
                    blocks: vec![ContentBlock::Text {
                        text: "contents".to_string(),
                    }],
                    is_error: false,
                }],
                Provenance::ToolResult {
                    call_id: call_id.to_string(),
                    tool: ToolName::new("read"),
                },
            )
        }

        let plugin = GoalPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];
        let payload = ContextPayload {
            segments: vec![
                persisted_note("old revision 1"),
                tool_use("a"),
                persisted_note("old revision 2"),
                tool_result("a"),
                persisted_note("old revision 3"),
            ],
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;

        let call_ids: Vec<&str> = out
            .segments
            .iter()
            .filter_map(|s| {
                s.content.iter().find_map(|b| match b {
                    ContentBlock::ToolUse { call_id, .. } => Some(call_id.as_str()),
                    _ => None,
                })
            })
            .collect();
        let result_ids: Vec<&str> = out
            .segments
            .iter()
            .filter_map(|s| {
                s.content.iter().find_map(|b| match b {
                    ContentBlock::ToolResultBlock { call_id, .. } => Some(call_id.as_str()),
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            call_ids,
            vec!["a"],
            "the tool-use segment must survive the note-removal filter untouched: {:?}",
            out.segments
        );
        assert_eq!(
            result_ids,
            vec!["a"],
            "the tool-result segment must survive the note-removal filter untouched: {:?}",
            out.segments
        );
    }

    // ------------------------------------------------------------------
    // Status-line text truncates sanely.
    // ------------------------------------------------------------------

    #[test]
    fn status_text_truncates_sanely_on_a_char_boundary() {
        let long = "x".repeat(200);
        let truncated = truncate_for_display(&long);
        assert!(truncated.chars().count() <= DISPLAY_MAX_CHARS + 1);
        assert!(truncated.ends_with('…'));

        let short = "ship it";
        assert_eq!(truncate_for_display(short), short);
    }

    // ------------------------------------------------------------------
    // Review finding 5: the status-line value is sanitized before
    // truncation -- newlines/tabs collapse to a space, and a raw control
    // character (never typed on purpose) is laundered, never shown live.
    // ------------------------------------------------------------------

    #[test]
    fn truncate_for_display_collapses_newlines_and_tabs_to_spaces() {
        let text = "ship\nthe\tthing";
        assert_eq!(truncate_for_display(text), "ship the thing");
    }

    #[test]
    fn truncate_for_display_launders_other_control_characters() {
        // `\x1b` (ESC) is a raw ANSI escape introducer -- `is_control()`
        // true, not `\n`/`\t` -- must come out as the shared placeholder,
        // never live.
        let text = "ship\x1b[31mit";
        let out = truncate_for_display(text);
        assert!(
            !out.contains('\x1b'),
            "a raw control character must never survive to the status line: {out:?}"
        );
        assert!(
            out.contains(conway::sanitize_control_chars("\x1b").as_str()),
            "must be laundered via the SAME shared placeholder \
             `conway::sanitize_control_chars` produces: {out:?}"
        );
    }

    // ------------------------------------------------------------------
    // Review finding 4: `decode_latest_goal_note` is the ONE decode both
    // `reconstruct_from_history` (above) and `conway_cli`'s own
    // `latest_goal_text` call -- exercised here directly against the
    // exact two-step contract (latest wins, empty means cleared, no
    // matching note at all is also `None`).
    // ------------------------------------------------------------------

    #[test]
    fn decode_latest_goal_note_takes_the_last_entry_and_treats_empty_as_cleared() {
        assert_eq!(
            decode_latest_goal_note(["first goal", "second goal"].into_iter()),
            Some("second goal".to_string())
        );
        assert_eq!(
            decode_latest_goal_note(["first goal", ""].into_iter()),
            None,
            "an empty latest entry is a clear marker, not a reason to fall back \
             to an earlier one"
        );
        assert_eq!(
            decode_latest_goal_note(std::iter::empty()),
            None,
            "no matching entry at all is also None"
        );
    }

    // ------------------------------------------------------------------
    // Empty history, no note at all -> no segment, no status.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn no_note_at_all_produces_no_segment_and_no_status() {
        let plugin = GoalPlugin::new();
        let hook = &plugin.context_hooks()[0];
        let prefix = vec![user_prompt("hi")];
        let payload = ContextPayload {
            segments: prefix.clone(),
            tools: vec![],
        };
        let out = hook
            .before_request(&hook_ctx(AgentId::new()), payload)
            .await;
        assert_eq!(
            out.segments, prefix,
            "no note at all means the payload is returned completely unchanged"
        );
        assert!(plugin.status_contributions().is_empty());
    }
}

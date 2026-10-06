//! `conway.todo`: a small task list the model itself writes and ticks off,
//! so a long multi-step run stays on a plan rather than improvising one turn
//! at a time, and the operator can see where it is without reading the
//! whole transcript.
//!
//! Two tools (`todo_write` replaces the whole list; `todo_read` reads it
//! back), one [`conway::plugin::ContextHook`] that renders the current list
//! as a single compact segment near the end of the assembled request (only
//! when the list is non-empty), one status-line contribution (`todo: 2/5`),
//! and one slash command (`/conway.todo.list`) that prints the same list as
//! plain text for the operator.
//!
//! **Opt-in, deliberately not a default.** This crate is installed only
//! when an operator names its id in `[plugins].install`; it is NOT a member
//! of `conway-cli`'s own opinionated first-run set. A model-authored plan is
//! a genuine opinion about how an agent should work -- some tasks are too
//! short to benefit, and an operator who never asked for a visible todo
//! list should not get a new context segment on every turn unasked (the
//! same "you install it, you chose it" posture every other first-party
//! plugin in this tier takes for its own capability).
//!
//! # How state survives a process restart -- the established mechanism
//!
//! A [`conway::plugin::Tool`] has no privileged way to append an arbitrary
//! record to a session's own log; the one channel that exists is
//! [`conway::plugin::ToolObserver::after_tool_call`], whose returned
//! [`conway::plugin::ObserverNote`]s the runtime itself turns into a real,
//! persisted system-note record -- the same mechanism an existing
//! repeated-tool-call detector already uses for an unrelated reason. This
//! crate reuses it unchanged: `TodoNoteObserver` watches for its own
//! successful `todo_write` calls and answers with one note per call, its
//! text the whole current list serialized as JSON.
//!
//! That persisted record is not a side channel this crate invented either.
//! Context assembly already turns every past system-note record back into
//! an ordinary history segment on every subsequent request -- including
//! after a process restart, since a resumed session's context is rebuilt
//! from the full on-disk log, not from anything held in memory by the
//! process that wrote it. `TodoContextHook::before_request` reads that
//! replayed history: when this plugin's own in-memory cache has nothing for
//! the requesting agent (true on the very first request a freshly started
//! process builds for it, exactly the shape a resume leaves behind), it
//! finds the MOST RECENT matching note in the request's own history,
//! decodes the list from it, and uses that as the current state -- both to
//! render this turn's segment and to refill the cache so `todo_read` and
//! the status-line contribution see the same list immediately after,
//! without waiting for another `todo_write`. No new persistence primitive
//! exists in this crate; it is entirely the established tool-observer note
//! plus the context builder's own unconditional log replay.
//!
//! # Context cost stays flat, no matter how many `todo_write` calls ran
//!
//! Every successful `todo_write` persists its own `LogRecord::SystemNote`
//! (the mechanism just above), and context assembly replays EVERY past
//! `SystemNote` back as its own segment on every later request -- that
//! replay is unconditional and belongs to context assembly generically, not
//! to this plugin. Left alone, a session with N `todo_write` calls would
//! carry N copies of (an earlier version of) the list into every request
//! from the N-th call onward, each one a little more stale than the last.
//! `TodoContextHook::before_request` closes that off itself, in the one
//! place it owns: before appending its own freshly rendered segment, it
//! removes every segment whose `Provenance::SystemNote` reason is exactly
//! [`NOTE_REASON`] (shared with `TodoNoteObserver`, the one writer, so the
//! two can never drift onto two different strings) from the payload it was
//! handed. One request therefore ever carries at most ONE `conway.todo`
//! segment -- the one this method itself appends -- regardless of how many
//! `todo_write` calls preceded it.
//!
//! This removal happens on EVERY request, including the very first one
//! built after a `todo_write` call's own note was persisted -- there is no
//! "let it through once, then start filtering" transition. A persisted note
//! is therefore never actually part of ANY request payload a backend ever
//! sees, and never part of what a backend might cache about the request
//! either: removing something that was never sent cannot invalidate a
//! cache of what was. See "The segment never churns the cached prefix"
//! below for the complementary half of that claim (the one segment this
//! method DOES add).
//!
//! # The segment never churns the cached prefix
//!
//! Past its own removal of stale `conway.todo` notes (above), this method
//! only ever APPENDS one segment, to the very end of the list, and
//! otherwise returns every remaining segment byte-for-byte unchanged, in
//! the same order -- never inserted earlier, never used to replace or edit
//! anything else that was there. A list that changes turn to turn is
//! exactly the kind of volatile content that must sit after the inherited
//! prefix and this session's own turn records, not among them, so the part
//! of the request a backend might cache never moves.
//!
//! # What this plugin does NOT do
//!
//! No dependency graph between items, no scheduling, and no persistence
//! beyond this session's own log -- a list does not follow a model into a
//! different, unrelated session. An item's `status` is one of three fixed
//! values; anything else fails the call with a plain argument error rather
//! than being coerced into one of them.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use conway::plugin::{
    async_trait, Command, CommandCtx, CommandOutcome, CommandSpec, ContentBlock, ContextHook,
    ContextHookCtx, ContextPayload, ObservedCall, ObserverAnswer, ObserverCtx, ObserverNote,
    PathArgs, PermissionClass, Plugin, PluginDescription, PluginManifest, PluginStatusContribution,
    PromptSegment, Provenance, RenderKind, ResultStatus, Role, Tool, ToolCall, ToolCategory,
    ToolCtx, ToolError, ToolName, ToolObserver, ToolOutput, ToolSpec, TruncationPolicy,
};
use conway::AgentId;

/// The install id an operator names in `[plugins].install`.
pub const PLUGIN_ID: &str = "conway.todo";

/// The bare name `TodoWriteTool` registers under.
pub const WRITE_TOOL_NAME: &str = "todo_write";

/// The bare name `TodoReadTool` registers under.
pub const READ_TOOL_NAME: &str = "todo_read";

/// The bare name `TodoListCommand` registers under -- reachable as
/// `/conway.todo.list` once the host prefixes it with [`PLUGIN_ID`].
pub const COMMAND_NAME: &str = "list";

/// `SystemNote::reason` on every PERSISTED note this plugin writes (via
/// `TodoNoteObserver`) -- the exact reason `reconstruct_from_history`
/// filters on, so the two halves of the resume mechanism agree on one
/// constant rather than two string literals that could drift apart.
pub const NOTE_REASON: &str = "conway.todo";

/// The [`PluginStatusContribution::key`] this plugin's status-line entry is
/// filed under (rendered on the status line as `todo: 2/5`).
pub const STATUS_KEY: &str = "todo";

/// One `todo_write` item's status. Exactly three values -- a closed Rust
/// enum, so an unrecognized string (e.g. `"blocked"`) fails ordinary serde
/// deserialization and therefore `todo_write`'s own argument parsing,
/// rather than being silently coerced into one of these three and
/// misrepresenting the model's own intent.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    /// The one-character glyph `render_list` prefixes each line with.
    fn glyph(self) -> char {
        match self {
            TodoStatus::Pending => ' ',
            TodoStatus::InProgress => '~',
            TodoStatus::Done => 'x',
        }
    }
}

/// One item of the current list -- also the exact shape [`NOTE_REASON`]'s
/// persisted note serializes as JSON, so `reconstruct_from_history` can
/// decode a historical note back into the same type a live `todo_write`
/// call produces.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TodoItem {
    pub id: String,
    pub text: String,
    pub status: TodoStatus,
}

/// `(done, total)` -- the exact pair both the status-line value (`2/5`) and
/// the rendered segment's header read from.
fn counts(items: &[TodoItem]) -> (usize, usize) {
    let done = items
        .iter()
        .filter(|item| item.status == TodoStatus::Done)
        .count();
    (done, items.len())
}

/// The one `{STATUS_KEY}: done/total` contribution a non-empty list
/// renders as, or nothing at all for an empty (or absent) one -- shared by
/// [`TodoPlugin::status_contributions`] (the `last_touched` fallback) and
/// [`TodoPlugin::status_contributions_for`] (looked up by the CALLER's own
/// agent) so the two can never drift onto two different renderings of the
/// same pair.
fn status_contribution_from_items(items: &[TodoItem]) -> Vec<PluginStatusContribution> {
    if items.is_empty() {
        return Vec::new();
    }
    let (done, total) = counts(items);
    vec![PluginStatusContribution {
        key: STATUS_KEY.to_string(),
        status: ResultStatus::Completed,
        value: format!("{done}/{total}"),
    }]
}

/// One line per item, `- [<glyph>] <id>: <text>` -- the SINGLE rendering
/// every reader of the current list (the `todo_write`/`todo_read` tool
/// replies, the context segment, `/conway.todo.list`) shares, so an id a
/// model just read in one place is spelled identically everywhere else it
/// might see it again.
fn render_list(items: &[TodoItem]) -> String {
    items
        .iter()
        .map(|item| format!("- [{}] {}: {}", item.status.glyph(), item.id, item.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The header line `render_list`'s own callers put above it, e.g. `conway.todo
/// (2/5 done):`.
fn header(items: &[TodoItem]) -> String {
    let (done, total) = counts(items);
    format!("conway.todo ({done}/{total} done)")
}

/// `header` + `render_list`, or a plain "no items" line when `items` is
/// empty -- the shared "describe the current list as a human-readable
/// block" used by both tools' replies and `TodoListCommand`.
fn describe(items: &[TodoItem]) -> String {
    if items.is_empty() {
        "conway.todo: no items".to_string()
    } else {
        format!("{}:\n{}", header(items), render_list(items))
    }
}

/// This plugin's whole mutable state: the current list per agent, plus
/// which agent most recently wrote one -- the latter is the single value
/// [`Plugin::status_contributions`] (which carries no per-agent context of
/// its own) reports against, mirroring a single-root-agent session's own
/// shape. `lists` is ALREADY keyed per agent, which is exactly what lets
/// [`TodoPlugin::status_contributions_for`] (board item
/// `01M48N3N1PRQXPGF6VQGK745VE`) answer for any one caller-named agent
/// with no new state at all -- only `last_touched`'s own single-value
/// fallback predates that method.
#[derive(Default)]
struct TodoState {
    lists: HashMap<AgentId, Vec<TodoItem>>,
    last_touched: Option<AgentId>,
}

/// Shared between every `Tool`/`ContextHook`/`ToolObserver`/`Command` this
/// plugin hands out, and read directly by [`TodoPlugin::status_contributions`]
/// -- one `Arc<Mutex<_>>` per [`TodoPlugin`] instance, the same shape
/// `conway-plugin-stepguard`'s own per-agent ring state takes.
type Shared = Arc<Mutex<TodoState>>;

/// True when `segment` is a PERSISTED `conway.todo` note -- carries
/// `Provenance::SystemNote { reason }` with `reason` exactly [`NOTE_REASON`],
/// the one string `TodoNoteObserver` ever writes it under. Shared by
/// [`reconstruct_from_history`] (which reads the most recent one) and
/// `TodoContextHook::before_request` (which removes every one of them from
/// the payload before appending its own freshly rendered segment -- see
/// this crate's own module doc, "Context cost stays flat") -- one predicate,
/// so the two halves of that mechanism can never drift onto two different
/// notions of "is this one of ours."
fn is_persisted_note(segment: &PromptSegment) -> bool {
    matches!(
        &segment.provenance,
        Provenance::SystemNote { reason } if reason == NOTE_REASON
    )
}

/// Scans `segments` for the LAST one [`is_persisted_note`] accepts -- the
/// shape context assembly replays a past [`NOTE_REASON`]-tagged system-note
/// record into, on every request including the first one after a resume
/// (see this crate's own module doc, "How state survives a process
/// restart") -- and decodes its text back into the list it was serialized
/// from. `None` when no such segment exists (a brand new session, or one
/// that never called `todo_write`) or the one found fails to decode (a
/// foreign or malformed note somehow sharing this reason) -- either way,
/// nothing to reconstruct, never a panic.
fn reconstruct_from_history(segments: &[PromptSegment]) -> Option<Vec<TodoItem>> {
    segments
        .iter()
        .rev()
        .filter(|segment| is_persisted_note(segment))
        .find_map(|segment| {
            let ContentBlock::Text { text } = segment.content.first()? else {
                return None;
            };
            serde_json::from_str::<Vec<TodoItem>>(text).ok()
        })
}

/// Reads the current list for `agent_id`: the in-memory cache when present,
/// otherwise `reconstruct_from_history` against `segments` -- refilling
/// the cache with whatever it found so later reads (another hook call this
/// same request, `todo_read`, the status line) see it too. Shared by
/// `TodoContextHook::before_request` (the one caller with `segments` to
/// reconstruct from) and exercised directly by this crate's own tests.
fn resolve_current(state: &Shared, agent_id: AgentId, segments: &[PromptSegment]) -> Vec<TodoItem> {
    let mut state = state.lock().expect("todo state lock poisoned");
    if let Some(items) = state.lists.get(&agent_id) {
        return items.clone();
    }
    let reconstructed = reconstruct_from_history(segments).unwrap_or_default();
    if !reconstructed.is_empty() {
        state.lists.insert(agent_id, reconstructed.clone());
    }
    reconstructed
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TodoWriteItem {
    /// A stable id to keep across calls. Omit it for a brand new item --
    /// one is minted and returned in the reply; supply it again on a later
    /// `todo_write` to keep updating the SAME item rather than creating a
    /// duplicate, since every call replaces the whole list.
    #[serde(default)]
    id: Option<String>,
    text: String,
    status: TodoStatus,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TodoWriteArgs {
    /// The WHOLE list, replacing whatever was there before -- not a patch
    /// against the previous call. Omit an item from this call to drop it;
    /// an empty array clears the list entirely.
    items: Vec<TodoWriteItem>,
}

/// Replaces the whole current list for the calling agent. See this crate's
/// own module doc for the persistence mechanism this tool feeds
/// (`TodoNoteObserver` watches for this tool's own successful calls).
struct TodoWriteTool {
    state: Shared,
}

#[async_trait]
impl Tool for TodoWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(WRITE_TOOL_NAME),
            description: "Replace the whole current task list with `items` -- not a patch, the \
                          full list every time. Give an item an `id` to keep updating the SAME \
                          item across calls; omit it on a brand new item and one is minted. An \
                          empty `items` array clears the list. Not registered by default -- \
                          installed via `[plugins].install` or `ConwayBuilder::with_plugin`."
                .to_string(),
            schema: schemars::schema_for!(TodoWriteArgs),
            category: ToolCategory::Think,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let args: TodoWriteArgs =
            serde_json::from_value(call.arguments).map_err(|e| ToolError::InvalidArguments {
                detail: e.to_string(),
            })?;

        // Reject a duplicate EXPLICIT id within this one call, before
        // anything else about it is acted on. Two items sharing an id is
        // never a legitimate "whole-list replace" -- the model named the
        // SAME item twice in what claims to be a flat list -- and letting it
        // through would silently decide a winner (whichever survives into
        // `state.lists`) rather than surfacing the model's own mistake.
        // Minted ids (an omitted `id`) are excluded: those are THIS
        // method's own choice, made fresh below, never the model's.
        let mut seen_explicit_ids = std::collections::HashSet::new();
        for item in &args.items {
            let Some(id) = item
                .id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            if !seen_explicit_ids.insert(id.to_string()) {
                return Err(ToolError::InvalidArguments {
                    detail: format!(
                        "duplicate id `{id}` in one todo_write call -- each item's id must be \
                         unique within the call"
                    ),
                });
            }
        }

        let items: Vec<TodoItem> = args
            .items
            .into_iter()
            .map(|item| TodoItem {
                id: item
                    .id
                    .filter(|id| !id.trim().is_empty())
                    .unwrap_or_else(|| ulid::Ulid::new().to_string()),
                text: item.text,
                status: item.status,
            })
            .collect();

        {
            let mut state = self.state.lock().expect("todo state lock poisoned");
            state.lists.insert(ctx.agent_id, items.clone());
            state.last_touched = Some(ctx.agent_id);
        }

        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: describe(&items),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TodoReadArgs {}

/// Reads the calling agent's current list back -- no arguments, no side
/// effect.
struct TodoReadTool {
    state: Shared,
}

#[async_trait]
impl Tool for TodoReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(READ_TOOL_NAME),
            description: "Read the current task list back, exactly as the most recent \
                          todo_write left it. Not registered by default -- installed via \
                          `[plugins].install` or `ConwayBuilder::with_plugin`."
                .to_string(),
            schema: schemars::schema_for!(TodoReadArgs),
            category: ToolCategory::Read,
            permission: PermissionClass::Safe,
        }
    }

    async fn invoke(&self, call: ToolCall, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        serde_json::from_value::<TodoReadArgs>(call.arguments).map_err(|e| {
            ToolError::InvalidArguments {
                detail: e.to_string(),
            }
        })?;
        let items = self
            .state
            .lock()
            .expect("todo state lock poisoned")
            .lists
            .get(&ctx.agent_id)
            .cloned()
            .unwrap_or_default();
        Ok(ToolOutput {
            blocks: vec![ContentBlock::Text {
                text: describe(&items),
            }],
            is_error: false,
            truncation: TruncationPolicy::None,
            artifacts: Vec::new(),
        })
    }

    fn path_args(&self) -> PathArgs {
        PathArgs::None
    }

    fn render_kind(&self) -> RenderKind {
        RenderKind::Structured
    }
}

/// Watches for this plugin's own successful `todo_write` calls and answers
/// with one note carrying the whole current list, JSON-encoded, under
/// [`NOTE_REASON`] -- the one channel a `Tool` has for getting anything
/// durably into the session log (see this crate's own module doc).
struct TodoNoteObserver {
    state: Shared,
}

#[async_trait]
impl ToolObserver for TodoNoteObserver {
    async fn after_tool_call(&self, _ctx: &ObserverCtx, call: &ObservedCall) -> ObserverAnswer {
        if call.is_error || call.tool.as_str() != WRITE_TOOL_NAME {
            return ObserverAnswer::default();
        }
        let items = {
            let state = self.state.lock().expect("todo state lock poisoned");
            state.lists.get(&call.agent_id).cloned()
        };
        let Some(items) = items else {
            return ObserverAnswer::default();
        };
        let Ok(text) = serde_json::to_string(&items) else {
            return ObserverAnswer::default();
        };
        ObserverAnswer {
            notes: vec![ObserverNote {
                text,
                reason: NOTE_REASON.to_string(),
            }],
        }
    }
}

/// Removes every stale `conway.todo` note, then renders the current list
/// as one compact segment, appended after every segment that remains --
/// see this crate's own module doc, "Context cost stays flat" and "The
/// segment never churns the cached prefix", for why a PERSISTED note is
/// never left in the payload this hands onward, and why the one segment
/// this method does add is strictly an append and never a mutation of
/// anything else that was there.
struct TodoContextHook {
    state: Shared,
}

#[async_trait]
impl ContextHook for TodoContextHook {
    async fn before_request(
        &self,
        ctx: &ContextHookCtx,
        payload: ContextPayload,
    ) -> ContextPayload {
        // Reads whichever persisted note matters BEFORE the filter below
        // removes it from the payload -- `resolve_current`'s own
        // `reconstruct_from_history` call needs the UNFILTERED segments to
        // find it at all.
        let items = resolve_current(&self.state, ctx.agent_id, &payload.segments);
        let ContextPayload { segments, tools } = payload;
        // Strip every PERSISTED `conway.todo` note already in the payload
        // (one per past `todo_write` call -- see this crate's own module
        // doc, "Context cost stays flat") so the segment appended below is
        // the ONLY `conway.todo` segment this request ever carries, no
        // matter how many `todo_write` calls preceded it. This can never
        // orphan a tool call/result pair: a persisted note's `ContentBlock`
        // is always a single `Text` block (see `TodoNoteObserver`), never a
        // `ToolUse`/`ToolResultBlock`, so removing one never touches either
        // half of a call/result pairing.
        let mut segments: Vec<PromptSegment> = segments
            .into_iter()
            .filter(|segment| !is_persisted_note(segment))
            .collect();
        if items.is_empty() {
            return ContextPayload { segments, tools };
        }
        let (done, total) = counts(&items);
        let segment = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: describe(&items),
            }],
            // Names the plugin AND the count, mirroring `conway.compaction`'s
            // own `SystemNote` reason -- never an anonymous note. Not
            // `NOTE_REASON` itself: that string is reserved for the
            // PERSISTED per-write note `is_persisted_note` filters out
            // above, so this freshly rendered segment is never mistaken for
            // (and re-filtered as) one of those on a later call within the
            // same request-building pass.
            Provenance::SystemNote {
                reason: format!("conway.todo: {done}/{total} done"),
            },
        );
        segments.push(segment);
        ContextPayload { segments, tools }
    }
}

/// `/conway.todo.list` -- prints the calling agent's current list as a
/// plain-text block, for the operator rather than the model.
struct TodoListCommand {
    state: Shared,
}

#[async_trait]
impl Command for TodoListCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: COMMAND_NAME.to_string(),
            summary: "Prints the current conway.todo list as plain text.".to_string(),
        }
    }

    async fn invoke(&self, ctx: CommandCtx) -> CommandOutcome {
        let items = self
            .state
            .lock()
            .expect("todo state lock poisoned")
            .lists
            .get(&ctx.focused_agent)
            .cloned()
            .unwrap_or_default();
        CommandOutcome::Output(vec![describe(&items)])
    }
}

/// The plugin itself: one shared `TodoState` behind every tool, hook,
/// observer and command it hands out.
#[derive(Default)]
pub struct TodoPlugin {
    state: Shared,
}

impl TodoPlugin {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Plugin for TodoPlugin {
    fn manifest(&self) -> PluginManifest {
        PluginManifest {
            id: PLUGIN_ID.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            tools: vec![
                ToolName::new(WRITE_TOOL_NAME),
                ToolName::new(READ_TOOL_NAME),
            ],
            required_host_caps: vec![],
            optional_host_caps: vec![],
            requires: vec![],
            optional: vec![],
        }
    }

    /// Honest about the opt-in choice -- see this crate's own module doc,
    /// "Opt-in, deliberately not a default".
    fn description(&self) -> PluginDescription {
        PluginDescription {
            summary: "a small task list the model writes and ticks off -- opt-in, not in the \
                      default opinion set"
                .to_string(),
            you_get: format!(
                "2 tools ({WRITE_TOOL_NAME} replaces the whole list, {READ_TOOL_NAME} reads it \
                 back), a compact segment near the end of context whenever the list is \
                 non-empty, a `{STATUS_KEY}` status-line entry (e.g. `{STATUS_KEY}: 2/5`), and \
                 `/{PLUGIN_ID}.{COMMAND_NAME}` to print it for yourself"
            ),
            you_lose: "nothing else -- with this uninstalled, no task-list segment is ever \
                       added and neither tool exists"
                .to_string(),
            costs: "exactly one short extra segment near the end of every request once a list \
                    exists -- never more, no matter how many todo_write calls preceded it; one \
                    system-note record persisted per todo_write call (for resume, not replayed \
                    into requests as more than that one segment)"
                .to_string(),
        }
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(TodoWriteTool {
                state: self.state.clone(),
            }),
            Arc::new(TodoReadTool {
                state: self.state.clone(),
            }),
        ]
    }

    fn commands(&self) -> Vec<Arc<dyn Command>> {
        vec![Arc::new(TodoListCommand {
            state: self.state.clone(),
        })]
    }

    fn context_hooks(&self) -> Vec<Arc<dyn ContextHook>> {
        vec![Arc::new(TodoContextHook {
            state: self.state.clone(),
        })]
    }

    fn observers(&self) -> Vec<Arc<dyn ToolObserver>> {
        vec![Arc::new(TodoNoteObserver {
            state: self.state.clone(),
        })]
    }

    /// The most recently written agent's `done/total` pair, or nothing at
    /// all before any `todo_write` call -- see `TodoState::last_touched`'s
    /// own doc for why this method (which carries no per-agent context of
    /// its own) reports a single value rather than one per agent. A caller
    /// that knows which agent it actually wants should call
    /// [`Self::status_contributions_for`] instead -- this method is kept
    /// exactly as it was (the required [`Plugin::status_contributions`]
    /// method still has no agent to receive) for a single-agent/scripting
    /// caller, and remains what [`Self::status_contributions_for`]'s own
    /// default (`agent: None`) falls back to.
    fn status_contributions(&self) -> Vec<PluginStatusContribution> {
        let state = self.state.lock().expect("todo state lock poisoned");
        let Some(agent) = state.last_touched else {
            return Vec::new();
        };
        let Some(items) = state.lists.get(&agent) else {
            return Vec::new();
        };
        status_contribution_from_items(items)
    }

    /// Board item `01M48N3N1PRQXPGF6VQGK745VE`: [`Self::status_contributions`],
    /// but scoped to `agent` -- `TodoState::lists` already keys its whole
    /// store by [`AgentId`], so answering for ONE caller-named agent
    /// instead of whichever one was `last_touched` needs no new state,
    /// only a different lookup key. `agent: None` falls back to
    /// [`Self::status_contributions`] unchanged, matching
    /// [`Plugin::status_contributions_for`]'s own documented default for
    /// every implementor that has no opinion about a specific agent.
    fn status_contributions_for(&self, agent: Option<AgentId>) -> Vec<PluginStatusContribution> {
        let Some(agent) = agent else {
            return self.status_contributions();
        };
        let state = self.state.lock().expect("todo state lock poisoned");
        let Some(items) = state.lists.get(&agent) else {
            return Vec::new();
        };
        status_contribution_from_items(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::plugin::ArtifactWriteHandle;
    use conway::SessionId;

    fn shared() -> Shared {
        Arc::new(Mutex::new(TodoState::default()))
    }

    fn tool_ctx(agent_id: AgentId) -> ToolCtx {
        ToolCtx::for_test(
            agent_id,
            std::path::PathBuf::from("/tmp"),
            Arc::new(conway_testkit::FakeSubagentHost::new(agent_id)),
            Arc::new(conway_testkit::CollectingEventSink::new()),
        )
    }

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

    fn cmd_ctx(agent_id: AgentId) -> CommandCtx {
        CommandCtx {
            focused_agent: agent_id,
            root_agent: agent_id,
            session_id: SessionId::new(),
            args: String::new(),
        }
    }

    fn observer_ctx() -> ObserverCtx {
        ObserverCtx {
            events: conway::plugin::PluginEventHandle::noop(PLUGIN_ID),
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "c1".to_string(),
            name: ToolName::new(name),
            arguments,
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

    fn find_tool(plugin: &TodoPlugin, name: &str) -> Arc<dyn Tool> {
        plugin
            .tools()
            .into_iter()
            .find(|t| t.spec().name.as_str() == name)
            .unwrap_or_else(|| panic!("no tool named {name}"))
    }

    // ------------------------------------------------------------------
    // Sanity: manifest/description, the plugin browser's own read surface.
    // ------------------------------------------------------------------

    #[test]
    fn manifest_id_matches_the_published_constant() {
        assert_eq!(TodoPlugin::new().manifest().id, PLUGIN_ID);
    }

    #[test]
    fn manifest_names_both_tools() {
        let manifest = TodoPlugin::new().manifest();
        assert_eq!(
            manifest.tools,
            vec![
                ToolName::new(WRITE_TOOL_NAME),
                ToolName::new(READ_TOOL_NAME)
            ]
        );
    }

    #[test]
    fn description_is_non_empty() {
        let description = TodoPlugin::new().description();
        assert!(!description.summary.is_empty());
        assert!(!description.you_get.is_empty());
        assert!(!description.you_lose.is_empty());
        assert!(!description.costs.is_empty());
    }

    #[test]
    fn plugin_declares_one_tool_hook_command_and_observer_pair() {
        let plugin = TodoPlugin::new();
        assert_eq!(plugin.tools().len(), 2);
        assert_eq!(plugin.commands().len(), 1);
        assert_eq!(plugin.context_hooks().len(), 1);
        assert_eq!(plugin.observers().len(), 1);
    }

    // ------------------------------------------------------------------
    // todo_write changes the next request's segment and the status
    // contribution -- the core acceptance criterion.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn todo_write_changes_the_next_requests_segment_and_status_contribution() {
        let plugin = TodoPlugin::new();
        let agent = AgentId::new();

        assert!(
            plugin.status_contributions().is_empty(),
            "no contribution before any write"
        );

        let write_tool = find_tool(&plugin, WRITE_TOOL_NAME);
        let out = write_tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [ {"text": "write tests", "status": "pending"} ] }),
                ),
                tool_ctx(agent),
            )
            .await
            .expect("todo_write must succeed");
        assert!(!out.is_error);

        let status = plugin.status_contributions();
        assert_eq!(status.len(), 1);
        assert_eq!(status[0].key, STATUS_KEY);
        assert_eq!(status[0].status, ResultStatus::Completed);
        assert_eq!(status[0].value, "0/1");

        let hook = &plugin.context_hooks()[0];
        let prefix = vec![user_prompt("hello")];
        let payload = ContextPayload {
            segments: prefix.clone(),
            tools: vec![],
        };
        let out_payload = hook.before_request(&hook_ctx(agent), payload).await;
        assert_eq!(
            out_payload.segments.len(),
            prefix.len() + 1,
            "exactly one segment is appended"
        );
        let appended = out_payload.segments.last().unwrap();
        let Provenance::SystemNote { reason } = &appended.provenance else {
            panic!("expected the appended segment to carry Provenance::SystemNote");
        };
        assert!(
            reason.contains("conway.todo"),
            "reason names the plugin: {reason}"
        );
        let ContentBlock::Text { text } = &appended.content[0] else {
            panic!("expected a single text block");
        };
        assert!(text.contains("write tests"));
    }

    // ------------------------------------------------------------------
    // Board item `01M48N3N1PRQXPGF6VQGK745VE`: `status_contributions_for`
    // answers for the CALLER-NAMED agent, not whichever one wrote most
    // recently -- the shape a focused-agent TUI status line needs and
    // `status_contributions` itself (no agent parameter at all) cannot
    // give it.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn status_contributions_for_follows_the_named_agent_not_the_last_writer() {
        let plugin = TodoPlugin::new();
        let agent_a = AgentId::new();
        let agent_b = AgentId::new();
        let write_tool = find_tool(&plugin, WRITE_TOOL_NAME);

        write_tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [
                        {"text": "a1", "status": "done"},
                        {"text": "a2", "status": "pending"},
                    ] }),
                ),
                tool_ctx(agent_a),
            )
            .await
            .expect("todo_write on agent_a must succeed");
        write_tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [
                        {"text": "b1", "status": "done"},
                        {"text": "b2", "status": "done"},
                        {"text": "b3", "status": "pending"},
                    ] }),
                ),
                tool_ctx(agent_b),
            )
            .await
            .expect("todo_write on agent_b must succeed");

        // `status_contributions` (no agent parameter) reports whichever
        // agent wrote MOST RECENTLY -- agent_b, here.
        let last_writer = plugin.status_contributions();
        assert_eq!(last_writer.len(), 1);
        assert_eq!(last_writer[0].value, "2/3");

        // `status_contributions_for` answers for whichever agent the
        // CALLER names -- agent_a's own 1/2, even though agent_b wrote
        // afterward.
        let for_a = plugin.status_contributions_for(Some(agent_a));
        assert_eq!(for_a.len(), 1);
        assert_eq!(for_a[0].key, STATUS_KEY);
        assert_eq!(
            for_a[0].value, "1/2",
            "status_contributions_for(agent_a) must report agent_a's OWN list, not agent_b's \
             (the most recent writer)"
        );

        let for_b = plugin.status_contributions_for(Some(agent_b));
        assert_eq!(for_b[0].value, "2/3");
    }

    #[tokio::test]
    async fn status_contributions_for_a_background_write_does_not_change_the_focused_agents_value()
    {
        let plugin = TodoPlugin::new();
        let focused = AgentId::new();
        let background = AgentId::new();
        let write_tool = find_tool(&plugin, WRITE_TOOL_NAME);

        write_tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [ {"text": "focused item", "status": "pending"} ] }),
                ),
                tool_ctx(focused),
            )
            .await
            .expect("todo_write on the focused agent must succeed");

        let before = plugin.status_contributions_for(Some(focused));
        assert_eq!(before[0].value, "0/1");

        // A write on a completely different, BACKGROUND agent -- this is
        // the exact event that used to blank or replace the focused
        // agent's own value when read through the single-value
        // `status_contributions` alone.
        write_tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [
                        {"text": "bg1", "status": "done"},
                        {"text": "bg2", "status": "done"},
                    ] }),
                ),
                tool_ctx(background),
            )
            .await
            .expect("todo_write on the background agent must succeed");

        let after = plugin.status_contributions_for(Some(focused));
        assert_eq!(
            after[0].value, "0/1",
            "a background agent's own write must never change the FOCUSED agent's own \
             contribution: {after:?}"
        );
    }

    #[test]
    fn status_contributions_for_none_falls_back_to_status_contributions() {
        let plugin = TodoPlugin::new();
        assert_eq!(
            plugin.status_contributions_for(None),
            plugin.status_contributions()
        );
    }

    // ------------------------------------------------------------------
    // The appended segment's position: after every existing segment,
    // never touching one of them.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn the_appended_segment_sits_after_every_existing_segment_unchanged() {
        let plugin = TodoPlugin::new();
        let agent = AgentId::new();
        find_tool(&plugin, WRITE_TOOL_NAME)
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [ {"text": "a", "status": "pending"} ] }),
                ),
                tool_ctx(agent),
            )
            .await
            .unwrap();

        let hook = &plugin.context_hooks()[0];
        let prefix = vec![
            user_prompt("inherited prefix"),
            user_prompt("turn 1"),
            PromptSegment::new(
                Role::ToolResult,
                vec![ContentBlock::Text {
                    text: "tool result".to_string(),
                }],
                Provenance::ToolResult {
                    call_id: "x".to_string(),
                    tool: ToolName::new("read"),
                },
            ),
        ];
        let payload = ContextPayload {
            segments: prefix.clone(),
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;

        assert_eq!(out.segments.len(), prefix.len() + 1);
        assert_eq!(
            &out.segments[..prefix.len()],
            prefix.as_slice(),
            "every existing segment must be byte-for-byte unchanged, in the same order -- the \
             appended segment must never churn the cached prefix"
        );
    }

    // ------------------------------------------------------------------
    // Empty list -> no segment.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn empty_list_produces_no_segment() {
        let plugin = TodoPlugin::new();
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
            "no todo items means the payload is returned completely unchanged"
        );
    }

    // ------------------------------------------------------------------
    // Id stability across a whole-list replace.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn id_stability_across_a_whole_list_replace() {
        let state = shared();
        let tool = TodoWriteTool {
            state: state.clone(),
        };
        let agent = AgentId::new();

        tool.invoke(
            call(
                WRITE_TOOL_NAME,
                serde_json::json!({ "items": [ {"text": "a", "status": "pending"} ] }),
            ),
            tool_ctx(agent),
        )
        .await
        .unwrap();
        let first = state
            .lock()
            .unwrap()
            .lists
            .get(&agent)
            .cloned()
            .expect("state after first write");
        assert_eq!(first.len(), 1);
        let id = first[0].id.clone();

        // Replay the SAME id, with an edited status -- the id must survive
        // unchanged, since this is a whole-list replace, not an insert.
        tool.invoke(
            call(
                WRITE_TOOL_NAME,
                serde_json::json!({ "items": [ {"id": id, "text": "a", "status": "done"} ] }),
            ),
            tool_ctx(agent),
        )
        .await
        .unwrap();
        let second = state.lock().unwrap().lists.get(&agent).cloned().unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(
            second[0].id, id,
            "a supplied id must survive a whole-list replace unchanged"
        );
        assert_eq!(second[0].status, TodoStatus::Done);

        // A THIRD write keeps that id AND adds a brand new item with none --
        // the new item must mint an id that does not collide with the one
        // already in use.
        tool.invoke(
            call(
                WRITE_TOOL_NAME,
                serde_json::json!({
                    "items": [
                        {"id": id, "text": "a", "status": "done"},
                        {"text": "b", "status": "pending"},
                    ]
                }),
            ),
            tool_ctx(agent),
        )
        .await
        .unwrap();
        let third = state.lock().unwrap().lists.get(&agent).cloned().unwrap();
        assert_eq!(third.len(), 2);
        assert_eq!(third[0].id, id);
        assert_ne!(
            third[1].id, id,
            "a freshly minted id must not collide with an existing one"
        );
    }

    // ------------------------------------------------------------------
    // The list survives resume via the log.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn the_list_survives_resume_via_the_log() {
        // The shape context assembly replays from a PERSISTED
        // `LogRecord::SystemNote` this plugin's own `ToolObserver` wrote in
        // an earlier process -- see this crate's own module doc for the
        // mechanism this reconstructs from.
        let items = vec![
            TodoItem {
                id: "t1".to_string(),
                text: "write tests".to_string(),
                status: TodoStatus::Done,
            },
            TodoItem {
                id: "t2".to_string(),
                text: "ship it".to_string(),
                status: TodoStatus::Pending,
            },
        ];
        let historical = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text {
                text: serde_json::to_string(&items).unwrap(),
            }],
            Provenance::SystemNote {
                reason: NOTE_REASON.to_string(),
            },
        );

        // A BRAND NEW plugin instance -- no in-memory state at all, exactly
        // what a freshly started process has right after `--resume`.
        let plugin = TodoPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];
        let payload = ContextPayload {
            segments: vec![historical],
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;

        assert_eq!(
            out.segments.len(),
            1,
            "the historical PERSISTED note is removed -- only the one freshly rendered segment \
             remains (see this crate's own module doc, \"Context cost stays flat\")"
        );
        let ContentBlock::Text { text } = &out.segments[0].content[0] else {
            panic!("expected a text block");
        };
        assert!(text.contains("write tests") && text.contains("ship it"));
        assert!(text.contains("1/2"));

        // The read tool, on the SAME fresh instance, must see the
        // reconstructed state too -- proving the hook backfilled the
        // in-memory cache, not merely rendered a one-off segment.
        let read_out = find_tool(&plugin, READ_TOOL_NAME)
            .invoke(call(READ_TOOL_NAME, serde_json::json!({})), tool_ctx(agent))
            .await
            .unwrap();
        let ContentBlock::Text { text } = &read_out.blocks[0] else {
            panic!("expected a text block");
        };
        assert!(text.contains("write tests") && text.contains("ship it"));
    }

    // ------------------------------------------------------------------
    // Context cost stays flat: N persisted notes, ONE segment per request.
    // ------------------------------------------------------------------

    /// The core regression this item exists for: a session carrying N
    /// PERSISTED `conway.todo` notes in its history (one per past
    /// `todo_write` call -- exactly what context assembly's own
    /// unconditional `SystemNote` replay hands this hook) must still see
    /// exactly ONE `conway.todo` segment in the request this hook builds,
    /// regardless of N.
    #[tokio::test]
    async fn n_persisted_notes_still_produce_exactly_one_todo_segment() {
        fn persisted_note(n: usize) -> PromptSegment {
            let items = vec![TodoItem {
                id: "t1".to_string(),
                text: format!("revision {n}"),
                status: TodoStatus::Pending,
            }];
            PromptSegment::new(
                Role::System,
                vec![ContentBlock::Text {
                    text: serde_json::to_string(&items).unwrap(),
                }],
                Provenance::SystemNote {
                    reason: NOTE_REASON.to_string(),
                },
            )
        }

        // A brand new plugin instance (no in-memory cache) with FIVE
        // historical notes already in the payload's history -- the shape
        // five past `todo_write` calls leave behind.
        let plugin = TodoPlugin::new();
        let agent = AgentId::new();
        let hook = &plugin.context_hooks()[0];
        let history: Vec<PromptSegment> = (0..5).map(persisted_note).collect();
        let payload = ContextPayload {
            segments: history,
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;

        let todo_segments: Vec<&PromptSegment> = out
            .segments
            .iter()
            .filter(|s| matches!(&s.provenance, Provenance::SystemNote { reason } if reason.contains("conway.todo")))
            .collect();
        assert_eq!(
            todo_segments.len(),
            1,
            "exactly one conway.todo segment must survive, regardless of how many persisted \
             notes preceded it: {:?}",
            out.segments
        );
        // The one segment that remains reflects the MOST RECENT note (the
        // last one `reconstruct_from_history` finds scanning in reverse),
        // not an earlier, staler revision.
        let ContentBlock::Text { text } = &todo_segments[0].content[0] else {
            panic!("expected a text block");
        };
        assert!(
            text.contains("revision 4"),
            "must reflect the MOST RECENT persisted note: {text}"
        );
    }

    /// The persisted note is filtered out of EVERY request's payload,
    /// including the very first one built right after it was written --
    /// there is no "let it through once" transition, so the cached prefix
    /// never contains it and therefore never churns when a later request
    /// removes it (see this crate's own module doc).
    #[tokio::test]
    async fn the_persisted_note_never_survives_into_any_requests_payload_not_even_the_first() {
        let plugin = TodoPlugin::new();
        let agent = AgentId::new();
        find_tool(&plugin, WRITE_TOOL_NAME)
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [ {"text": "a", "status": "pending"} ] }),
                ),
                tool_ctx(agent),
            )
            .await
            .unwrap();
        let observer = TodoNoteObserver {
            state: plugin.state.clone(),
        };
        let note = observer
            .after_tool_call(&observer_ctx(), &observed(agent, WRITE_TOOL_NAME, false))
            .await
            .notes
            .remove(0);
        // The exact persisted segment the FIRST request built after this
        // write would carry, per context assembly's own unconditional
        // `SystemNote` replay.
        let freshly_persisted = PromptSegment::new(
            Role::System,
            vec![ContentBlock::Text { text: note.text }],
            Provenance::SystemNote {
                reason: note.reason,
            },
        );

        let hook = &plugin.context_hooks()[0];
        let payload = ContextPayload {
            segments: vec![user_prompt("hello"), freshly_persisted],
            tools: vec![],
        };
        let out = hook.before_request(&hook_ctx(agent), payload).await;

        assert!(
            !out
                .segments
                .iter()
                .any(|s| matches!(&s.provenance, Provenance::SystemNote { reason } if reason == NOTE_REASON)),
            "the persisted note must never survive into ANY request's payload, including the \
             very first one built after it was written: {:?}",
            out.segments
        );
    }

    // ------------------------------------------------------------------
    // Removing persisted notes can never orphan a tool call/result pair.
    // ------------------------------------------------------------------

    /// `GuardedContextHook`'s own coherence check
    /// (`conway_runtime::context::hook_guard`) is `pub(crate)` to that crate
    /// and unreachable from this one (this plugin depends only on the
    /// public `conway`/`conway::plugin` facade -- see this crate's own
    /// Cargo.toml doc comment), so this test re-asserts the same invariant
    /// by hand: a `ToolUse`/`ToolResultBlock` pair interleaved with several
    /// persisted `conway.todo` notes must come out the other side with
    /// BOTH halves of the pair still present and still matched by
    /// `call_id`, proving the note-removal filter never touches a segment
    /// that isn't itself a persisted note.
    #[tokio::test]
    async fn removing_persisted_notes_leaves_every_tool_call_result_pair_intact() {
        fn persisted_note(text: &str) -> PromptSegment {
            let items = vec![TodoItem {
                id: "t1".to_string(),
                text: text.to_string(),
                status: TodoStatus::Pending,
            }];
            PromptSegment::new(
                Role::System,
                vec![ContentBlock::Text {
                    text: serde_json::to_string(&items).unwrap(),
                }],
                Provenance::SystemNote {
                    reason: NOTE_REASON.to_string(),
                },
            )
        }
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

        let plugin = TodoPlugin::new();
        let agent = AgentId::new();
        find_tool(&plugin, WRITE_TOOL_NAME)
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [ {"text": "a", "status": "pending"} ] }),
                ),
                tool_ctx(agent),
            )
            .await
            .unwrap();

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
    // Schema rejects an unknown status.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn schema_rejects_an_unknown_status() {
        let tool = TodoWriteTool { state: shared() };
        let err = tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({ "items": [ {"text": "a", "status": "blocked"} ] }),
                ),
                tool_ctx(AgentId::new()),
            )
            .await
            .expect_err("an unrecognized status must fail argument parsing");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));
    }

    // ------------------------------------------------------------------
    // A duplicate explicit id within one call is rejected.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn rejects_a_duplicate_explicit_id_within_one_call() {
        let state = shared();
        let tool = TodoWriteTool {
            state: state.clone(),
        };
        let err = tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({
                        "items": [
                            {"id": "dup", "text": "a", "status": "pending"},
                            {"id": "dup", "text": "b", "status": "pending"},
                        ]
                    }),
                ),
                tool_ctx(AgentId::new()),
            )
            .await
            .expect_err("two items sharing an explicit id in one call must be rejected");
        assert!(matches!(err, ToolError::InvalidArguments { .. }));

        // The rejected call must never have taken effect -- no half-applied
        // state left behind for a call that failed argument validation.
        assert!(
            state.lock().unwrap().lists.is_empty(),
            "a rejected todo_write must not mutate state"
        );
    }

    #[tokio::test]
    async fn omitted_ids_never_collide_with_the_duplicate_check() {
        // Two items with NO id at all -- each mints its own fresh one below
        // (`ulid::Ulid::new()`), which must never be mistaken for the
        // "duplicate explicit id" case, since neither item named one.
        let tool = TodoWriteTool { state: shared() };
        let out = tool
            .invoke(
                call(
                    WRITE_TOOL_NAME,
                    serde_json::json!({
                        "items": [
                            {"text": "a", "status": "pending"},
                            {"text": "b", "status": "pending"},
                        ]
                    }),
                ),
                tool_ctx(AgentId::new()),
            )
            .await
            .expect("two items with no id at all must succeed");
        assert!(!out.is_error);
    }

    // ------------------------------------------------------------------
    // The observer: the persistence half, in isolation.
    // ------------------------------------------------------------------

    fn observed(agent: AgentId, tool: &str, is_error: bool) -> ObservedCall {
        ObservedCall {
            agent_id: agent,
            session: SessionId::new(),
            call_id: "c1".to_string(),
            tool: ToolName::new(tool),
            arguments: serde_json::json!({}),
            is_error,
            result_seq: conway::LogSeq(1),
        }
    }

    #[tokio::test]
    async fn a_successful_todo_write_is_observed_and_notes_the_current_list() {
        let state = shared();
        let tool = TodoWriteTool {
            state: state.clone(),
        };
        let observer = TodoNoteObserver {
            state: state.clone(),
        };
        let agent = AgentId::new();
        tool.invoke(
            call(
                WRITE_TOOL_NAME,
                serde_json::json!({ "items": [ {"text": "a", "status": "pending"} ] }),
            ),
            tool_ctx(agent),
        )
        .await
        .unwrap();

        let answer = observer
            .after_tool_call(&observer_ctx(), &observed(agent, WRITE_TOOL_NAME, false))
            .await;
        assert_eq!(answer.notes.len(), 1);
        assert_eq!(answer.notes[0].reason, NOTE_REASON);
        let decoded: Vec<TodoItem> = serde_json::from_str(&answer.notes[0].text).unwrap();
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].text, "a");
    }

    #[tokio::test]
    async fn the_observer_ignores_other_tools_and_failed_calls() {
        let state = shared();
        let tool = TodoWriteTool {
            state: state.clone(),
        };
        let observer = TodoNoteObserver {
            state: state.clone(),
        };
        let agent = AgentId::new();
        tool.invoke(
            call(
                WRITE_TOOL_NAME,
                serde_json::json!({ "items": [ {"text": "a", "status": "pending"} ] }),
            ),
            tool_ctx(agent),
        )
        .await
        .unwrap();

        let other_tool = observer
            .after_tool_call(&observer_ctx(), &observed(agent, "read", false))
            .await;
        assert!(other_tool.notes.is_empty());

        let failed_write = observer
            .after_tool_call(&observer_ctx(), &observed(agent, WRITE_TOOL_NAME, true))
            .await;
        assert!(failed_write.notes.is_empty());
    }

    // ------------------------------------------------------------------
    // The operator-facing command.
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn list_command_prints_the_current_list() {
        let state = shared();
        let agent = AgentId::new();
        TodoWriteTool {
            state: state.clone(),
        }
        .invoke(
            call(
                WRITE_TOOL_NAME,
                serde_json::json!({ "items": [ {"text": "buy milk", "status": "in_progress"} ] }),
            ),
            tool_ctx(agent),
        )
        .await
        .unwrap();

        let command = TodoListCommand { state };
        let CommandOutcome::Output(lines) = command.invoke(cmd_ctx(agent)).await else {
            panic!("expected CommandOutcome::Output");
        };
        assert!(lines.join("\n").contains("buy milk"));
    }

    #[tokio::test]
    async fn list_command_says_so_when_empty() {
        let command = TodoListCommand { state: shared() };
        let CommandOutcome::Output(lines) = command.invoke(cmd_ctx(AgentId::new())).await else {
            panic!("expected CommandOutcome::Output");
        };
        assert!(lines.join(" ").contains("no items"));
    }

    #[test]
    fn command_bare_name_matches_the_published_constant() {
        let plugin = TodoPlugin::new();
        assert_eq!(plugin.commands()[0].spec().name, COMMAND_NAME);
    }
}

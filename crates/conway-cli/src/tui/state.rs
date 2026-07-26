//! `AppState`: the TUI's render model, derived purely from the same
//! `EventStream` the one-shot renderers consume (WI-114).
//!
//! `apply` is the single mutation entry point -- fed one [`Envelope`] at a
//! time by the app loop -- so this module can be unit-tested with no
//! terminal at all: construct an `AppState`, feed it a sequence of
//! `Envelope`s, and assert on the resulting `transcript`/`tree`.

use std::collections::HashMap;

use conway::{AgentId, AgentResult, Envelope, Event, ResultStatus, Usage};

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

/// One line of the transcript pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User(String),
    Assistant {
        text: String,
    },
    Tool {
        call_id: String,
        name: String,
        status: ToolStatus,
        preview: String,
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
    /// A `/ask` ephemeral fork-ask (facade `SessionHandle::ask`, WI-127
    /// criterion 5): `reply` is `None` while the forked child's turn is
    /// still in flight, populated once `App`'s spawned task resolves it.
    /// `id` correlates this entry with the async result -- there is no
    /// other identifier in scope for it (unlike `Tool`'s `call_id`, which
    /// the facade itself assigns).
    EphemeralAsk {
        id: u64,
        question: String,
        reply: Option<String>,
    },
    Notice {
        text: String,
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

/// `Normal` (the input line submits a prompt or a `/command`) or
/// `AwaitingPermission` (the input line is inert; `y`/`a`/`n`/`Esc` resolve
/// the pending prompt -- see `input.rs`). Only one prompt is shown at a
/// time; concurrent requests queue in `pending_prompts` (module notes:
/// "concurrent requests queue in arrival order").
pub enum Mode {
    Normal,
    AwaitingPermission(PendingPrompt),
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Mode::Normal => write!(f, "Normal"),
            Mode::AwaitingPermission(p) => {
                write!(f, "AwaitingPermission({})", p.request.call_id)
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
    /// Arrow-selected row in the on-demand agent panel (WI-130). Index into
    /// `tree.nodes`; clamped wherever it is read, so tree growth/shrink never
    /// leaves it dangling. Only meaningful while `agent_view_open`.
    pub agent_selected: usize,
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
    /// Monotonic id source for [`Entry::EphemeralAsk`] entries -- see that
    /// variant's own doc for why an id is needed at all.
    next_ask_id: u64,
    /// The permission overlay's own command-body scroll offset (bug fix,
    /// 01KYB0F7V65QAMZWWYH8K7DWDC: "no way to see the entire command" for a
    /// long tool-call argument). Driven by `PageUp`/`PageDown` while
    /// `Mode::AwaitingPermission` (`input.rs::handle_permission_key`), read
    /// by `view/mod.rs::draw_permission_overlay` (which clamps it to the
    /// command's own wrapped line count, so this can hold an arbitrarily
    /// large value with no risk of scrolling past real content). Reset to 0
    /// whenever a NEW prompt becomes the active one -- see
    /// [`Self::offer_prompt`]/[`Self::resolve_current_prompt`] -- so a
    /// leftover scroll position from a previous, unrelated command never
    /// carries over.
    pub permission_scroll: u16,
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
        });
        Self {
            transcript: Vec::new(),
            tree,
            last_model_decision: None,
            input: String::new(),
            cursor: 0,
            mode: Mode::Normal,
            scroll: 0,
            follow_tail: true,
            queued_prompts: std::collections::VecDeque::new(),
            agent_view_open: false,
            palette_selected: None,
            palette_stem: String::new(),
            agent_selected: 0,
            focused_agent: root,
            activity: Activity::Idle,
            focused_agent_usage: Usage::default(),
            next_ask_id: 0,
            permission_scroll: 0,
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
            Some(node) => !matches!(
                node.status,
                NodeStatus::Finished | NodeStatus::Failed | NodeStatus::Cancelled
            ),
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

    /// Moves the agent-panel selection by `delta` rows, clamped to the tree
    /// (WI-130). No wrap -- a browsing list stops at its ends.
    pub fn agent_scroll(&mut self, delta: isize) {
        let n = self.tree.nodes.len();
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

    /// Bare arrow `Up` (01KYASZPVVRCHGTEAN9XS5C6EC): [`Self::scroll_page_up`]
    /// with a one-line step -- touchpad/wheel scroll arrives here as arrow
    /// keys (this struct's own module doc), so a light nudge must move the
    /// view by one line, not jump a whole page like `PageUp` does. Same
    /// clamp/follow-disengage behavior as `scroll_page_up`, just a step of 1.
    pub fn scroll_line_up(&mut self, max_scroll: u16) {
        self.scroll_page_up(1, max_scroll);
    }

    /// Bare arrow `Down`'s counterpart to [`Self::scroll_line_up`]: one-line
    /// step, same re-engage-follow-at-the-bottom behavior as
    /// [`Self::scroll_page_down`].
    pub fn scroll_line_down(&mut self, max_scroll: u16) {
        self.scroll_page_down(1, max_scroll);
    }

    /// Records a `/ask` question as a pending [`Entry::EphemeralAsk`] and
    /// returns its id, for the caller (`app.rs`) to correlate with the async
    /// reply once the spawned fork-ask task resolves.
    pub fn push_ephemeral_ask(&mut self, question: String) -> u64 {
        let id = self.next_ask_id;
        self.next_ask_id += 1;
        self.transcript.push(Entry::EphemeralAsk {
            id,
            question,
            reply: None,
        });
        id
    }

    /// Fills in the reply for the [`Entry::EphemeralAsk`] matching `id`.
    /// A no-op if `id` is not found (e.g. the entry scrolled out of a
    /// truncated transcript in some future change) -- never panics.
    pub fn resolve_ephemeral_ask(&mut self, id: u64, reply: String) {
        for entry in self.transcript.iter_mut().rev() {
            if let Entry::EphemeralAsk {
                id: eid, reply: r, ..
            } = entry
            {
                if *eid == id {
                    *r = Some(reply);
                    return;
                }
            }
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
            self.permission_scroll = 0;
        } else {
            self.queued_prompts.push_back(prompt);
        }
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
        if let Some(next) = self.queued_prompts.pop_front() {
            self.mode = Mode::AwaitingPermission(next);
            // Same reset as `offer_prompt`'s initial promotion -- a queued
            // prompt for a different call must not inherit the just-resolved
            // one's scroll position.
            self.permission_scroll = 0;
        }
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
                ephemeral,
                parent,
                agent_def,
                ..
            } => {
                // Ephemeral `/ask`-style forks (decision
                // 01KYD1TWXMZD4BT842CMJT1AED) are a distinct /btw-like
                // category, not persistent tree subagents. Drop the live
                // event here so the `/agents` panel and the inline
                // transcript never list them: do NOT call
                // `apply_agent_spawned` (no tree node, no `Entry::Agent`).
                // The runtime's own `AgentTree` snapshot -- what `tree()`
                // returns (P-2 provenance) -- STILL includes ephemeral
                // children; this filter is on the live event stream only,
                // never on `AppState::tree` itself (see
                // `ephemeral_event_filter_does_not_affect_tree_snapshot`).
                if *ephemeral {
                    return;
                }
                self.apply_agent_spawned(env.agent, *parent, agent_def.clone());
            }
            Event::AgentFinished {
                result,
                ephemeral,
                ..
            } => {
                // Same ephemeral filter as the `AgentSpawned` arm above:
                // an ephemeral child's finish must not reset the focused
                // agent's activity indicator, must not update any tree
                // node, and must not push a transcript entry. The
                // dedicated `/ask` UI is handled separately by
                // `push_ephemeral_ask`/`resolve_ephemeral_ask`; this just
                // prevents the live `AgentFinished` arm from
                // double-counting the ephemeral child.
                if *ephemeral {
                    return;
                }
                self.apply_agent_finished(env.agent, result);
                // Board item 01KYAGP11FF9YC3G60TWHHKKST: the focused
                // agent's own finish is the terminal "stopped working"
                // signal -- an unrelated agent (sibling/other subtree)
                // finishing must not reset an activity indicator that is
                // about the FOCUSED agent specifically.
                if env.agent == self.focused_agent {
                    self.activity = Activity::Idle;
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
                }
            }
            Event::ThinkingDelta { .. } => {
                if env.agent == self.focused_agent {
                    self.activity = Activity::Thinking;
                }
            }
            Event::TextDelta { text } => {
                self.append_assistant_text(text);
                if env.agent == self.focused_agent {
                    self.activity = Activity::Responding;
                }
            }
            Event::ToolCallProposed { call_id, tool, .. } => {
                self.transcript.push(Entry::Tool {
                    call_id: call_id.clone(),
                    name: tool.to_string(),
                    status: ToolStatus::Proposed,
                    preview: String::new(),
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
                    self.activity = Activity::Idle;
                    self.focused_agent_usage += *usage;
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
            Event::Error { error, fatal } => {
                self.transcript.push(Entry::Notice {
                    text: format!("{}error: {error}", if *fatal { "fatal " } else { "" }),
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
        parent: Option<AgentId>,
        agent_def: Option<String>,
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
                if p == self.focused_agent {
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

    fn append_assistant_text(&mut self, delta: &str) {
        if let Some(Entry::Assistant { text }) = self.transcript.last_mut() {
            text.push_str(delta);
        } else {
            self.transcript.push(Entry::Assistant {
                text: delta.to_string(),
            });
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
                Entry::Assistant { text } => Some(text.as_str()),
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

    #[test]
    fn push_ephemeral_ask_starts_with_no_reply() {
        let mut state = AppState::new(AgentId::new());
        let id = state.push_ephemeral_ask("what's the status?".to_string());
        assert!(matches!(
            state.transcript.last(),
            Some(Entry::EphemeralAsk { id: eid, question, reply: None })
                if *eid == id && question == "what's the status?"
        ));
    }

    #[test]
    fn resolve_ephemeral_ask_fills_in_the_matching_entry() {
        let mut state = AppState::new(AgentId::new());
        let id = state.push_ephemeral_ask("q".to_string());

        state.resolve_ephemeral_ask(id, "the answer".to_string());

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::EphemeralAsk { reply: Some(r), .. }) if r == "the answer"
        ));
    }

    #[test]
    fn resolve_ephemeral_ask_targets_the_right_entry_among_several() {
        let mut state = AppState::new(AgentId::new());
        let first = state.push_ephemeral_ask("first".to_string());
        let second = state.push_ephemeral_ask("second".to_string());

        state.resolve_ephemeral_ask(first, "first reply".to_string());

        let find = |id: u64| {
            state.transcript.iter().find_map(|e| match e {
                Entry::EphemeralAsk { id: eid, reply, .. } if *eid == id => Some(reply.clone()),
                _ => None,
            })
        };
        assert_eq!(find(first), Some(Some("first reply".to_string())));
        assert_eq!(find(second), Some(None));
    }

    #[test]
    fn resolve_ephemeral_ask_with_unknown_id_does_not_panic_or_mutate() {
        let mut state = AppState::new(AgentId::new());
        let _id = state.push_ephemeral_ask("q".to_string());
        let before = state.transcript.clone();

        state.resolve_ephemeral_ask(9999, "stray reply".to_string());

        assert_eq!(state.transcript, before);
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

    // ---- 01KYASZPVVRCHGTEAN9XS5C6EC: bare-arrow one-line scroll (as
    // opposed to PageUp/PageDown's full-page step, above) ----

    #[test]
    fn scroll_line_up_from_following_moves_exactly_one_line_and_disengages_follow() {
        let mut state = AppState::new(AgentId::new());
        assert!(state.follow_tail);

        state.scroll_line_up(20);

        assert_eq!(
            state.scroll, 19,
            "Up must step by exactly ONE line off the bottom, not a page"
        );
        assert!(!state.follow_tail);
    }

    #[test]
    fn scroll_line_up_clamps_at_the_top() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 0;
        state.follow_tail = false;

        state.scroll_line_up(20);

        assert_eq!(state.scroll, 0, "must not go negative / wrap");
        assert!(!state.follow_tail);
    }

    #[test]
    fn scroll_line_down_moves_exactly_one_line_and_reengages_follow_at_the_bottom() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 19;
        state.follow_tail = false;

        state.scroll_line_down(20);

        assert_eq!(
            state.scroll, 20,
            "Down must step by exactly ONE line, not a page"
        );
        assert!(
            state.follow_tail,
            "landing back on the bottom must re-engage auto-follow"
        );
    }

    #[test]
    fn scroll_line_down_short_of_the_bottom_leaves_follow_disengaged() {
        let mut state = AppState::new(AgentId::new());
        state.scroll = 10;
        state.follow_tail = false;

        state.scroll_line_down(20);

        assert_eq!(state.scroll, 11);
        assert!(
            !state.follow_tail,
            "must not re-engage follow until actually back at the bottom"
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
    // `UserTurn` followed by an `Assistant` record (`AgentProgress{note:
    // "user turn: ..."}` then `TextDelta{..}`, per that function's own
    // mapping) and asserts both land as real, visible transcript content. ----

    #[test]
    fn agent_progress_pushes_a_visible_notice() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        state.apply(&envelope(
            session,
            agent,
            Event::AgentProgress {
                note: "user turn: hi".to_string(),
            },
        ));

        assert!(matches!(
            state.transcript.last(),
            Some(Entry::Notice { text }) if text == "user turn: hi"
        ));
    }

    #[test]
    fn replayed_user_turn_and_assistant_reply_both_render_in_the_transcript() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let mut state = AppState::new(agent);

        // Exactly the envelope sequence `record_to_event` now synthesizes
        // for one `UserTurn` record followed by one `Assistant` record on
        // replay (`SessionHandle::agent_events`/`events_from`'s replay
        // batch): `AgentProgress{note: "user turn: {text}"}`, then
        // `TextDelta{text}` carrying the assistant's full reply.
        state.apply(&envelope(
            session,
            agent,
            Event::AgentProgress {
                note: "user turn: hi".to_string(),
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
                Entry::Notice { text } if text.contains("hi")
            )),
            "the replayed user prompt must be visible somewhere in the transcript: {:?}",
            state.transcript
        );
        assert!(
            state.transcript.iter().any(|e| matches!(
                e,
                Entry::Assistant { text } if text == "hello there"
            )),
            "the replayed assistant reply must render as a real Entry::Assistant, not be \
             dropped: {:?}",
            state.transcript
        );
    }

    #[test]
    fn a_notice_between_two_replayed_assistant_turns_keeps_them_as_separate_entries() {
        // The consecutive-turns concern from the review: since each
        // replayed user turn now pushes a non-`Assistant` `Entry::Notice`
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
                Event::AgentProgress {
                    note: format!("user turn: {prompt}"),
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
                Entry::Assistant { text } => Some(text.as_str()),
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

    // ---- Board item 01KYD2GY1QASN3PB8YEPY99SGS (conway_ask item e):
    // ephemeral forks must never appear in the `/agents` panel nor push
    // inline lifecycle entries into the focused agent's transcript. The
    // filter is at `apply`'s `Event::AgentSpawned`/`AgentFinished` arms
    // ONLY -- the runtime `tree()` snapshot still includes ephemeral
    // children (P-2), and `AppState::tree` is NOT filtered at the data
    // structure level (a directly-`insert`ed ephemeral node stays). ----

    #[test]
    fn ephemeral_spawn_does_not_appear_in_appstate_tree() {
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
                inherited_upto: None,
                ephemeral: true,
            },
        ));

        assert!(
            !state.tree.nodes.iter().any(|n| n.agent_id == child),
            "an ephemeral spawn must not insert a tree node"
        );
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
        // behavior the ephemeral filter must preserve for `ephemeral: false`).
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

        assert!(
            state.tree.nodes.iter().any(|n| n.agent_id == child),
            "a non-ephemeral spawn must still insert a tree node"
        );
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
    fn ephemeral_finished_does_not_reset_activity_or_push_transcript() {
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        // The focused agent is the parent (`root`), so without the filter the
        // finish's `if env.agent == self.focused_agent` guard would NOT fire
        // (it's the child finishing, not the parent) -- but `apply_agent_finished`
        // would still run, set tree status, and update any matching `Entry::Agent`.
        // Pre-set a non-Idle activity so a reset is observable, and pre-seed the
        // child as `Running` so `set_tree_status` would have a node to mutate.
        state.focused_agent = root;
        state.activity = Activity::Responding;
        state.tree.insert(TreeNode {
            agent_id: child,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Running,
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
            "an ephemeral finish must not touch the focused agent's activity"
        );
        assert_eq!(
            state.transcript, transcript_before,
            "an ephemeral finish must not push any transcript entry"
        );
        // The pre-seeded tree node status must NOT be advanced to `Finished`
        // by the ephemeral finish -- the filter drops the event before
        // `apply_agent_finished` runs.
        let node = state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == child)
            .expect("the pre-seeded child node must still be present (filter is on the event, not the tree)");
        assert_eq!(
            node.status,
            NodeStatus::Running,
            "an ephemeral finish must not update the child's tree node status"
        );
    }

    #[test]
    fn ephemeral_event_filter_does_not_affect_tree_snapshot() {
        // The filter is on the live event only -- never on `AppState::tree`
        // itself. A node `insert`ed directly (mimicking what the runtime
        // `tree()` snapshot would carry, P-2) must survive an ephemeral spawn
        // event for a DIFFERENT agent being dropped by `apply`.
        let session = SessionId::new();
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let ephemeral_in_tree = AgentId::new();
        let other = AgentId::new();
        // Directly seed an ephemeral node -- bypassing the `apply` filter.
        state.tree.insert(TreeNode {
            agent_id: ephemeral_in_tree,
            parent: Some(root),
            agent_def: None,
            status: NodeStatus::Running,
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
            state.tree.nodes.iter().any(|n| n.agent_id == ephemeral_in_tree),
            "a directly-seeded ephemeral node must stay -- the filter is on the live event only, not the tree"
        );
        assert!(
            !state.tree.nodes.iter().any(|n| n.agent_id == other),
            "the ephemeral spawn event itself must still be dropped (no node for `other`)"
        );
    }
}

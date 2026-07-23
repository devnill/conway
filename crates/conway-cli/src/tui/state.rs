//! `AppState`: the TUI's render model, derived purely from the same
//! `EventStream` the one-shot renderers consume (WI-114).
//!
//! `apply` is the single mutation entry point -- fed one [`Envelope`] at a
//! time by the app loop -- so this module can be unit-tested with no
//! terminal at all: construct an `AppState`, feed it a sequence of
//! `Envelope`s, and assert on the resulting `transcript`/`tree`.

use std::collections::HashMap;

use conway::{AgentId, AgentResult, Envelope, Event, ResultStatus};

use super::gate::PendingPrompt;

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
    pub scroll: u16,
    /// Prompts that arrived while another was already showing -- drained
    /// into `mode` as each one resolves (module notes: "concurrent requests
    /// queue in arrival order").
    pub queued_prompts: std::collections::VecDeque<PendingPrompt>,
    /// Whether the below-chat agent-tree panel (WI-127 criterion 4) is
    /// currently shown. Toggled by `/agents` (handled in `app.rs`, since
    /// `commands.rs` -- out of this item's file scope -- owns no such
    /// command); never an always-on pane.
    pub agent_view_open: bool,
    /// Monotonic id source for [`Entry::EphemeralAsk`] entries -- see that
    /// variant's own doc for why an id is needed at all.
    next_ask_id: u64,
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
            queued_prompts: std::collections::VecDeque::new(),
            agent_view_open: false,
            next_ask_id: 0,
        }
    }

    /// Shows/hides the below-chat agent-tree panel (`/agents`, WI-127
    /// criterion 4). A pure toggle -- no facade call, no transcript entry --
    /// so it is unit-testable with no `Host`/`SessionHandle` at all.
    pub fn toggle_agent_view(&mut self) {
        self.agent_view_open = !self.agent_view_open;
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
        }
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
                parent, agent_def, ..
            } => {
                self.apply_agent_spawned(env.agent, *parent, agent_def.clone());
            }
            Event::AgentFinished { result } => {
                self.apply_agent_finished(env.agent, result);
            }
            Event::TextDelta { text } => {
                self.append_assistant_text(text);
            }
            Event::ToolCallProposed { call_id, tool, .. } => {
                self.transcript.push(Entry::Tool {
                    call_id: call_id.clone(),
                    name: tool.to_string(),
                    status: ToolStatus::Proposed,
                    preview: String::new(),
                });
                self.set_tree_status(env.agent, NodeStatus::Running);
            }
            Event::PermissionRequested { call_id, .. } => {
                self.set_tool_status(call_id, ToolStatus::AwaitingPermission);
                self.set_tree_status(env.agent, NodeStatus::AwaitingPermission);
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
                self.transcript.push(Entry::Agent {
                    agent_id: agent,
                    label: agent_def.clone().unwrap_or_else(|| "agent".to_string()),
                    status: NodeStatus::Running,
                });
                Some(p)
            }
            Some(p) => {
                self.transcript.push(Entry::Notice {
                    text: format!("agent {agent} claimed unknown parent {p}; attached under root"),
                });
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
        if self.tree.root == Some(agent) {
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
        AgentResult, PermissionDecisionKind, ResultStatus, SessionId, SubagentMode, ToolName,
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
}

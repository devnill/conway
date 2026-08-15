//! The interactive session's own agent-tree data model
//! ([`AgentTreeView`], [`TreeNode`], [`NodeStatus`]) and how it is built
//! from the event stream: [`AppState::apply_agent_spawned`]/
//! [`AppState::apply_agent_finished`] (called from [`AppState::apply`]),
//! plus [`AppState::ensure_agent_tracked`], the out-of-band seed a bare
//! `/spawn`/`/fork` uses before its own `AgentSpawned` event can arrive
//! (see that method's own doc). The `/agents` panel's draw-time
//! navigation/visibility-filter state lives in [`super::agent_panel`] --
//! a separate seam over the SAME tree this module owns the data of.

use super::*;

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
    pub(super) fn is_terminal(self) -> bool {
        matches!(
            self,
            NodeStatus::Finished | NodeStatus::Failed | NodeStatus::Cancelled
        )
    }
}

impl AgentTreeView {
    pub(super) fn get_mut(&mut self, id: AgentId) -> Option<&mut TreeNode> {
        self.index.get(&id).map(|&i| &mut self.nodes[i])
    }

    fn contains(&self, id: AgentId) -> bool {
        self.index.contains_key(&id)
    }

    pub(super) fn insert(&mut self, node: TreeNode) {
        let id = node.agent_id;
        self.index.insert(id, self.nodes.len());
        self.nodes.push(node);
    }
}

impl AppState {
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
    /// `Self::apply_agent_spawned` itself no-ops (only refreshes status)
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

    pub(super) fn apply_agent_spawned(
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
        // Recognized-parent spawns get an inline `Entry::Agent` (
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

    pub(super) fn apply_agent_finished(&mut self, agent: AgentId, result: &AgentResult) {
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

    pub(super) fn set_tree_status(&mut self, agent: AgentId, status: NodeStatus) {
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
    use super::*;
    use crate::tui::state::fixtures::{envelope, spawned};
    use conway::SessionId;

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

    /// Criterion 4: a recognized-parent spawn must show up inline in
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
}

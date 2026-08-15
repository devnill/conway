//! The `/agents` panel's own draw-time state: [`AgentVisibility`] (the
//! terminal-status visibility filter, item A2) and the panel's navigation
//! ([`AppState::visible_agent_nodes`], [`AppState::agent_scroll`],
//! [`AppState::toggle_agent_view`]), plus the focus-liveness guards that
//! decide whether the currently focused agent can still be usefully
//! prompted ([`AppState::is_focused_agent_live`],
//! [`AppState::block_message_if_focused_agent_finished`]) and the
//! session-root accessors ([`AppState::root_agent`],
//! [`AppState::is_root_focused`]). The tree DATA these read is
//! [`super::agent_tree`]'s own seam -- this module never mutates it.

use super::*;

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

impl AppState {
    /// The session's own root agent, as recorded in `tree.root` at
    /// construction (`AppState::new`) -- the target [`Self::focus_agent`]
    /// returns to ("Esc, or focusing the root row" both resolve
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
    /// FILTERED row list (+ item A2). No wrap -- a browsing list
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

    /// Shows/hides the below-chat agent-tree panel (`/agents`
    /// criterion 4). A pure toggle -- no facade call, no transcript entry --
    /// so it is unit-testable with no `Host`/`SessionHandle` at all.
    pub fn toggle_agent_view(&mut self) {
        self.agent_view_open = !self.agent_view_open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

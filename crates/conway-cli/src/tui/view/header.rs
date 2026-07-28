//! The sticky context header + the floating "jump to bottom" footer (T6).
//!
//! Two small, independently-drawn scroll affordances that answer the
//! "scrolled-back with no idea where I am" report:
//!
//! - **The sticky header** ([`draw`]) is a single plain line -- `session ·
//!   focused agent · model · ctx%` -- pinned above the transcript pane
//!   whenever the transcript actually overflows the viewport
//!   (`view::mod::layout`'s own doc explains the non-recursive
//!   overflow test that decides this without a layout feedback loop).
//!   When content fits on screen, no header row is reserved at all -- the
//!   layout does not shift for a session that never needs to scroll.
//!
//!   **V5** extends the off-root `agent <id>` field into a lineage
//!   breadcrumb -- `agent <id> via root → fork @seq 3 → @reviewer` -- so
//!   focusing a subagent conveys where it sits in the tree, not just which
//!   one it is (the "clicking into a subagent doesn't show the parents"
//!   report). It is METADATA only: each hop's text is the same
//!   `view::agents::recipe_parts`/`hop_label` provenance string the
//!   `/agents` panel row already shows for that node (fork's `@seq N`
//!   fork point, spawn's `@agent_def`/`(inherit)`) -- never the ancestor's
//!   actual transcript CONTENT. That is deliberate, not a shortcut: a fork
//!   child truly inherited its parent's log up to `inherited_upto` and
//!   showing that would be accurate, but a spawn child inherited nothing,
//!   and rendering parent content next to it would show the user
//!   information the agent itself never saw. Distinguishing the two
//!   correctly at CONTENT granularity would need the ancestor's actual log
//!   fetched into `AppState` (a bigger change, and not among the fields the
//!   item's spec says already exist); staying at metadata sidesteps the
//!   trap entirely while still making fork vs spawn visibly different in
//!   the chain text. Degrades through shorter complete forms under a
//!   narrow terminal or a deep chain (`agent_field`'s `LineageDetail`,
//!   [`header_line`]'s width-fit search) -- the same "shorter complete
//!   form, never a mid-word clip" precedent [`footer_text`] already set.
//!   The walk itself is bounded (`view::agents::ancestor_chain`, P-10): a
//!   cycle in `parent` (should be impossible) ends the walk rather than
//!   hanging.
//! - **The floating footer** ([`draw_scroll_footer`]) is a small pill drawn
//!   over the BOTTOM ROW of the transcript area while `!state.follow_tail`
//!   (the user has scrolled away from the tail): `↓ N lines above tail --
//!   End to jump to bottom`. It disappears the instant `follow_tail`
//!   re-engages (`End`, or paging back down to the true bottom).
//!
//! **Neither widget is ever part of the transcript's own `Paragraph`**
//! (`view/transcript.rs`'s clean-copy guarantee: no `.block(..)`, no glyph
//! `entry_lines` did not itself emit). Both are drawn as their own,
//! separate `frame.render_widget` calls from `view::draw` -- the header
//! into its own reserved `Rect` above the transcript, the footer as a
//! `Clear` + `Paragraph` OVERLAY on top of the transcript's own last row,
//! the same "modal overlay drawn over transcript content, never folded
//! into its `Span`s" pattern `view/mod.rs`'s permission/`/ask`/intent
//! overlays already use. `entry_lines`/`build_lines` themselves are
//! completely untouched by this module -- the
//! `entry_lines_never_contain_box_drawing_glyphs` and
//! `rendered_buffer_contains_no_box_drawing_glyphs` tests in
//! `transcript.rs` still pass unmodified.
//!
//! **Mouse wheel stays out of scope.** `view/transcript.rs`'s own module
//! doc already explains why crossterm mouse capture is not enabled (it
//! would disable the terminal's native click-drag text selection, which
//! the clean-copy guarantee exists to protect). T6 ships `PageUp`/
//! `PageDown` (existing) plus `End`/`Home` (new, this module's keys) and
//! this floating footer as the keyboard-only, selection-preserving answer
//! to "how do I get back to the bottom" -- not a mouse-wheel workaround.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use conway::AgentId;

use super::agents;
use super::status;
use super::theme::Theme;
use crate::tui::state::AppState;

/// The sticky header's fixed height -- always exactly one plain line, no
/// border (mirroring `view/status.rs`'s own single-line, borderless
/// treatment of the bottom status line).
pub const HEADER_HEIGHT: u16 = 1;

/// Renders the sticky context header into `area` -- `view/mod.rs::layout`
/// only ever calls this with a `Some` header `Rect` (reserved only while
/// the transcript overflows), so there is no visibility check here: by the
/// time this runs, the caller has already decided the header belongs on
/// screen.
pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let paragraph = Paragraph::new(header_line(state, theme, area.width));
    frame.render_widget(paragraph, area);
}

/// The header's plain-text content: `session <id> [· agent <id>[ via
/// <lineage>]] [· model] · ctx%`, joined with ` · `. The bracketed `agent`
/// field is present only while the transcript is NOT showing the session's
/// own root (mirroring `view/status.rs::hint_spans`'s identical
/// "off-root only" convention for its own `focused: <id>` note, so the
/// common single-agent case stays uncluttered); `model` is present only
/// once the focused agent's first `Event::ModelDecision` has routed
/// (mirrors the status line's `model` field, which is likewise omitted
/// before that point). `ctx%`/raw-tokens reuses `status::ctx_label`
/// directly -- see that function's own doc for why (never a second,
/// drift-prone copy of the percentage formula).
///
/// `width` is the header's own render `Rect` width (`draw`'s caller,
/// `view/mod.rs`) -- V5's lineage breadcrumb is the one part of this line
/// whose length is NOT bounded by construction (a deep ancestry chain can
/// be arbitrarily long), so unlike the pre-V5 line, this now degrades the
/// breadcrumb through shorter complete forms (`agent_field`) rather than
/// letting ratatui hard-clip the `Rect` mid-word.
fn header_line(state: &AppState, theme: &Theme, width: u16) -> Line<'static> {
    let session = format!("session {}", agents::short_agent_id(state.root_agent()));
    let model = state.focused_model.clone();
    let ctx = status::ctx_label(state);
    let assemble = |agent_field: Option<String>| -> String {
        let mut parts = vec![session.clone()];
        parts.extend(agent_field);
        parts.extend(model.clone());
        parts.push(ctx.clone());
        parts.join(" · ")
    };

    let chosen = if state.is_root_focused() {
        assemble(None)
    } else {
        [
            LineageDetail::Full,
            LineageDetail::Compact,
            LineageDetail::Bare,
        ]
        .into_iter()
        .map(|detail| assemble(Some(agent_field(state, detail))))
        // `+ 2` for the line's own leading/trailing padding space below.
        .find(|line| line.chars().count() + 2 <= width as usize)
        .unwrap_or_else(|| assemble(Some(agent_field(state, LineageDetail::Bare))))
    };

    Line::from(Span::styled(format!(" {chosen} "), theme.header))
}

/// V5: how much of the focused agent's ancestry [`agent_field`] renders,
/// tried in this order (most informative first) by [`header_line`]'s
/// width-fit search -- the same "shorter COMPLETE form, never a mid-word
/// clip" shape `footer_text` already uses for the floating footer.
#[derive(Clone, Copy)]
enum LineageDetail {
    /// The whole chain, one hop per ancestor: `agent <id> via root →
    /// <hop1> → ... → <hopN>`.
    Full,
    /// Ancestors between the head and the immediate parent collapse to a
    /// `…(N)` count; the head and the LAST hop (how the focused agent
    /// itself came to be -- the single most locally relevant fact) stay
    /// named.
    Compact,
    /// The pre-V5 field with no lineage at all: `agent <id>`.
    Bare,
}

/// Builds the off-root `agent` field at `detail`'s verbosity. `Bare` never
/// touches the tree at all (guaranteed cheap, and the guaranteed-fits
/// fallback). `Full`/`Compact` walk [`agents::ancestor_chain`] (bounded,
/// P-10) and label each hop with [`agents::hop_label`] -- the SAME
/// provenance text `view/agents.rs`'s panel row already shows for that
/// node, so the breadcrumb and the panel can never disagree about how a
/// given agent came to exist. A node with `kind: None` (the root itself,
/// or one seeded out-of-band via `ensure_agent_tracked`, which never saw a
/// spawn event) has no recipe text to show; `hop_label` already falls back
/// to that node's own short id there, so it renders as "here's WHO" rather
/// than being mislabeled as a fork or a spawn it never was.
fn agent_field(state: &AppState, detail: LineageDetail) -> String {
    let bare = format!("agent {}", agents::short_agent_id(state.focused_agent));
    if matches!(detail, LineageDetail::Bare) {
        return bare;
    }

    // Root-first: `chain[0]` is the topmost ancestor reached (normally the
    // session root itself), `chain.last()` is the focused agent.
    let chain = agents::ancestor_chain(state, state.focused_agent);
    if chain.len() <= 1 {
        // The focused agent has no tree node at all (should not happen for
        // anything `focus_agent` was actually called with, but
        // `is_focused_agent_live` already fails open for this same case
        // elsewhere) -- nothing to walk, so no lineage claim is made.
        return bare;
    }

    let hop_text = |id: &AgentId| -> String {
        state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == *id)
            .map(agents::hop_label)
            .unwrap_or_else(|| agents::short_agent_id(*id))
    };
    // The chain's own head reads as literal "root" when it actually IS the
    // session root (the overwhelmingly common case, and already named by
    // the header's own leading `session <id>` field) -- otherwise (the
    // walk was cut short by the P-10 bound or a missing node) it gets the
    // same hop treatment as everything after it, so a truncated chain
    // still names what it actually reached instead of silently claiming
    // "root".
    let head = if chain[0] == state.root_agent() {
        "root".to_string()
    } else {
        hop_text(&chain[0])
    };
    let hops: Vec<String> = chain[1..].iter().map(hop_text).collect();

    let via = match detail {
        LineageDetail::Full => std::iter::once(head)
            .chain(hops)
            .collect::<Vec<_>>()
            .join(" → "),
        LineageDetail::Compact if hops.len() <= 1 => std::iter::once(head)
            .chain(hops)
            .collect::<Vec<_>>()
            .join(" → "),
        LineageDetail::Compact => {
            let omitted = hops.len() - 1;
            format!(
                "{head} → …({omitted}) → {}",
                hops.last().expect("hops.len() > 1 checked above")
            )
        }
        LineageDetail::Bare => unreachable!("returned above"),
    };

    format!("{bare} via {via}")
}

/// Renders the floating "jump to bottom" pill over the bottom row of
/// `transcript_area` while `!state.follow_tail`; a no-op while following
/// (nothing to jump back to) or if `transcript_area` has no rows at all
/// (an extreme small-terminal edge case). `max_scroll` is caller-computed
/// (`view::mod::draw`, via `view::max_scroll`) -- this module has no
/// terminal-width/height of its own to derive the wrapped line count from,
/// the same reason every other scroll-adjacent function in this crate
/// takes `max_scroll` as a parameter rather than recomputing it.
///
/// Drawn as a SEPARATE `Clear` + `Paragraph` overlay directly on the
/// frame -- never folded into `transcript::draw`'s own `Paragraph`, so it
/// can never leak into the transcript's clean-copy text. This is the exact
/// "modal drawn over transcript content" shape `view/mod.rs`'s permission/
/// `/ask`/intent-confirm overlays already use (`Clear`, then a widget, over
/// a sub-`Rect` of the transcript area) -- just one row tall instead of
/// claiming most of the pane.
pub fn draw_scroll_footer(
    frame: &mut Frame,
    transcript_area: Rect,
    state: &AppState,
    theme: &Theme,
    max_scroll: u16,
) {
    if state.follow_tail || transcript_area.height == 0 {
        return;
    }
    let above = state.lines_above_tail(max_scroll);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height - 1,
        width: transcript_area.width,
        height: 1,
    };
    let text = footer_text(above, area.width);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(Span::styled(text, theme.scroll_footer)),
        area,
    );
}

/// The footer's text at `width` columns, degrading rather than being
/// clipped mid-word.
///
/// The full form names both the position and the way out. On a narrow
/// terminal that does not fit, a hard truncation would cut the `End` hint
/// off first -- the half that tells the user what to *do* -- leaving a
/// dangling `↓ 8 lines above tai`. So the variants drop information in
/// order of least usefulness: the wordy form, then the terse one, then the
/// bare count. Whatever fits is complete; nothing is ever shown as a
/// fragment.
fn footer_text(above: u16, width: u16) -> String {
    let width = width as usize;
    let candidates = [
        format!(" ↓ {above} lines above tail — End to jump to bottom "),
        format!(" ↓ {above} above — End to jump "),
        format!(" ↓ {above} — End "),
        format!(" ↓{above} "),
    ];
    candidates
        .iter()
        .find(|c| c.chars().count() <= width)
        .cloned()
        // Every candidate is wider than the pane (a terminal only a few
        // columns wide). Show the shortest and let the terminal clip it --
        // there is no shorter honest form left to fall back to.
        .unwrap_or_else(|| candidates[candidates.len() - 1].clone())
}

#[cfg(test)]
mod tests {
    use conway::AgentId as TestAgentId;

    use super::*;
    use crate::tui::state::{Entry, NodeStatus, TreeNode};

    /// A header width generous enough that no test in this module
    /// accidentally exercises the [`LineageDetail`] degrade path unless it
    /// means to -- see the dedicated `agent_field_degrades_*` tests below
    /// for that.
    const WIDE: u16 = 200;

    fn thirty_lines_state() -> AppState {
        let mut state = AppState::new(TestAgentId::new());
        for i in 0..30 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state
    }

    // ---- header_line content ----

    #[test]
    fn header_line_includes_session_and_ctx_but_not_agent_or_model_by_default() {
        let root = TestAgentId::new();
        let state = AppState::new(root);
        let text = plain(&header_line(&state, &Theme::default(), WIDE));

        assert!(
            text.contains(&format!("session {}", agents::short_agent_id(root))),
            "{text}"
        );
        assert!(text.contains("ctx"), "{text}");
        assert!(
            !text.contains("agent "),
            "root-focused must not show a redundant `agent <id>` field: {text}"
        );
    }

    #[test]
    fn header_line_shows_the_focused_agent_off_root() {
        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        let child = TestAgentId::new();
        state.focus_agent(child);

        let text = plain(&header_line(&state, &Theme::default(), WIDE));

        assert!(text.contains(&format!("session {}", agents::short_agent_id(root))), "{text}");
        assert!(
            text.contains(&format!("agent {}", agents::short_agent_id(child))),
            "{text}"
        );
    }

    #[test]
    fn header_line_shows_the_model_once_known() {
        let mut state = AppState::new(TestAgentId::new());
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());
        let text = plain(&header_line(&state, &Theme::default(), WIDE));
        assert!(text.contains("anthropic/claude-sonnet-4-6"), "{text}");
    }

    #[test]
    fn header_line_ctx_matches_the_shared_status_line_helper() {
        let mut state = AppState::new(TestAgentId::new());
        state.focused_model_max_context = Some(200_000);
        state.focused_ctx_tokens = 50_000; // 25%
        let text = plain(&header_line(&state, &Theme::default(), WIDE));
        assert!(
            text.contains(&status::ctx_label(&state)),
            "the header must reuse status::ctx_label verbatim, not a second \
             copy of the percentage formula: {text}"
        );
        assert!(text.contains("ctx 25%"), "{text}");
    }

    fn plain(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn push_node(
        state: &mut AppState,
        id: AgentId,
        parent: AgentId,
        agent_def: Option<&str>,
        kind: Option<conway::SubagentMode>,
        inherited_upto: Option<conway::LogSeq>,
    ) {
        state.tree.nodes.push(TreeNode {
            agent_id: id,
            parent: Some(parent),
            agent_def: agent_def.map(str::to_string),
            status: NodeStatus::Running,
            kind,
            inherited_upto,
            ephemeral: false,
        });
    }

    // ---- V5: lineage breadcrumb (agent_field) ----

    /// The item's most important test: a fork child and a spawn child are
    /// distinguished in the breadcrumb, and the spawn child's header never
    /// shows the parent's actual content -- only the recipe metadata
    /// (`hop_label`/`recipe_parts`), which is all a spawn child ever truly
    /// has.
    #[test]
    fn fork_and_spawn_children_are_distinguished_and_spawn_shows_no_parent_content() {
        use conway::{LogSeq, SubagentMode};

        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        // Distinctive marker that would only ever appear if the header
        // leaked the PARENT's actual conversation content -- the header
        // must never contain it for either child.
        state.transcript.push(Entry::Assistant {
            text: "PARENT-SECRET-6f2c".to_string(),
            model: None,
            summary: None,
            ts: None,
        });

        let fork_child = TestAgentId::new();
        push_node(
            &mut state,
            fork_child,
            root,
            None,
            Some(SubagentMode::Fork),
            Some(LogSeq(5)),
        );
        let spawn_child = TestAgentId::new();
        push_node(
            &mut state,
            spawn_child,
            root,
            Some("reviewer"),
            Some(SubagentMode::Spawn),
            None,
        );

        state.focus_agent(fork_child);
        let fork_text = plain(&header_line(&state, &Theme::default(), WIDE));
        assert!(
            fork_text.contains("fork @seq 5"),
            "the fork child's breadcrumb must name its fork point: {fork_text}"
        );
        assert!(
            !fork_text.contains("PARENT-SECRET"),
            "the header is metadata-only -- it must never embed the \
             parent's actual transcript content: {fork_text}"
        );

        state.focus_agent(spawn_child);
        let spawn_text = plain(&header_line(&state, &Theme::default(), WIDE));
        assert!(
            spawn_text.contains("@reviewer"),
            "the spawn child's breadcrumb must name its agent_def: {spawn_text}"
        );
        assert!(
            !spawn_text.contains("PARENT-SECRET"),
            "a spawn child inherits NOTHING from its parent -- the header must \
             never show it parent content it does not have: {spawn_text}"
        );
        assert_ne!(
            fork_text.replace(&fork_child.to_string()[..8], ""),
            spawn_text.replace(&spawn_child.to_string()[..8], ""),
            "fork and spawn must render visibly differently in the chain: \
             fork={fork_text:?} spawn={spawn_text:?}"
        );
    }

    /// A node with `kind: None` between the root and the focused agent
    /// (seeded out-of-band, e.g. `ensure_agent_tracked`, which never saw a
    /// spawn event) must render sensibly -- its own short id -- rather than
    /// being mislabeled as a fork or a spawn.
    #[test]
    fn a_kindless_ancestor_renders_its_short_id_not_a_mislabeled_recipe() {
        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        let untracked_kind = TestAgentId::new();
        push_node(&mut state, untracked_kind, root, None, None, None);
        let focused = TestAgentId::new();
        push_node(
            &mut state,
            focused,
            untracked_kind,
            None,
            Some(conway::SubagentMode::Spawn),
            None,
        );
        state.focus_agent(focused);

        let text = plain(&header_line(&state, &Theme::default(), WIDE));
        assert!(
            text.contains(&agents::short_agent_id(untracked_kind)),
            "a kindless ancestor must still be named, by its own short id: {text}"
        );
        assert!(
            !text.contains("fork"),
            "a kindless ancestor must never be mislabeled as a fork: {text}"
        );
    }

    /// A deep ancestry chain degrades to a shorter COMPLETE form rather
    /// than being clipped mid-word, and never panics -- even at a GENEROUS
    /// width, since a 40-hop `Full` chain is long enough to force `Compact`
    /// on its own, independent of terminal size.
    #[test]
    fn deep_ancestry_chain_degrades_to_the_compact_ellipsis_form() {
        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        let mut cursor = root;
        for i in 0..40 {
            let next = TestAgentId::new();
            push_node(
                &mut state,
                next,
                cursor,
                Some(format!("agent{i}").as_str()),
                Some(conway::SubagentMode::Spawn),
                None,
            );
            cursor = next;
        }
        state.focus_agent(cursor);

        let text = plain(&header_line(&state, &Theme::default(), WIDE));
        assert!(
            text.contains("…(39)"),
            "a 40-hop chain must collapse to the compact ellipsis form even \
             at a generous width: {text}"
        );
        assert!(
            text.contains("@agent39"),
            "the LAST hop (how the focused agent itself came to be) must \
             stay named: {text}"
        );
        assert!(
            !text.contains("@agent0"),
            "an omitted middle hop must not appear as a dangling fragment: {text}"
        );

        // Narrower widths degrade further, all the way to the bare field --
        // never a fragment, never a panic.
        for width in [60u16, 30, 15, 0] {
            let narrow = plain(&header_line(&state, &Theme::default(), width));
            assert!(
                !narrow.trim().is_empty() || width == 0,
                "a deep chain must still render something at width {width}"
            );
        }

        // Also exercise it end to end through the real render pass (would
        // panic on any width-arithmetic underflow).
        let backend = ratatui::backend::TestBackend::new(20, 24);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, 20, HEADER_HEIGHT), &state, &Theme::default()))
            .expect("draw must not panic on a deep chain at a narrow width");
    }

    /// The narrowest candidate (`Bare`, e.g. `agent <id>`) is always tried
    /// last and is what a too-narrow terminal falls back to -- it is a
    /// complete field on its own, not a fragment of a longer one.
    #[test]
    fn narrow_width_falls_back_to_the_bare_agent_field_not_a_fragment() {
        use conway::SubagentMode;

        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        let child = TestAgentId::new();
        push_node(
            &mut state,
            child,
            root,
            Some("a-fairly-long-agent-definition-name"),
            Some(SubagentMode::Spawn),
            None,
        );
        state.focus_agent(child);

        let text = plain(&header_line(&state, &Theme::default(), 18));
        assert!(
            text.contains(&format!("agent {}", agents::short_agent_id(child))),
            "even the narrowest form must still name the focused agent: {text}"
        );
        assert!(
            !text.contains("via"),
            "a too-narrow width must fall back to the plain `agent <id>` \
             field, not a truncated lineage form: {text}"
        );
    }

    // ---- draw_scroll_footer ----

    /// The header's visibility rule is the whole reason `layout` needs a
    /// non-recursive overflow test: a short conversation must not pay a row
    /// for it. This exercises the rule end to end through the real
    /// `view::draw`, not the predicate in isolation.
    #[test]
    fn header_is_absent_when_content_fits_and_present_once_it_overflows() {
        use crate::tui::test_support::render_text;

        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(Entry::Assistant {
            text: "only line".to_string(),
            model: None,
            summary: None,
            ts: None,
        });

        // 24 rows, one transcript line: nothing to scroll, so no header.
        let fits = render_text(&state, 60, 24);
        assert!(
            !fits.contains(&format!("session {}", agents::short_agent_id(root))),
            "a transcript that fits on screen must not reserve a header row: {fits}"
        );

        // Same viewport, far more content than rows: the header appears.
        for i in 0..80 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        let overflows = render_text(&state, 60, 24);
        assert!(
            overflows.contains(&format!("session {}", agents::short_agent_id(root))),
            "an overflowing transcript must show the sticky header: {overflows}"
        );
    }

    #[test]
    fn footer_is_absent_while_following() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let state = thirty_lines_state();
        assert!(state.follow_tail);

        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 20, 8);
                draw_scroll_footer(f, area, &state, &Theme::default(), 20);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            !text.contains("jump to bottom"),
            "the footer must not render while follow_tail is set: {text}"
        );
    }

    #[test]
    fn footer_shows_the_correct_lines_above_tail_count_while_scrolled_up() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = thirty_lines_state();
        state.follow_tail = false;
        state.scroll = 12; // max_scroll(20) - 12 = 8 lines above tail.

        // Wide enough for the full form -- `footer_text`'s narrow variants
        // are exercised separately below.
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 60, 8);
                draw_scroll_footer(f, area, &state, &Theme::default(), 20);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        let text: String = buffer.content().iter().map(|c| c.symbol()).collect();
        assert!(
            text.contains("8 lines above tail"),
            "the footer must name the live lines-above-tail count: {text}"
        );
        assert!(text.contains("End to jump to bottom"), "{text}");
    }

    /// The footer degrades to a shorter complete form rather than being
    /// clipped mid-word: a truncated ` ↓ 8 lines above tai` loses the `End`
    /// hint, which is the half that tells the user what to do about it.
    #[test]
    fn footer_text_degrades_to_a_complete_shorter_form_when_narrow() {
        let wide = footer_text(8, 60);
        assert!(wide.contains("lines above tail"), "{wide}");
        assert!(wide.contains("End to jump to bottom"), "{wide}");

        for width in [40u16, 20, 12, 6] {
            let text = footer_text(8, width);
            assert!(
                text.chars().count() <= width as usize,
                "width {width} must not overflow: {text:?} ({} chars)",
                text.chars().count()
            );
            assert!(
                text.contains('8'),
                "every form must still name the count: {text:?}"
            );
        }

        // The narrower forms keep the `End` affordance for as long as it
        // fits at all.
        assert!(footer_text(8, 32).contains("End"), "{}", footer_text(8, 32));
    }

    #[test]
    fn footer_is_a_separate_overlay_not_part_of_the_transcript_paragraph() {
        // End to end: through the REAL `view::draw`, the footer text must
        // land on screen while scrolled up, but `transcript::entry_lines`
        // itself never emits it -- proven by the fact that scrolling back
        // to the tail makes it disappear again with no state change to the
        // transcript's own entries (only `follow_tail` changed).
        use crate::tui::test_support::render_text;

        let mut state = thirty_lines_state();
        state.follow_tail = false;
        state.scroll = 0;

        let scrolled_up = render_text(&state, 60, 8);
        assert!(
            scrolled_up.contains("lines above tail"),
            "{scrolled_up}"
        );

        state.follow_tail = true;
        let following = render_text(&state, 60, 8);
        assert!(
            !following.contains("lines above tail"),
            "the footer must disappear once follow_tail re-engages, with no \
             transcript entry mutation at all: {following}"
        );
    }
}

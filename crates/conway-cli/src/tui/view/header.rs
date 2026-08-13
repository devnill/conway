//! The sticky prompt overlay + the floating "jump to bottom" footer.
//!
//! **This item corrects a requirement miss in T6.** T6's problem statement
//! was scroll-shaped -- "you scroll and lose track of where you are" -- but
//! its binding decision put `session · agent <id>[ via lineage] · model ·
//! ctx%` in the overlay: application CHROME (what session/agent/model am I
//! in), not an answer to "what am I looking at". The tell was T6's own
//! gating: it showed that line only while the transcript overflowed the
//! viewport, and nobody gates session/model/ctx on scroll position if they
//! actually mean it as persistent chrome -- chrome that flickers with scroll
//! position is noise, not information. The user's own correction: "the
//! sticky header isn't a full app header. its just for scrolling, it was a
//! requirement miss... We want a sticky header showing the prompt (or a
//! preview of the prompt)."
//!
//! So this module now draws exactly one thing above the transcript: the
//! CURRENT TURN'S OWN PROMPT, and only while it has scrolled out of view.
//! `session`/`model`/`ctx%` stay exactly where T6 already had them (the
//! status line, `view/status.rs`) -- they were never removed, just never
//! duplicated up here. The lineage breadcrumb (V5) is the one field that
//! DID need a new home (it had none before T6 misfiled it here): it moved to
//! `view/status.rs`'s new `Lineage` field, taking its width-degrade
//! machinery (`LineageDetail`) and its fork/spawn-content trap with it
//! unchanged -- see that module's own doc for the full "never show a spawn
//! child its parent's content" reasoning, which still applies verbatim.
//!
//! - **The sticky prompt** ([`draw_sticky_prompt`]) is a single plain line
//!   pinned to the TOP of the transcript pane, shown only while the current
//!   turn's own `Entry::User` prompt has scrolled above the viewport --
//!   see that function's own doc for the exact trigger and the wrapped-row
//!   mapping that decides it.
//! - **The floating footer** ([`draw_scroll_footer`]) is unchanged from T6:
//!   a small pill drawn over the BOTTOM ROW of the transcript area while
//!   `!state.follow_tail` (the user has scrolled away from the tail):
//!   `↓ N lines above tail -- End to jump to bottom`. It disappears the
//!   instant `follow_tail` re-engages (`End`, or paging back down to the
//!   true bottom).
//!
//! **Neither widget reserves a layout row.** T6's sticky header claimed a
//! `Constraint::Length` row from `view/mod.rs::layout` whenever the
//! transcript overflowed -- a reserved row that could appear/disappear
//! between renders, reflowing the very content the reader was looking at
//! right under them. An overlay needs no such reservation: both widgets here
//! are drawn AFTER `transcript::draw`, as their own separate
//! `frame.render_widget` calls straight onto the already-rendered frame --
//! the sticky prompt at `transcript_area`'s own top row, the footer at its
//! own bottom row. `entry_lines`/`build_lines` themselves are completely
//! untouched by this module -- the `entry_lines_never_contain_box_drawing_
//! glyphs` and `rendered_buffer_contains_no_box_drawing_glyphs` tests in
//! `transcript.rs` still pass unmodified, and the transcript viewport is
//! never a row shorter (or taller) because one of these widgets happened to
//! turn on or off.
//!
//! **The trigger is "is THIS TURN'S prompt on screen", not "did the
//! transcript overflow".** T6's old header used the wrong test entirely
//! (`transcript overflows the viewport`) and the floating footer's test
//! (`!follow_tail`) is *also* wrong for this purpose: a short turn scrolled
//! back only slightly still has its own prompt genuinely on screen, and must
//! show no overlay even though `follow_tail` is already false. The actual
//! rule ([`draw_sticky_prompt`]): draw only when the entry governing the
//! viewport's very top row is NOT (and does not contain) the nearest
//! preceding `Entry::User` -- i.e. the prompt has scrolled fully above the
//! visible area. This also settles which prompt to name: the NEAREST
//! `Entry::User` at or before whatever the top visible row belongs to --
//! "sticky" in the editor sense (the heading currently in scope), never
//! simply "the most recent prompt anywhere in the transcript", which would
//! name an unrelated question several turns back the moment the reader
//! scrolls into an earlier one.
//!
//! **A focused spawn child with no user turn of its own shows nothing.**
//! `AppState::focus_agent` clears the transcript down to that agent's OWN
//! log with no lineage content mixed in (see that method's own doc) -- a
//! spawn child inherited nothing from its parent, so if its own log has no
//! `Entry::User` yet, there is no prompt this overlay could show without
//! reaching into a parent's content the agent itself never saw. The governing-
//! prompt search below returns `None` in exactly that case, and `None` draws
//! nothing -- it never falls back to an ancestor's prompt.
//!
//! **Mouse wheel stays out of scope.** `view/transcript.rs`'s own module
//! doc already explains why crossterm mouse capture is not enabled (it
//! would disable the terminal's native click-drag text selection, which
//! the clean-copy guarantee exists to protect). `PageUp`/`PageDown` (existing)
//! plus `End`/`Home` (T6) and the floating footer remain the keyboard-only,
//! selection-preserving answer to "how do I get back to the bottom".

use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use super::theme::Theme;
use super::transcript;
use crate::tui::state::{AppState, Entry};

/// Renders the sticky prompt overlay over the TOP row of `transcript_area`
/// while [`governing_prompt`] finds a prompt that has scrolled out of view;
/// a no-op when it finds none (root turn still on screen, or a freshly
/// focused agent/spawn child with no `Entry::User` of its own yet).
///
/// `effective_scroll` is the SAME clamped, wrapped-row scroll offset
/// `transcript::draw` actually rendered from this frame (`view/mod.rs::draw`
/// recomputes it fresh, mirroring the way it already recomputes
/// `max_scroll` for the floating footer, rather than threading a private
/// out of that render pass) -- so the trigger below can never disagree with
/// what is actually on screen.
///
/// Drawn as a `Clear` + `Paragraph` overlay directly on the frame -- never
/// folded into `transcript::draw`'s own `Paragraph` (this module's doc), and
/// never reserving a layout row of its own (also this module's doc): a
/// transcript exactly as tall on a frame where this overlay shows as one
/// where it does not.
pub fn draw_sticky_prompt(
    frame: &mut Frame,
    transcript_area: Rect,
    state: &AppState,
    theme: &Theme,
    effective_scroll: u16,
) {
    if transcript_area.height == 0 || transcript_area.width == 0 {
        return;
    }
    let Some(prompt) = governing_prompt(state, effective_scroll, transcript_area.width) else {
        return;
    };
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y,
        width: transcript_area.width,
        height: 1,
    };
    let text = sticky_prompt_text(prompt, area.width);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(Span::styled(text, theme.header)), area);
}

/// Finds the `Entry::User` text this frame's sticky overlay should show, or
/// `None` when no overlay is warranted.
///
/// 1. [`transcript::entry_row_starts`] maps each transcript entry to the
///    WRAPPED row (not logical line -- see that function's own doc for why
///    the distinction matters) its own first rendered line starts at, up
///    through the entry whose range contains `effective_scroll` -- it
///    short-circuits there (its own doc), so this never re-wraps entries
///    below the one this lookup actually cares about.
/// 2. The entry governing the viewport's very top VISIBLE row is whichever
///    entry's range contains `effective_scroll` -- the largest index whose
///    start row is `<= effective_scroll`.
/// 3. The prompt to show is the NEAREST `Entry::User` at or before that
///    entry (the "sticky" governing heading, never simply the most recent
///    prompt anywhere in the transcript).
/// 4. If that nearest prompt IS the entry governing the top row itself, at
///    least some part of the actual prompt is genuinely on screen already
///    (a short turn scrolled back only slightly, or a multi-line prompt
///    scrolled to a row within its own wrapped span) -- the overlay must
///    stay away rather than point at a prompt the reader can already see.
///    Otherwise the prompt has scrolled fully above the viewport, so its
///    text is returned.
/// 5. No `Entry::User` at or before the top row at all (a freshly focused
///    spawn child with nothing of its own yet) returns `None` -- draw
///    nothing, never an ancestor's prompt (this module's doc).
///
/// **A `state.follow_tail` short-circuit was considered and deliberately
/// NOT added here** (a code-review suggestion, on the theory that "the
/// overlay is hidden while following the tail anyway"). That theory does
/// not hold: while following the tail, `effective_scroll` is `max_scroll`,
/// the row at the viewport's own top while bottom-anchored -- and if the
/// CURRENT turn's own response is itself taller than the viewport (a long
/// streaming answer, the exact moment this function runs most often), that
/// top row sits inside the response, past the prompt that already scrolled
/// off above it. The overlay is exactly right to show there: the reader is
/// auto-following a long answer and has lost sight of which question it's
/// answering. A blanket `if state.follow_tail { return None }` gate would
/// have silently hidden the overlay in precisely that case -- a behavior
/// change, not a no-op optimization -- so the actual fix for the
/// performance finding is [`transcript::entry_row_starts`]'s own
/// `scroll_row` short-circuit instead (see that function's doc).
fn governing_prompt(state: &AppState, effective_scroll: u16, width: u16) -> Option<&str> {
    if state.transcript.is_empty() {
        return None;
    }
    let starts = transcript::entry_row_starts(state, width, effective_scroll);
    let top_entry = starts
        .partition_point(|&s| s <= effective_scroll)
        .saturating_sub(1);

    let (user_idx, text) = state.transcript[..=top_entry]
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, e)| match e {
            Entry::User(text) => Some((i, text.as_str())),
            _ => None,
        })?;

    if user_idx == top_entry {
        None
    } else {
        Some(text)
    }
}

/// The overlay's rendered text at `width` columns: `↑ {prompt}`, the prompt
/// flattened to one line, control bytes stripped, and mid-string truncated
/// with an ellipsis if it still doesn't fit.
///
/// **Truncation here is a deliberate exception**, not a lapse in the
/// "shorter complete form, never a mid-word clip" precedent `footer_text`
/// (below) and `view/status.rs`'s `LineageDetail` degrade both follow: those
/// two have a menu of alternate, always-COMPLETE phrasings to fall back
/// through. A user's own prompt has no such menu -- there is no shorter
/// complete rewrite of someone's own words available to fall back to -- so
/// once it doesn't fit, real mid-string truncation with a `…` sentinel is
/// the only honest option left.
///
/// Two more things a pasted prompt can carry that plain formatting would
/// mishandle:
/// - **Multi-line** (`Alt`/`Shift-Enter` insert literal `\n`s) -- flattened
///   to one line (newlines replaced with a space) before measuring width, so
///   a multi-line prompt still renders as one tidy overlay row rather than
///   pushing the footer/transcript around or silently showing only its
///   first line.
/// - **Control bytes** -- bracketed paste means a pasted prompt can contain
///   raw ANSI escapes (`\x1b[...`). This overlay renders the text as a
///   `Span`; an unescaped control byte reaching the terminal through it
///   could inject styling that was never actually part of the message. Every
///   `char::is_control` byte is dropped before truncation.
///
///   This FILTERS where the rest of the codebase REPLACES (see
///   `conway_core::text::sanitize_control_chars`, which rewrites a control
///   char to `U+FFFD`). The difference is deliberate, and load-bearing here:
///   this function MEASURES display width to decide where to truncate. A
///   control char itself renders with zero width, so dropping it yields an
///   accurate visible width; replacing it with `U+FFFD` (display width 1)
///   would INFLATE the measured width by one column per control char and
///   truncate earlier than the text actually warrants. Replacing is correct
///   everywhere a string feeds token structure or model context (where the
///   evidence must be preserved); filtering is correct here, where only the
///   visible width matters. Do not "deduplicate" this site onto the shared
///   sanitizer without changing the truncation behavior in the same commit.
///
/// Truncation itself counts CHARACTERS, not bytes -- slicing a `String` by
/// byte offset can land mid multi-byte UTF-8 sequence and render as garbage;
/// `chars().take(n)` can't split a scalar value in two.
fn sticky_prompt_text(prompt: &str, width: u16) -> String {
    const PREFIX: &str = "↑ ";
    let flat: String = prompt.split('\n').collect::<Vec<_>>().join(" ");
    let clean: String = flat.chars().filter(|c| !c.is_control()).collect();
    // `+ 2` for the line's own leading/trailing padding space, mirroring
    // `footer_text`'s identical accounting below.
    let budget = (width as usize).saturating_sub(PREFIX.chars().count() + 2);
    let body = truncate_chars_with_ellipsis(&clean, budget);
    format!(" {PREFIX}{body} ")
}

/// Truncates `text` to at most `budget` characters, replacing the tail with
/// a single `…` sentinel once it doesn't fit whole -- never panics on any
/// input (untrusted: a 100KB pasted prompt, a pure-emoji prompt, or `budget == 0`
/// all degrade to *something*, never a crash and never a mid-byte split).
fn truncate_chars_with_ellipsis(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    if budget == 1 {
        return "…".to_string();
    }
    let mut truncated: String = text.chars().take(budget - 1).collect();
    truncated.push('…');
    truncated
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
/// "modal overlay drawn over transcript content, never folded into its
/// `Span`s" pattern `view/mod.rs`'s permission/`/ask`/intent overlays already
/// use (`Clear`, then a widget, over a sub-`Rect` of the transcript area) --
/// just one row tall instead of claiming most of the pane.
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
    use crate::tui::test_support::{render, render_text};

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

    // ---- sticky prompt overlay: trigger + content ----

    /// Acceptance: one turn longer than the viewport, scrolled down, shows
    /// that turn's own prompt; scrolled back until the prompt is visible,
    /// shows nothing.
    #[test]
    fn sticky_prompt_shows_the_current_turns_prompt_once_it_scrolls_off_and_hides_once_visible() {
        let mut state = AppState::new(TestAgentId::new());
        state
            .transcript
            .push(Entry::User("what is the plan for today".to_string()));
        for i in 0..40 {
            state.transcript.push(Entry::Assistant {
                text: format!("reply line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.follow_tail = false;

        // Scrolled deep into the reply: the prompt is long gone off the top.
        state.scroll = 30;
        let scrolled = render_text(&state, 60, 10);
        assert!(
            scrolled.contains("what is the plan for today"),
            "the overlay must name the current turn's own prompt once it \
             scrolls off screen: {scrolled}"
        );

        // Scrolled all the way back to the top: the prompt itself is on
        // screen, so the overlay must not appear.
        state.scroll = 0;
        let at_top = render_text(&state, 60, 10);
        let overlay_rows = at_top
            .lines()
            .filter(|l| l.contains('↑') && l.contains("what is the plan"))
            .count();
        assert_eq!(
            overlay_rows, 0,
            "once the prompt is itself on screen the overlay must not \
             duplicate it: {at_top}"
        );
    }

    /// Acceptance (the test that distinguishes this from the trivial "most
    /// recent prompt" version): three turns, scrolled into the FIRST one --
    /// the overlay must show the first prompt, never the third.
    #[test]
    fn sticky_prompt_shows_the_governing_prompt_not_the_most_recent_one() {
        let mut state = AppState::new(TestAgentId::new());
        state
            .transcript
            .push(Entry::User("first question".to_string()));
        for i in 0..20 {
            state.transcript.push(Entry::Assistant {
                text: format!("first reply {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state
            .transcript
            .push(Entry::User("second question".to_string()));
        state.transcript.push(Entry::Assistant {
            text: "second reply".to_string(),
            model: None,
            summary: None,
            ts: None,
        });
        state
            .transcript
            .push(Entry::User("third question".to_string()));
        state.transcript.push(Entry::Assistant {
            text: "third reply".to_string(),
            model: None,
            summary: None,
            ts: None,
        });

        state.follow_tail = false;
        // Scrolled to view the middle of the FIRST turn's long reply --
        // well before "second question" ever entered the picture.
        state.scroll = 10;

        let text = render_text(&state, 60, 8);
        assert!(
            text.contains("first question"),
            "the overlay must name the FIRST turn's prompt, the one \
             governing the current viewport: {text}"
        );
        assert!(
            !text.contains("third question"),
            "the overlay must never name the most recent prompt when it is \
             not the one governing the current viewport: {text}"
        );
    }

    /// Verifies the reasoning documented above `governing_prompt` (and
    /// relied on by `entry_row_starts`'s short-circuit doc): the sticky
    /// overlay is NOT gated on `!state.follow_tail`. A single turn whose own
    /// response is taller than the viewport, while auto-following the tail
    /// (the default, and the common case DURING active streaming), must
    /// still show the overlay -- the prompt genuinely has scrolled off the
    /// top even though the reader never manually scrolled. This is the
    /// concrete evidence that a blanket `follow_tail` skip would have been a
    /// behavior change, not a safe no-op.
    #[test]
    fn sticky_prompt_shows_while_following_the_tail_of_a_response_taller_than_the_viewport() {
        let mut state = AppState::new(TestAgentId::new());
        state
            .transcript
            .push(Entry::User("what is the plan for today".to_string()));
        for i in 0..40 {
            state.transcript.push(Entry::Assistant {
                text: format!("reply line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        assert!(state.follow_tail, "AppState::new must default to following");

        let text = render_text(&state, 60, 10);
        assert!(
            text.contains("what is the plan for today"),
            "the overlay must show even while follow_tail is true, once the \
             current turn's response has grown taller than the viewport: {text}"
        );
    }

    /// Acceptance: still works after a focus switch -- `focus_agent` clears
    /// the transcript, and the newly focused agent's own long turn must
    /// still get its own sticky prompt, not the previous focus's.
    #[test]
    fn sticky_prompt_still_works_after_a_focus_switch() {
        let root = TestAgentId::new();
        let mut state = AppState::new(root);
        state
            .transcript
            .push(Entry::User("root prompt".to_string()));
        for i in 0..10 {
            state.transcript.push(Entry::Assistant {
                text: format!("root reply {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }

        let child = TestAgentId::new();
        state.focus_agent(child);
        assert!(
            state.transcript.is_empty(),
            "focus_agent clears the transcript"
        );

        state
            .transcript
            .push(Entry::User("child prompt".to_string()));
        for i in 0..40 {
            state.transcript.push(Entry::Assistant {
                text: format!("child reply {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.follow_tail = false;
        state.scroll = 30;

        let text = render_text(&state, 60, 8);
        assert!(
            text.contains("child prompt"),
            "the overlay must show the NEWLY focused agent's own prompt: {text}"
        );
        assert!(
            !text.contains("root prompt"),
            "the overlay must never show a stale prompt from before the \
             focus switch: {text}"
        );
    }

    /// Acceptance: a focused spawn child with no user turn of its own draws
    /// nothing -- never an ancestor's prompt.
    #[test]
    fn sticky_prompt_draws_nothing_for_a_focused_child_with_no_user_turn() {
        let mut state = AppState::new(TestAgentId::new());
        for i in 0..40 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.follow_tail = false;
        state.scroll = 30;

        let text = render_text(&state, 60, 8);
        assert!(
            !text.contains('↑'),
            "with no Entry::User at all, the overlay must draw nothing: {text}"
        );
    }

    /// Acceptance: no layout row is reserved for the overlay in any state --
    /// the transcript's own viewport height never changes because the
    /// overlay is showing or not.
    #[test]
    fn sticky_prompt_never_reserves_a_layout_row() {
        use crate::tui::view;

        let mut with_overlay = AppState::new(TestAgentId::new());
        with_overlay
            .transcript
            .push(Entry::User("prompt".to_string()));
        for i in 0..40 {
            with_overlay.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        with_overlay.follow_tail = false;
        with_overlay.scroll = 30;

        let without_overlay = AppState::new(TestAgentId::new());

        let area = Rect::new(0, 0, 60, 24);
        assert_eq!(
            view::transcript_area(&with_overlay, area).height,
            view::transcript_area(&without_overlay, area).height,
            "the transcript viewport must be the SAME height whether the \
             sticky prompt overlay is showing or not -- it is drawn over \
             the transcript, never reserved as its own layout row"
        );
    }

    /// Acceptance: a multi-line prompt renders on ONE overlay row (flattened
    /// before measuring width), a long prompt truncates by CHARACTER with an
    /// ellipsis rather than breaking a grapheme mid-way, and raw control
    /// bytes render inert rather than reaching the terminal as live escapes.
    #[test]
    fn sticky_prompt_text_flattens_multiline_truncates_by_char_and_strips_control_bytes() {
        let multiline = "line one\nline two\nline three";
        let flattened = sticky_prompt_text(multiline, 200);
        assert!(!flattened.contains('\n'), "{flattened:?}");
        assert!(flattened.contains("line one"), "{flattened:?}");
        assert!(flattened.contains("line two"), "{flattened:?}");
        assert!(flattened.contains("line three"), "{flattened:?}");

        // A multi-byte emoji prompt, truncated at a narrow width -- must
        // never panic and must never split a scalar value (which would
        // render replacement-garbage or crash a strict UTF-8 assertion).
        let emoji_prompt = "🎉".repeat(50);
        for width in [0u16, 1, 2, 5, 20, 200] {
            let text = sticky_prompt_text(&emoji_prompt, width);
            // Must be valid UTF-8 (guaranteed by `String`, but assert the
            // call itself never panics across every width).
            let _ = text.len();
        }

        // A pasted control byte (bracketed paste can carry a raw ANSI
        // escape) must never survive into the rendered text.
        let with_escape = "hello\x1b[31mworld";
        let cleaned = sticky_prompt_text(with_escape, 200);
        assert!(
            !cleaned.contains('\u{1b}'),
            "the raw ESC control byte must never reach the rendered Span: {cleaned:?}"
        );
        assert!(cleaned.contains("hello"), "{cleaned:?}");
        assert!(cleaned.contains("world"), "{cleaned:?}");
        // Pin FILTER (drop) semantics, not just "the byte is gone." The shared
        // `conway_core::text::sanitize_control_chars` REPLACES a control char
        // with U+FFFD (one output char per input); this site FILTERS (drops).
        // A future reader who deduplicates this onto the shared helper would
        // keep the assertions above green while silently inflating the
        // measured display width by one column per control char (U+FFFD is
        // width 1; a control char is width 0). U+FFFD present => replace
        // slipped in; absent => filter held.
        assert!(
            !cleaned.contains('\u{FFFD}'),
            "filter semantics: ESC must be dropped, not replaced with U+FFFD: {cleaned:?}"
        );

        // A 100KB prompt must degrade to a fitting width, never panic.
        let huge = "x".repeat(100_000);
        let text = sticky_prompt_text(&huge, 40);
        assert!(text.chars().count() <= 40, "{}", text.chars().count());
    }

    /// A zero-width/zero-height terminal never panics.
    #[test]
    fn sticky_prompt_never_panics_on_a_zero_size_viewport() {
        let mut state = AppState::new(TestAgentId::new());
        state.transcript.push(Entry::User("prompt".to_string()));
        for i in 0..20 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.follow_tail = false;
        state.scroll = 10;

        let backend = ratatui::backend::TestBackend::new(1, 1);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                draw_sticky_prompt(f, Rect::new(0, 0, 0, 0), &state, &Theme::default(), 5);
            })
            .expect("draw must not panic on a zero-size rect");
    }

    /// Clean-copy invariant companion: the overlay's OWN row (the
    /// transcript viewport's top row, where [`draw_sticky_prompt`] draws --
    /// the rest of the rendered buffer legitimately contains box-drawing
    /// glyphs, e.g. the always-bordered input box) never contains a
    /// box-drawing glyph.
    #[test]
    fn sticky_prompt_overlay_adds_no_box_drawing_glyph() {
        use crate::tui::view;

        const BOX_DRAWING_CHARS: &[char] = &[
            '│', '─', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼', '║', '═', '╔', '╗', '╚', '╝',
            '╠', '╣', '╦', '╩', '╬',
        ];
        let mut state = AppState::new(TestAgentId::new());
        state.transcript.push(Entry::User("prompt".to_string()));
        for i in 0..40 {
            state.transcript.push(Entry::Assistant {
                text: format!("line {i}"),
                model: None,
                summary: None,
                ts: None,
            });
        }
        state.follow_tail = false;
        state.scroll = 30;

        let width = 60;
        let height = 10;
        let rect = Rect::new(0, 0, width, height);
        let overlay_row = view::transcript_area(&state, rect).y as usize;

        let rows = render(&state, width, height);
        let row = &rows[overlay_row];
        assert!(
            row.contains('↑'),
            "sanity: the overlay must actually be showing on its own row: {row:?}"
        );
        assert!(
            !row.chars().any(|c| BOX_DRAWING_CHARS.contains(&c)),
            "the sticky prompt overlay must never add a box-drawing glyph: {row:?}"
        );
    }

    // ---- draw_scroll_footer (unchanged from T6) ----

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
        let mut state = thirty_lines_state();
        state.follow_tail = false;
        state.scroll = 0;

        let scrolled_up = render_text(&state, 60, 8);
        assert!(scrolled_up.contains("lines above tail"), "{scrolled_up}");

        state.follow_tail = true;
        let following = render_text(&state, 60, 8);
        assert!(
            !following.contains("lines above tail"),
            "the footer must disappear once follow_tail re-engages, with no \
             transcript entry mutation at all: {following}"
        );
    }
}

//! The shared bottom-anchored, content-sized, capped modal primitive (V1).
//!
//! Before this item, Conway had five overlay/panel surfaces built to three
//! different shapes: the permission prompt, the `/ask` modal, and the NL
//! intent-confirm card each hand-rolled their own "claim nearly the whole
//! transcript area" `Rect` math (`view/mod.rs`'s three `draw_*` fns, each
//! with its own copy of the same border/`Clear`/footer-split logic); `/help`
//! (T7) copied that shape a fourth time; `/agents` is a `Layout` row, not an
//! overlay at all (see this module's own doc below on why it stays that
//! way). The permission prompt's own doc used to read *"claim nearly the
//! whole transcript area"* — which is exactly the user complaint this item
//! exists to fix: a modal that always eats the whole screen regardless of
//! how little it actually has to say.
//!
//! This module is the ONE place that decides a modal's `Rect`: **bottom-
//! anchored, sized to its own content, capped at a maximum** so a short
//! modal (a one-line command, a short `/ask` answer) renders short and a
//! long one grows only up to the cap and then **scrolls** rather than either
//! truncating silently or eating the whole screen. Every ported surface
//! ([`super::draw_permission_overlay`], [`super::draw_ask_modal`],
//! [`super::draw_intent_confirm`], [`super::help::draw`]) calls
//! [`draw_modal_frame`] for its `Rect`/border/`Clear` treatment and
//! [`body_max_scroll`]/[`clamp_scroll`] for its scroll math, so all four
//! share one sizing rule instead of four independent copies that could
//! silently drift apart.
//!
//! ## The cap
//!
//! [`DEFAULT_CAP_DENOMINATOR`] is `2` — a modal's natural height is capped at
//! `transcript_area.height / 2`, the spec's own suggested starting point.
//! `/agents`' own cap (`view/agents.rs`, `AGENT_PANEL_HEIGHT.min(area.height
//! / 3)`) was the first thing tried here, on the reasoning that it is the
//! shape the user explicitly said felt right -- but `/agents`' `/3` is
//! measured against the WHOLE frame, while a modal's cap is measured
//! against `transcript_area` (already shrunk by the input box, status line,
//! and any open agent panel), and reusing the same fraction in the smaller
//! reference frame proved too tight in practice: on an ordinary 80x24
//! terminal (`transcript_area.height` around 20 once chrome is subtracted)
//! a `/3` cap left as few as two body rows for a decision-owed modal's
//! content, forcing even a short, single-line `/ask` answer to scroll. `/2`
//! is what this module settled on after that measurement, still per-caller
//! tunable (below) so a genuinely different surface is not stuck with one
//! number.
//!
//! The cap is a **per-caller parameter**, not a global constant, precisely
//! because a permission prompt showing one short `bash` invocation and a
//! read-only reference document like `/help` (or a settings tree, V4) have
//! genuinely different natural sizes and different reasons to be on screen
//! at all: a DECISION-owed surface (the permission prompt, the `/ask`
//! modal, the intent-confirm card) interrupts the user and should stay
//! modest so the transcript above it remains visibly present, while an
//! INFORMATIONAL surface the user explicitly opened to browse (`/help`) can
//! reasonably claim more of the screen. `view/help.rs` passes its own,
//! larger cap denominator (`1`, i.e. up to the whole `transcript_area`) for
//! exactly this reason -- see that module's own doc.
//!
//! ## `/agents` stays a panel, not a modal
//!
//! The item spec calls this a judgment call and asks for a justified answer
//! either way. `/agents` stays a `Layout` row (`view/mod.rs::layout`'s
//! `show_agents` branch), not a modal on this primitive, for a reason that
//! is not just "don't break it": the panel is meant to be browsed **while
//! still composing** — it shares the screen with a live input line so you
//! can glance at the tree, arrow through it, and keep typing, none of which
//! a modal (which the item's own spec requires to sit "over the transcript,
//! never into it") can do without contradicting its own bottom-anchored-
//! over-the-transcript shape. A modal is for a **decision** (the three
//! `Mode` variants) or a **read-only reference** (`/help`) that temporarily
//! owns the screen; `/agents` is neither — it is an ambient, side-by-side
//! view, the one thing this primitive is not shaped for. What DOES carry
//! over is the *feel* `/agents` got right, per the user's own naming of it
//! as the reference: bottom-anchored, bordered, never eating the whole
//! screen -- this module's [`modal_area`] gives every ported surface that
//! same restraint, even though the exact cap fraction had to be re-measured
//! for the modal's own, smaller reference frame (see "The cap" above).
//!
//! ## Never panics on a tiny terminal
//!
//! [`modal_area`] clamps its own minimum to whatever is actually available
//! (`transcript_area.height`), all the way down to a zero-height `Rect` on a
//! genuinely 0-row transcript area — ratatui renders a zero-size `Rect` as
//! nothing, never a panic. See that function's own doc for the exact clamp
//! order and why it can never ask `u16::clamp` for an invalid `min > max`
//! range.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear};
use ratatui::Frame;

/// The default cap fraction for a DECISION-owed modal (the permission
/// prompt, the `/ask` modal, the intent-confirm card): half of
/// `transcript_area`'s own height. See this module's own doc, "The cap"
/// section, for the measurement that landed on `2` rather than the
/// `/agents`-matching `3` this module started from. Callers with a
/// genuinely different natural size (an informational surface like
/// `/help`, or V4's settings tree) pass a different `cap_denominator` to
/// [`modal_area`]/[`draw_modal_frame`] instead of this constant governing
/// every caller.
pub const DEFAULT_CAP_DENOMINATOR: u16 = 2;

/// The border rows every modal reserves (`Borders::ALL`'s top+bottom rule),
/// factored out so [`modal_area`]'s min-height math stays self-documenting.
const BORDER_ROWS: u16 = 2;

/// Computes the bottom-anchored, content-sized, capped `Rect` for a modal
/// drawn over `transcript_area`.
///
/// - `content_rows` is the body's own natural height (e.g. a wrapped line
///   count) — NOT including the border or footer rows, which this function
///   adds on top.
/// - `footer_rows` is the caller's own fixed footer height (the decision-key
///   hint, an in-modal error line, ...) — always fully reserved, even when
///   the body has to shrink for it (mirrors the pre-existing
///   `PERMISSION_FOOTER_ROWS` invariant every ported surface already
///   depended on: the footer is what you act on, so it is the LAST thing to
///   get squeezed, never the first).
/// - `cap_denominator` divides `transcript_area.height` to produce the
///   maximum total height a modal may grow to before it must scroll instead
///   of growing further ([`DEFAULT_CAP_DENOMINATOR`] is the shared default;
///   see this module's own doc for why `3`, not the spec's own suggested
///   `2`).
///
/// The returned height is `desired` (content + border + footer) clamped between
/// a floor (`BORDER_ROWS + footer_rows + 1`, i.e. "at least one row of body is
/// visible", itself never exceeding what `transcript_area` actually has) and
/// the cap (also never exceeding what's actually there). Both clamp bounds are
/// independently `.min(transcript_area.height)`, so the floor can never exceed
/// the ceiling and `u16::clamp` can never be asked for an invalid `min > max`
/// range — this is what makes the function safe against any terminal size even
/// at `transcript_area.height == 0` (both bounds collapse to `0` and the modal
/// simply renders nothing, never panicking).
pub fn modal_area(
    transcript_area: Rect,
    content_rows: u16,
    footer_rows: u16,
    cap_denominator: u16,
) -> Rect {
    let min_height = (BORDER_ROWS + footer_rows + 1).min(transcript_area.height);
    let desired = content_rows
        .saturating_add(BORDER_ROWS)
        .saturating_add(footer_rows);
    let denom = cap_denominator.max(1);
    let cap = (transcript_area.height / denom)
        .max(min_height)
        .min(transcript_area.height);
    let height = desired.clamp(min_height, cap);
    Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height.saturating_sub(height),
        width: transcript_area.width,
        height,
    }
}

/// The usable width a modal's body content wraps against — `transcript_area`
/// minus the two vertical border columns (`Borders::ALL`'s left+right rule).
/// Callers use this BEFORE calling [`modal_area`]/[`draw_modal_frame`] to
/// measure their own content's wrapped line count for `content_rows`, since
/// the modal's width is always the full transcript width regardless of its
/// (content-dependent) height.
pub fn body_width(transcript_area: Rect) -> u16 {
    transcript_area.width.saturating_sub(2)
}

/// The `Rect`s a drawn modal splits into: the whole bordered `area`, and its
/// interior split into a growing/scrolling `body_area` and a FIXED-height
/// `footer_area` pinned below it (never shrinks unless the interior itself
/// is smaller than the requested footer, in which case the footer shrinks
/// last -- see [`modal_area`]'s own doc on why the footer is reserved
/// first).
pub struct ModalFrame {
    /// Kept for future/test callers that need the whole bordered `Rect`
    /// (e.g. asserting a modal's total on-screen bounds); none of this
    /// item's four ported `draw_*` fns need it themselves -- they only ever
    /// render into `body_area`/`footer_area`.
    #[allow(dead_code)]
    pub area: Rect,
    pub body_area: Rect,
    pub footer_area: Rect,
}

/// Draws a modal's chrome over `transcript_area`: [`modal_area`]'s `Rect`,
/// then `Clear` + a bordered `Block` (title + `border_style`), then splits
/// the interior into a body/footer pair with the footer's rows reserved
/// FIRST (`Constraint::Length(footer_rows)`) so the body is what shrinks on
/// a tight viewport, never the footer. Returns the three `Rect`s so the
/// caller renders its own body `Paragraph`/`List`/whatever into them --
/// this function draws only the shared chrome, never the content, since
/// every ported surface's content (and how it computes `content_rows`) is
/// genuinely different.
pub fn draw_modal_frame(
    frame: &mut Frame,
    transcript_area: Rect,
    content_rows: u16,
    footer_rows: u16,
    cap_denominator: u16,
    title: &str,
    border_style: Style,
) -> ModalFrame {
    let area = modal_area(transcript_area, content_rows, footer_rows, cap_denominator);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let footer_rows_actual = footer_rows.min(inner.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_rows_actual)])
        .split(inner);

    ModalFrame {
        area,
        body_area: rows[0],
        footer_area: rows[1],
    }
}

/// How many rows of `content_rows` sit below what `body_height` can show at
/// once -- the modal's own scroll ceiling. `0` means the whole body already
/// fits (no scrolling needed).
pub fn body_max_scroll(content_rows: u16, body_height: u16) -> u16 {
    content_rows.saturating_sub(body_height)
}

/// Clamps a caller's stored scroll offset to `max` -- an over-large stored
/// value (e.g. left over from a previous, longer piece of content) lands on
/// the true bottom instead of scrolling past real content or panicking on
/// an out-of-range `Paragraph::scroll`.
pub fn clamp_scroll(scroll: u16, max: u16) -> u16 {
    scroll.min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(height: u16) -> Rect {
        Rect::new(0, 0, 80, height)
    }

    // ---- Acceptance: a short modal renders short; a long one grows to the
    // cap and then must scroll rather than truncate. ----

    #[test]
    fn a_short_modal_is_content_sized_not_capped() {
        let area = transcript(24);
        // 1 row of content + 2 border + 1 footer = 4, well under the cap
        // (24 / DEFAULT_CAP_DENOMINATOR == 12).
        let rect = modal_area(area, 1, 1, DEFAULT_CAP_DENOMINATOR);
        assert_eq!(rect.height, 4, "a short modal must size to its own content");
        assert!(
            rect.height < area.height / DEFAULT_CAP_DENOMINATOR,
            "a short modal must not claim the whole cap it doesn't need"
        );
    }

    #[test]
    fn a_long_modal_grows_only_to_the_cap() {
        let area = transcript(24);
        let cap = area.height / DEFAULT_CAP_DENOMINATOR;
        // 500 rows of content would demand a much taller modal than the cap
        // allows -- it must be capped, not grow to fit.
        let rect = modal_area(area, 500, 1, DEFAULT_CAP_DENOMINATOR);
        assert_eq!(
            rect.height, cap,
            "a modal whose content exceeds the cap must stop growing at the cap"
        );
        // And the scroll ceiling this implies is genuinely non-zero -- the
        // caller (each `draw_*` fn) is what turns this into actual
        // scrolling via `body_max_scroll`, proven end-to-end in
        // `view/mod.rs`'s ported-surface tests.
        let body_height = cap - BORDER_ROWS - 1; // 1 footer row
        assert!(
            body_max_scroll(500, body_height) > 0,
            "content taller than the capped body must have a positive scroll ceiling"
        );
    }

    #[test]
    fn the_cap_is_a_per_caller_parameter_not_a_shared_global() {
        let area = transcript(24);
        // A caller with a genuinely bigger natural size (e.g. V4's settings
        // tree) can pass a smaller denominator for more room, independent
        // of every OTHER caller's cap.
        let default_cap = modal_area(area, 500, 1, DEFAULT_CAP_DENOMINATOR).height;
        let bigger_cap = modal_area(area, 500, 1, 1).height;
        assert!(
            bigger_cap > default_cap,
            "cap_denominator must be independently tunable per call site"
        );
    }

    // ---- never panics, degrades gracefully on a tiny/zero terminal ----

    #[test]
    fn zero_height_transcript_area_degrades_to_a_zero_rect_without_panicking() {
        let area = transcript(0);
        let rect = modal_area(area, 500, 3, DEFAULT_CAP_DENOMINATOR);
        assert_eq!(
            rect.height, 0,
            "no room at all must degrade to nothing, not panic"
        );
    }

    #[test]
    fn a_terminal_too_small_for_the_footer_alone_still_does_not_panic() {
        // 2 rows total: not even enough for the 2 border rows plus a
        // 3-row footer -- every clamp bound must still resolve to a valid
        // (non-panicking) range.
        let area = transcript(2);
        let rect = modal_area(area, 50, 3, DEFAULT_CAP_DENOMINATOR);
        assert!(
            rect.height <= area.height,
            "must never exceed what's actually there"
        );
    }

    #[test]
    fn modal_area_never_exceeds_the_transcript_area_height_across_a_size_sweep() {
        // Sweep: no combination of a tiny terminal + huge content ever
        // produces a `Rect` taller than what's actually available.
        for h in 0..=30u16 {
            for content in [0u16, 1, 5, 50, 5000] {
                let area = transcript(h);
                let rect = modal_area(area, content, 3, DEFAULT_CAP_DENOMINATOR);
                assert!(
                    rect.height <= h,
                    "height {} exceeded transcript height {} (content={})",
                    rect.height,
                    h,
                    content
                );
                assert!(
                    rect.y >= area.y,
                    "must stay within the transcript area's own bounds"
                );
            }
        }
    }

    #[test]
    fn modal_is_bottom_anchored() {
        let area = transcript(24);
        let rect = modal_area(area, 1, 1, DEFAULT_CAP_DENOMINATOR);
        assert_eq!(
            rect.y + rect.height,
            area.y + area.height,
            "the modal's bottom edge must exactly meet the transcript area's own bottom edge"
        );
    }

    // ---- body_max_scroll / clamp_scroll ----

    #[test]
    fn body_max_scroll_is_zero_when_content_fits() {
        assert_eq!(body_max_scroll(5, 10), 0);
    }

    #[test]
    fn body_max_scroll_is_the_overflow_when_content_exceeds_the_body() {
        assert_eq!(body_max_scroll(30, 10), 20);
    }

    #[test]
    fn clamp_scroll_caps_an_over_large_stored_value() {
        assert_eq!(clamp_scroll(500, 20), 20);
        assert_eq!(clamp_scroll(5, 20), 5);
    }

    #[test]
    fn body_width_subtracts_the_two_border_columns() {
        assert_eq!(body_width(Rect::new(0, 0, 80, 24)), 78);
    }

    #[test]
    fn body_width_never_underflows_on_a_too_narrow_area() {
        assert_eq!(body_width(Rect::new(0, 0, 1, 24)), 0);
        assert_eq!(body_width(Rect::new(0, 0, 0, 24)), 0);
    }
}

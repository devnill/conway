//! The `/help` keybinding overlay (T7): a read-only cheat-sheet of every key
//! binding the TUI actually has.
//!
//! Before this item, `/help` dumped a static command list
//! (`commands.rs::HELP_LINES`, now removed) into the transcript as a pile of
//! `Entry::Notice` lines -- spamming the conversation with content that
//! already lived in the `/` command palette (`view/palette.rs::COMMANDS`),
//! and there was no keybinding reference anywhere. `/help` now opens this
//! overlay instead and pushes zero transcript entries.
//!
//! **Keybindings only.** Every genuine slash *command* (`/steer`, `/fork`,
//! `/spawn`, `/ask`, `/agents`, `/settings`, `/resume`, `/quit`, ...) stays
//! exclusively in the `/` palette -- this overlay never lists one, so the
//! two surfaces never drift into duplicating each other. V4 removed the one
//! prior exception (`/thinking`/`/timestamps`, syntactically commands but
//! functionally keyboard-driven view toggles): both are now consolidated
//! into `/settings` (`view/settings.rs`), a genuine command like any other,
//! so the "keybindings only" rule now holds with no carve-out. What DOES
//! stay documented here is the settings menu's OWN key handling (`Up`/
//! `Down`/`Enter`/`Left`/`Right`/`Esc`, live only while it's open) -- those
//! are real keybindings, not a command signature, the same distinction that
//! already earns the `/ask` modal / intent-confirm card / permission
//! prompt's own keys a "modal keys" group below.
//!
//! ** (plugin-declared TUI commands)
//! does not change this.** A plugin-declared command is a genuine slash
//! *command*, exactly like `/steer` or `/fork` -- it belongs in
//! `view/palette.rs` (which now merges the static built-in table with
//! `AppState::plugin_commands`, the installed plugin command list) alongside
//! every other command, never duplicated into this keybindings-only overlay.
//! This overlay's own footer text ("`/help` does not list slash commands;
//! see `/` for those") already covers a plugin command with zero changes
//! needed here.
//!
//! **No hotkey opens this overlay.** Conway is always in input-typing mode,
//! so a bare printable key (`?`, `F1`, ...) can never be a binding --
//! `/help` is the only way in, and `Esc` is the only way out.
//!
//! **Shape.** V1 ports this overlay onto the shared bottom-anchored,
//! content-sized, capped modal primitive (`view/modal.rs`) that the
//! permission/ask/intent-confirm overlays now also use: `Clear` + a bordered
//! `Block` drawn over the transcript area, exempt from the transcript's own
//! clean-copy guarantee (it is a modal, not conversation text -- see
//! `transcript.rs`'s module doc for that guarantee's scope). Unlike those
//! three it is not a `state::Mode` variant at all -- see
//! `AppState::help_open`'s own doc for why a plain flag (not a fourth `Mode`
//! variant) is the right shape here. Also unlike the pre-V1 shape, the
//! overlay now SCROLLS (`PageUp`/`PageDown`, sharing `AppState::modal_scroll`
//! with the other three modal-bearing surfaces -- only one of the four is
//! ever showing at a time) rather than silently clipping the binding list on
//! a small terminal.
//!
//! **Mouse wheel is deliberately absent from the keybinding rows below.**
//! Conway never calls `EnableMouseCapture` and has no `MouseEventKind`
//! handler anywhere in this crate --
//! the wheel scrolling you see in your terminal is the emulator's own
//! scrollback, not a Conway binding. Capturing the mouse would disable the
//! terminal's native click-drag text selection, the very mechanism the
//! clean-copy guarantee exists to protect, so Conway deliberately leaves it
//! uncaptured; `PageUp`/`PageDown` and `Home`/`End` are the in-app
//! equivalents. The overlay's trailing note says so in plain prose -- never
//! as a keybinding row, so a well-meaning future "mouse: scroll" row can
//! never sneak back in as if it were a real binding (`no_binding_row_mentions_mouse`
//! below guards this).
//!
//! **Transcript reservation, not just an overlay (board item
//! `01M1AFGDWR9CQ8WNYYV2B1TQBK`).** This overlay is informational and
//! commonly left open while session activity continues behind it -- exactly
//! the shape that made an appended error unreadable while `/settings`
//! covered it (fixed one item earlier, `01M1A9M2EVJNR0HBN86A8E40EA`).
//! [`modal_rect`] gives `view::mod::layout` this overlay's own height BEFORE
//! `transcript::draw` runs, so the transcript shrinks ahead of it instead of
//! being drawn over -- see [`CAP_DENOMINATOR`]'s own doc for why the cap had
//! to change alongside the reservation, not stay as it was.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::modal;
use super::theme::Theme;

/// One row: a key/chord and what it does.
struct Binding {
    keys: &'static str,
    action: &'static str,
}

/// One named group of [`Binding`]s, rendered as a bold title line followed
/// by its rows.
struct Group {
    title: &'static str,
    bindings: &'static [Binding],
}

/// The overlay's whole content, grouped exactly as this item's verified
/// binding list (enumerated from `input.rs` at HEAD, not the work item's own
/// -- stale -- spec text). Kept as one `const` so
/// [`no_binding_row_mentions_mouse`] can scan it directly, with no rendered
/// buffer needed.
const GROUPS: &[Group] = &[
    Group {
        title: "input & editing",
        bindings: &[
            Binding {
                keys: "Enter",
                action: "submit",
            },
            Binding {
                keys: "Alt-Enter / Shift-Enter",
                action: "insert a newline (both bound -- some terminals encode \
                          Shift-Enter as a plain Enter)",
            },
            Binding {
                keys: "Left / Right",
                action: "move the cursor",
            },
            Binding {
                keys: "Backspace",
                action: "delete back",
            },
            Binding {
                keys: "Ctrl-W",
                action: "delete the previous word",
            },
            Binding {
                keys: "Ctrl-D",
                action: "quit -- only when the input is empty",
            },
            Binding {
                keys: "Ctrl-C",
                action: "interrupt",
            },
        ],
    },
    Group {
        title: "history & navigation",
        bindings: &[
            Binding {
                keys: "Up / Down",
                action: "scroll the transcript one line -- or move the \
                          palette/agent-panel selection, or a multi-line \
                          draft's own lines, whichever currently owns the \
                          key. Your mouse wheel arrives here too.",
            },
            Binding {
                keys: "Ctrl-P / Ctrl-N",
                action: "recall previous/next input history",
            },
            Binding {
                keys: "Home / End",
                action: "jump the transcript to top/tail -- only when the \
                          input is empty; with text present, moves the \
                          cursor to the line's start/end",
            },
            Binding {
                keys: "PageUp / PageDown",
                action: "scroll the transcript by a page",
            },
        ],
    },
    Group {
        title: "tools & display",
        bindings: &[
            Binding {
                keys: "Ctrl-E",
                action: "expand/collapse all tool output",
            },
            Binding {
                keys: "Shift-Tab",
                action: "cycle the permission mode: prompt -> plan -> \
                          auto-allow -- the same cycle as the /settings \
                          mode row, only while typing (not while a \
                          permission prompt or other modal is up)",
            },
        ],
    },
    Group {
        title: "settings menu (only while /settings is open)",
        bindings: &[
            Binding {
                keys: "Up / Down",
                action: "move the selection",
            },
            Binding {
                keys: "Enter",
                action: "toggle a display setting, or expand/collapse a group",
            },
            Binding {
                keys: "Left / Right",
                action: "adjust the numeric setting (tool preview lines)",
            },
            Binding {
                keys: "Esc",
                action: "close the settings menu",
            },
        ],
    },
    Group {
        title: "modal keys (only while that modal is up)",
        bindings: &[
            Binding {
                keys: "/ask modal: f",
                action: "fork",
            },
            Binding {
                keys: "/ask modal: p",
                action: "pull in",
            },
            Binding {
                keys: "/ask modal: Esc",
                action: "discard",
            },
            Binding {
                keys: "intent-confirm card: Enter",
                action: "confirm",
            },
            Binding {
                keys: "intent-confirm card: e",
                action: "edit",
            },
            Binding {
                keys: "intent-confirm card: Esc",
                action: "manual",
            },
            Binding {
                keys: "permission prompt: y",
                action: "allow once",
            },
            Binding {
                keys: "permission prompt: a",
                action: "allow always",
            },
            Binding {
                keys: "permission prompt: n",
                action: "deny",
            },
            Binding {
                keys: "permission prompt: Esc",
                action: "deny with feedback",
            },
            Binding {
                keys: "permission prompt: PageUp / PageDown",
                action: "scroll the command",
            },
        ],
    },
    Group {
        title: "agent panel",
        bindings: &[
            Binding {
                keys: "v",
                action: "cycle the panel's visibility filter -- only while \
                          the panel is open",
            },
            Binding {
                keys: "Esc",
                action: "close the panel (keeping the focused agent); press \
                          again to return to the root conversation",
            },
        ],
    },
];

/// The freeform note about the mouse wheel -- prose, deliberately never a
/// [`Binding`] row (see this module's own doc).
pub(super) const MOUSE_NOTE: &str =
    "note: mouse wheel scrolling is your terminal's own scrollback, not \
                            a Conway binding -- Conway does not capture the mouse, so your \
                            terminal's native click-drag text selection keeps working. Use \
                            PageUp/PageDown or Home/End instead.";

/// Rows the overlay's footer always reserves: the `[Esc] close` hint.
const FOOTER_ROWS: u16 = 1;

/// The overlay's OWN cap denominator.
///
/// **Written for V1 as `1` ("up to the whole `transcript_area`"), corrected
/// by board item `01M1AFGDWR9CQ8WNYYV2B1TQBK`.** The `1` was sound while
/// `/help` drew straight OVER an already-rendered transcript (`Clear` cost
/// nothing the overlay wasn't already covering) -- but this item makes
/// `view::mod::layout` reserve this overlay's own height out of the
/// transcript pane BEFORE `transcript::draw` runs (see [`modal_rect`],
/// mirroring `view/settings.rs::modal_rect`'s own fix one item earlier). A
/// cap of `1` and a reservation are the same contradiction `settings.rs`'s
/// own `CAP_DENOMINATOR` doc already names: the overlay claims the WHOLE
/// pane, the transcript shrinks to nothing, and an error appended while
/// `/help` is open is unreadable again -- the exact defect the reservation
/// exists to fix. `2` (matching [`modal::DEFAULT_CAP_DENOMINATOR`] and
/// `settings.rs`'s own choice) is what this correction settles on: half the
/// transcript pane, leaving the other half visibly present above it.
///
/// **Still safe to cap, unlike a plain unscrollable `Paragraph`.** This
/// overlay's body is a `Paragraph` (not `/settings`'/`/plugin`'s stateful
/// `List`), but it is NOT scroll-state-free either: `draw` already calls
/// `Paragraph::scroll` against `scroll` (`AppState::modal_scroll`), and
/// `input.rs::handle_help_key` already wires `PageUp`/`PageDown` to it
/// (`adjust_modal_scroll`) -- a REAL, working, independent scroll offset
/// that has existed since before this item, not something this correction
/// adds. Capping only shrinks the VIEWPORT (`frame_areas.body_area.height`,
/// which `body_max_scroll` reads); the full binding list is still there to
/// scroll to, exactly the way `/settings`' `ListState` keeps rows past ITS
/// cap reachable, just via an explicit key instead of auto-follow-selection.
const CAP_DENOMINATOR: u16 = 2;

/// The overlay's own body content -- one `Paragraph`, built ONCE here so
/// [`modal_rect`] (which needs its wrapped height BEFORE anything renders)
/// and [`draw`] (which renders it) can never compute two different bodies
/// that could silently drift apart (steering P-14; mirrors `view/
/// settings.rs::build_tree` being the one tree both `modal_rect` and `draw`
/// build from).
fn build_body(theme: &Theme) -> Paragraph<'static> {
    let mut body_lines: Vec<Line> = Vec::new();
    for group in GROUPS {
        body_lines.push(Line::from(Span::styled(group.title, theme.emphasized)));
        for binding in group.bindings {
            body_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(binding.keys, theme.help_key),
                Span::raw("  -- "),
                Span::raw(binding.action),
            ]));
        }
        body_lines.push(Line::from(""));
    }
    body_lines.push(Line::from(Span::styled(MOUSE_NOTE, theme.dim)));
    Paragraph::new(body_lines).wrap(Wrap { trim: false })
}

/// The `/help` overlay's own bottom-anchored, content-sized `Rect`, computed
/// against `transcript_area` -- exactly what [`draw`] itself asks for via
/// this same function, never a second, independent computation (steering
/// P-14, mirrors `view/settings.rs::modal_rect`'s own doc).
///
/// **Board item `01M1AFGDWR9CQ8WNYYV2B1TQBK`.** Factored out so
/// `view::mod::layout` can learn how tall this overlay will render BEFORE
/// `transcript::draw` runs, and shrink the transcript pane by exactly that
/// height -- see this module's own doc, the `CAP_DENOMINATOR` correction,
/// for why a cap alone was not enough.
///
/// Takes only `transcript_area` (unlike `settings::modal_rect`, which also
/// takes `state`) because this overlay's own HEIGHT depends on nothing but
/// its wrapped line count -- see this module's own doc, "No other
/// `AppState` is needed": [`GROUPS`] is a `const`, so there is no
/// `AppState` to thread through at all. [`build_body`] does need a `Theme`
/// (for styling), but `Paragraph::line_count` measures wrapped rows from
/// TEXT alone -- style never changes how many rows a `Line` wraps to -- so
/// this passes `Theme::default()` rather than asking `view::mod::layout`
/// (which has no `Theme` of its own to give it) to thread one through just
/// for a value the row count can never actually depend on.
pub(crate) fn modal_rect(transcript_area: Rect) -> Rect {
    let body = build_body(&Theme::default());
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;
    modal::modal_area(transcript_area, content_rows, FOOTER_ROWS, CAP_DENOMINATOR)
}

/// Draws the `/help` overlay over `transcript_area` via the shared
/// [`modal`] primitive (V1): bottom-anchored, sized to the binding list's
/// own wrapped height, capped at [`CAP_DENOMINATOR`] of the transcript
/// area, and SCROLLING (`scroll`, `AppState::modal_scroll`) past the cap
/// rather than clipping. No other `AppState` is needed -- every line of
/// content here is static.
///
/// Never panics on a tiny area -- [`modal::modal_area`]'s own clamp
/// covers that; see its doc for why the floor can never exceed the ceiling.
pub fn draw(frame: &mut Frame, transcript_area: Rect, scroll: u16, theme: &Theme) {
    let body = build_body(theme);
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;
    // `modal_rect` re-derives the SAME `Rect` from the SAME `content_rows`
    // this call just computed -- never a second, independently-resolved
    // area that could disagree with what `view::mod::layout` already
    // reserved (steering P-14, mirrors `view/settings.rs::draw`'s own
    // `modal_rect`-then-`draw_modal_frame_in` shape).
    let area = modal_rect(transcript_area);

    let frame_areas = modal::draw_modal_frame_in(
        frame,
        area,
        FOOTER_ROWS,
        " HELP -- keybindings (/help does not list slash commands; see / for those) ",
        theme.help_border,
    );

    let body_max_scroll = modal::body_max_scroll(content_rows, frame_areas.body_area.height);
    let clamped_scroll = modal::clamp_scroll(scroll, body_max_scroll);
    frame.render_widget(body.scroll((clamped_scroll, 0)), frame_areas.body_area);

    let hint = if body_max_scroll > 0 {
        "[Esc] close  [PageUp/PageDown] scroll"
    } else {
        "[Esc] close"
    };
    let footer = Paragraph::new(Line::from(hint)).wrap(Wrap { trim: true });
    frame.render_widget(footer, frame_areas.footer_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No binding row may claim a MOUSE KEY, because Conway captures no
    /// mouse events -- there is no such binding to document
    /// (`EnableMouseCapture` stays off to preserve click-drag selection;
    ///).
    ///
    /// V3 narrowed this guard. It used to forbid the word "mouse" in a
    /// row's ACTION text too, on the reasoning that Conway never saw the
    /// wheel at all. That turned out to be wrong: terminals implement
    /// alternate scroll (DECSET 1007), which delivers wheel events as
    /// `Up`/`Down` cursor keys while the alternate screen is active. So the
    /// wheel really does drive a Conway binding, and the `Up`/`Down` row
    /// says so. Forbidding that would suppress a true and useful fact --
    /// the guard now protects only against inventing a mouse *key*.
    #[test]
    fn no_binding_row_claims_a_mouse_key() {
        for group in GROUPS {
            for binding in group.bindings {
                assert!(
                    !binding.keys.to_lowercase().contains("mouse"),
                    "group {:?}: binding key {:?} must not name a mouse key -- \
                     Conway captures no mouse events, so this would document a \
                     binding that does not exist",
                    group.title,
                    binding.keys
                );
            }
        }
    }
    #[test]
    fn mouse_note_exists_and_mentions_mouse() {
        assert!(MOUSE_NOTE.to_lowercase().contains("mouse"));
        assert!(MOUSE_NOTE.to_lowercase().contains("scrollback"));
    }
}

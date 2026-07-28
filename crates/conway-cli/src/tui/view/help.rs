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
//! handler anywhere in this crate (decision 01KYKDKYJEATSYXM7YS1C17HHA) --
//! the wheel scrolling you see in your terminal is the emulator's own
//! scrollback, not a Conway binding. Capturing the mouse would disable the
//! terminal's native click-drag text selection, the very mechanism the
//! clean-copy guarantee exists to protect, so Conway deliberately leaves it
//! uncaptured; `PageUp`/`PageDown` and `Home`/`End` are the in-app
//! equivalents. The overlay's trailing note says so in plain prose -- never
//! as a keybinding row, so a well-meaning future "mouse: scroll" row can
//! never sneak back in as if it were a real binding (`no_binding_row_mentions_mouse`
//! below guards this).

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
        bindings: &[Binding {
            keys: "Ctrl-E",
            action: "expand/collapse all tool output",
        }],
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
                action: "close the agent panel when open",
            },
        ],
    },
];

/// The freeform note about the mouse wheel -- prose, deliberately never a
/// [`Binding`] row (see this module's own doc).
pub(super) const MOUSE_NOTE: &str = "note: mouse wheel scrolling is your terminal's own scrollback, not \
                            a Conway binding -- Conway does not capture the mouse, so your \
                            terminal's native click-drag text selection keeps working. Use \
                            PageUp/PageDown or Home/End instead.";

/// Rows the overlay's footer always reserves: the `[Esc] close` hint.
const FOOTER_ROWS: u16 = 1;

/// The overlay's OWN cap denominator, deliberately more generous than
/// [`modal::DEFAULT_CAP_DENOMINATOR`] -- `1` means "up to the whole
/// `transcript_area`", the pre-V1 behavior this overlay already had. This is
/// the one case `modal.rs`'s own doc calls out explicitly: `/help` is
/// INFORMATIONAL (the user opened it on purpose, to read, not a decision
/// interrupting them -- `AppState::help_open`'s own doc on that
/// distinction), so it can reasonably claim more of the screen than a
/// decision-owed modal, and its binding list is genuinely long enough that
/// the tighter default would force scrolling even on an ordinary terminal.
/// It still scrolls past THIS cap on a small enough viewport (below).
const CAP_DENOMINATOR: u16 = 1;

/// Draws the `/help` overlay over `transcript_area` via the shared
/// [`modal`] primitive (V1): bottom-anchored, sized to the binding list's
/// own wrapped height, capped at [`CAP_DENOMINATOR`] of the transcript
/// area, and SCROLLING (`scroll`, `AppState::modal_scroll`) past the cap
/// rather than clipping. No other `AppState` is needed -- every line of
/// content here is static.
///
/// P-10: never panics on a tiny area -- [`modal::modal_area`]'s own clamp
/// covers that; see its doc for why the floor can never exceed the ceiling.
pub fn draw(frame: &mut Frame, transcript_area: Rect, scroll: u16, theme: &Theme) {
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
    let body = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;

    let frame_areas = modal::draw_modal_frame(
        frame,
        transcript_area,
        content_rows,
        FOOTER_ROWS,
        CAP_DENOMINATOR,
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
    /// decision 01KYKDKYJEATSYXM7YS1C17HHA).
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

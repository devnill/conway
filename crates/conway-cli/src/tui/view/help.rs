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
//! `/spawn`, `/ask`, `/agents`, `/resume`, `/quit`, ...) stays exclusively in
//! the `/` palette -- this overlay never lists one, so the two surfaces never
//! drift into duplicating each other. `/thinking` and `/timestamps` are the
//! one deliberate exception: syntactically they are slash commands, but
//! functionally they are keyboard-driven VIEW TOGGLES (on par with `Ctrl-E`,
//! not with "spawn an agent"), so they are grouped under "tools & display"
//! alongside `Ctrl-E` rather than omitted.
//!
//! **No hotkey opens this overlay.** Conway is always in input-typing mode,
//! so a bare printable key (`?`, `F1`, ...) can never be a binding --
//! `/help` is the only way in, and `Esc` is the only way out.
//!
//! **Shape.** Follows the permission/ask/intent-confirm overlays exactly
//! (`view/mod.rs`): `Clear` + a bordered `Block` drawn over the transcript
//! area, exempt from the transcript's own clean-copy guarantee (it is a
//! modal, not conversation text -- see `transcript.rs`'s module doc for that
//! guarantee's scope). Unlike those three it is not a `state::Mode` variant
//! at all -- see `AppState::help_open`'s own doc for why a plain flag (not a
//! fourth `Mode` variant) is the right shape here.
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

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

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
                keys: "/thinking",
                action: "hide/show reasoning traces",
            },
            Binding {
                keys: "/timestamps",
                action: "toggle per-entry HH:MM timestamps",
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

/// Draws the `/help` overlay over `transcript_area`, following
/// `view/mod.rs::draw_permission_overlay`'s shape: claim nearly all of the
/// transcript area, `Clear` + a bordered `Block`, footer pinned at the
/// bottom. No `AppState` needed -- every line here is static.
///
/// P-10: never panics on a tiny area. `height`/`area` are clamped exactly
/// the way the other three overlays clamp theirs, and the body `Paragraph`
/// simply clips (no scrolling, no indexing) when the content is taller than
/// what's left after the footer -- a real terminal too small for the full
/// list shows as much as fits rather than crashing.
pub fn draw(frame: &mut Frame, transcript_area: Rect, theme: &Theme) {
    // At minimum: 2 border rows + the pinned footer + one row of body.
    let min_height = (2 + FOOTER_ROWS + 1).min(transcript_area.height);
    let height = transcript_area
        .height
        .saturating_sub(1)
        .max(min_height)
        .min(transcript_area.height);
    let area = Rect {
        x: transcript_area.x,
        y: transcript_area.y + transcript_area.height.saturating_sub(height),
        width: transcript_area.width,
        height,
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" HELP -- keybindings (/help does not list slash commands; see / for those) ")
        .border_style(theme.help_border);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);

    let footer_rows = FOOTER_ROWS.min(inner.height);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(footer_rows)])
        .split(inner);
    let body_area = rows[0];
    let footer_area = rows[1];

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
    frame.render_widget(body, body_area);

    let footer = Paragraph::new(Line::from("[Esc] close")).wrap(Wrap { trim: true });
    frame.render_widget(footer, footer_area);
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

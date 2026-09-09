//! The `/help` keybinding overlay: a read-only cheat-sheet of every key
//! binding the TUI actually has.
//!
//! `/help` used to dump a static command list
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
use crate::tui::keybindings::{self, Context, ACTIONS};
use crate::tui::state::AppState;

/// One row: a key/chord and what it does.
struct Binding {
    keys: String,
    action: &'static str,
}

/// One named group of [`Binding`]s, rendered as a bold title line followed
/// by its rows.
struct Group {
    title: &'static str,
    bindings: Vec<Binding>,
}

/// One row in a [`FixedGroup`] -- the const-friendly counterpart of
/// [`Binding`] for the keys that stay hardcoded (see
/// `crate::tui::keybindings`'s own "Not (yet) rebindable" doc): both fields
/// are `&'static str`, so [`FIXED_GROUPS`] can be a plain `const`.
struct FixedBinding {
    keys: &'static str,
    action: &'static str,
}

struct FixedGroup {
    title: &'static str,
    bindings: &'static [FixedBinding],
}

/// The overlay's FIXED content -- keys `crate::tui::keybindings::ACTIONS`
/// does not cover at all (text-editing primitives, the two safety chords,
/// the ask-modal/intent-confirm/agent-panel-Esc decision keys), verified
/// against `input.rs` at HEAD (never any spec text, which can go stale).
/// [`build_groups`] appends one further, DYNAMIC group per
/// `crate::tui::keybindings::Context` after these, generated from
/// `AppState::keybindings` -- the SAME table `input.rs`'s dispatcher
/// resolves real keystrokes against -- see this module's own top-of-file
/// doc, "Keybindings only," and `crate::tui::keybindings`'s own "one
/// table" doc.
const FIXED_GROUPS: &[FixedGroup] = &[
    FixedGroup {
        title: "input & editing",
        bindings: &[
            FixedBinding {
                keys: "Enter",
                action: "submit",
            },
            FixedBinding {
                keys: "Alt-Enter / Shift-Enter",
                action: "insert a newline (both bound -- some terminals encode \
                          Shift-Enter as a plain Enter)",
            },
            FixedBinding {
                keys: "Left / Right",
                action: "move the cursor",
            },
            FixedBinding {
                keys: "Backspace",
                action: "delete back",
            },
            FixedBinding {
                keys: "Ctrl-D",
                action: "quit -- only when the input is empty",
            },
            FixedBinding {
                keys: "Ctrl-C",
                action: "interrupt",
            },
        ],
    },
    FixedGroup {
        title: "history & navigation",
        bindings: &[
            FixedBinding {
                keys: "Up / Down",
                action: "scroll the transcript one line -- or move the \
                          palette/agent-panel selection, or a multi-line \
                          draft's own lines, whichever currently owns the \
                          key. Your mouse wheel arrives here too.",
            },
            FixedBinding {
                keys: "Home / End",
                action: "jump the transcript to top/tail -- only when the \
                          input is empty; with text present, moves the \
                          cursor to the line's start/end",
            },
        ],
    },
    FixedGroup {
        title: "modal keys (only while that modal is up)",
        bindings: &[
            FixedBinding {
                keys: "/ask modal: f",
                action: "fork",
            },
            FixedBinding {
                keys: "/ask modal: p",
                action: "pull in",
            },
            FixedBinding {
                keys: "/ask modal: Esc",
                action: "discard",
            },
            FixedBinding {
                keys: "intent-confirm card: Enter",
                action: "confirm",
            },
            FixedBinding {
                keys: "intent-confirm card: e",
                action: "edit",
            },
            FixedBinding {
                keys: "intent-confirm card: Esc",
                action: "manual",
            },
        ],
    },
    FixedGroup {
        title: "agent panel",
        bindings: &[FixedBinding {
            keys: "Esc",
            action: "close the panel (keeping the focused agent); press \
                          again to return to the root conversation",
        }],
    },
];

/// The prose heading [`build_groups`] gives each DYNAMIC, keymap-driven
/// group -- one per `crate::tui::keybindings::Context`, in
/// [`Context::all`]'s own order.
fn context_title(context: Context) -> &'static str {
    match context {
        Context::Prompt => "prompt (rebindable -- keybindings.json)",
        Context::Transcript => "transcript (rebindable -- keybindings.json)",
        Context::Permission => "permission prompt (rebindable -- only while a call is pending)",
        Context::AgentsPanel => "agent panel (rebindable -- only while the panel is open)",
        Context::Palette => "command palette (rebindable -- only while `/` is showing matches)",
        Context::Settings => "settings menu (rebindable -- only while /settings is open)",
    }
}

/// [`FIXED_GROUPS`], plus one further group per
/// `crate::tui::keybindings::Context`, each listing that context's
/// [`ACTIONS`] with their CURRENT keys from `keymap` -- the EFFECTIVE
/// bindings (defaults as overridden by `keybindings.json`, if any), never
/// the bare defaults. An action rebound to `[]` (deliberately disabled, see
/// `crate::tui::keybindings::Keymap::keys_for`'s own doc) renders as
/// `(unbound)` rather than an empty string, so the row still reads as a
/// deliberate choice rather than a rendering glitch.
fn build_groups(keymap: &keybindings::Keymap) -> Vec<Group> {
    let mut groups: Vec<Group> = FIXED_GROUPS
        .iter()
        .map(|g| Group {
            title: g.title,
            bindings: g
                .bindings
                .iter()
                .map(|b| Binding {
                    keys: b.keys.to_string(),
                    action: b.action,
                })
                .collect(),
        })
        .collect();

    for context in Context::all() {
        let bindings: Vec<Binding> = ACTIONS
            .iter()
            .filter(|spec| spec.context == context)
            .map(|spec| {
                let keys = keymap.keys_for(spec.context, spec.name);
                let keys = if keys.is_empty() {
                    "(unbound)".to_string()
                } else {
                    keys.join(" / ")
                };
                Binding {
                    keys,
                    action: spec.description,
                }
            })
            .collect();
        groups.push(Group {
            title: context_title(context),
            bindings,
        });
    }

    groups
}

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
/// nothing the overlay wasn't already covering) -- but
/// `view::mod::layout` now reserves this overlay's own height out of the
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
/// that already existed, not something this correction
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
fn build_body(state: &AppState, theme: &Theme) -> Paragraph<'static> {
    // Board item `01M1YVJ4RA5V7FF95MFRQMTQW3`: `state.keybindings` is this
    // session's effective table -- defaults as overridden by
    // `keybindings.json`, if any -- the same one `input.rs`'s dispatcher
    // resolves every keystroke against.
    let groups = build_groups(&state.keybindings);
    let mut body_lines: Vec<Line> = Vec::new();
    for group in &groups {
        body_lines.push(Line::from(Span::styled(group.title, theme.emphasized)));
        for binding in &group.bindings {
            body_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(binding.keys.clone(), theme.help_key),
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
/// Takes `state` (board item `01M1YVJ4RA5V7FF95MFRQMTQW3` -- before this
/// item, it took only `transcript_area`, unlike `settings::modal_rect`,
/// which always took `state`): [`build_groups`]'s dynamic content now
/// comes from `state.keybindings`, so this overlay's own HEIGHT depends on
/// it too. [`build_body`] does need a `Theme` (for styling), but
/// `Paragraph::line_count` measures wrapped rows from TEXT alone -- style
/// never changes how many rows a `Line` wraps to -- so this passes
/// `Theme::default()` rather than asking `view::mod::layout` (which has no
/// `Theme` of its own to give it) to thread one through just for a value
/// the row count can never actually depend on.
pub(crate) fn modal_rect(state: &AppState, transcript_area: Rect) -> Rect {
    let body = build_body(state, &Theme::default());
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;
    modal::modal_area(transcript_area, content_rows, FOOTER_ROWS, CAP_DENOMINATOR)
}

/// Draws the `/help` overlay over `transcript_area` via the shared
/// [`modal`] primitive (V1): bottom-anchored, sized to the binding list's
/// own wrapped height, capped at [`CAP_DENOMINATOR`] of the transcript
/// area, and SCROLLING (`state.modal_scroll`) past the cap rather than
/// clipping. Board item `01M1YVJ4RA5V7FF95MFRQMTQW3`: takes `state` (not
/// just a bare `scroll: u16`) now that the binding list itself is
/// `state.keybindings`-dependent -- mirrors `settings::draw`'s own
/// `(frame, transcript_area, state, theme)` shape exactly.
///
/// Never panics on a tiny area -- [`modal::modal_area`]'s own clamp
/// covers that; see its doc for why the floor can never exceed the ceiling.
pub fn draw(frame: &mut Frame, transcript_area: Rect, state: &AppState, theme: &Theme) {
    let body = build_body(state, theme);
    let content_rows = body
        .line_count(modal::body_width(transcript_area))
        .min(u16::MAX as usize) as u16;
    // `modal_rect` re-derives the SAME `Rect` from the SAME `content_rows`
    // this call just computed -- never a second, independently-resolved
    // area that could disagree with what `view::mod::layout` already
    // reserved (steering P-14, mirrors `view/settings.rs::draw`'s own
    // `modal_rect`-then-`draw_modal_frame_in` shape).
    let area = modal_rect(state, transcript_area);

    let frame_areas = modal::draw_modal_frame_in(
        frame,
        area,
        FOOTER_ROWS,
        " HELP -- keybindings (/help does not list slash commands; see / for those) ",
        theme.help_border,
    );

    let body_max_scroll = modal::body_max_scroll(content_rows, frame_areas.body_area.height);
    let clamped_scroll = modal::clamp_scroll(state.modal_scroll, body_max_scroll);
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
        let groups = build_groups(&keybindings::Keymap::defaults());
        for group in &groups {
            for binding in &group.bindings {
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

    /// Every default action's default key(s) show up under plain
    /// [`keybindings::Keymap::defaults`] -- proves [`build_groups`] is
    /// really reading `crate::tui::keybindings::ACTIONS`, not a stale
    /// second copy.
    #[test]
    fn every_default_action_and_key_appears_in_the_built_groups() {
        let groups = build_groups(&keybindings::Keymap::defaults());
        let rendered: Vec<&str> = groups
            .iter()
            .flat_map(|g| g.bindings.iter().map(|b| b.keys.as_str()))
            .collect();
        for spec in ACTIONS {
            for default_key in spec.defaults.iter().copied() {
                assert!(
                    rendered.iter().any(|k| k.contains(default_key)),
                    "default key {default_key:?} for {}.{} must appear in the rendered \
                     groups -- got {rendered:?}",
                    spec.context.key(),
                    spec.name
                );
            }
        }
    }

    /// Acceptance check (b) ("shows in `/help`"): a `keybindings.json`
    /// rebind changes what [`build_groups`] renders for that action's key,
    /// and the OLD default key no longer appears for it -- proves `/help`
    /// shows the EFFECTIVE bindings, not the compiled-in defaults, when fed
    /// a non-default [`keybindings::Keymap`].
    #[test]
    fn build_groups_reflects_a_rebind_not_the_compiled_in_default() {
        // `Keymap::load` is the only public constructor for a non-default
        // table, so this round-trips through a real temp file exactly like
        // `keybindings.rs`'s own tests do, rather than poking private
        // fields.
        let path = std::env::temp_dir().join(format!(
            "conway-help-test-rebind-{}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"transcript": {"toggle_tool_output": ["Ctrl-O"]}}"#,
        )
        .expect("write must succeed");
        let keymap = keybindings::Keymap::load(&path).expect("a valid rebind must load");
        let _ = std::fs::remove_file(&path);

        let groups = build_groups(&keymap);
        let transcript_group = groups
            .iter()
            .find(|g| g.title.starts_with("transcript"))
            .expect("a transcript group must exist");
        let toggle_row = transcript_group
            .bindings
            .iter()
            .zip(ACTIONS.iter().filter(|a| a.context == Context::Transcript))
            .find(|(_, spec)| spec.name == "toggle_tool_output")
            .map(|(binding, _)| binding)
            .expect("toggle_tool_output must be one of the transcript rows");

        assert_eq!(toggle_row.keys, "Ctrl-O", "must show the rebound key");
        assert_ne!(
            toggle_row.keys, "Ctrl-E",
            "must NOT show the compiled-in default once rebound"
        );
    }
}

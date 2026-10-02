//! The `/help` overlay: a read-only cheat-sheet of every key binding the
//! TUI has, PLUS (board item `01M1YVH1X49WYSQ9C2Z4D6B4XM`, "B3d") the full
//! command list.
//!
//! `/help` used to dump a static command list
//! (`commands.rs::HELP_LINES`, now removed) into the transcript as a pile of
//! `Entry::Notice` lines -- spamming the conversation with content that
//! already lived in the `/` command palette (`view/palette.rs::COMMANDS`),
//! and there was no keybinding reference anywhere. `/help` opened this
//! keybindings-only overlay instead, and pushed zero transcript entries.
//!
//! **"`/help` does not list the slash commands" was GUIDE.md's own first
//! warning to a new operator -- board item `01M1YVH1X49WYSQ9C2Z4D6B4XM`
//! closes it.** A pure keybinding reference that cannot tell an operator
//! which commands exist at all forces a second lookup (GUIDE.md, or trial
//! and error at the `/` palette) for the single most basic question
//! ("what can I even type?"). [`COMMANDS_SECTION_TITLE`] now heads a second
//! section, built from [`command_rows`] -- **the identical source the `/`
//! palette itself reads** (`view::palette::matches` with the catch-all
//! `"/"` prefix, which the palette's own three-tier ranking collapses back
//! to declaration order for an empty fragment -- see that function's own
//! doc). There is no second, hand-kept command table anywhere in this
//! module: `tests::help_command_list_matches_the_palette_list` is the
//! direct proof the two can never drift, the same guarantee
//! `view/palette.rs`'s own module doc already describes for ITS built-in
//! half (`commands::builtin_commands()`). A plugin command (`AppState::
//! plugin_commands`) is covered with zero extra code here, for the
//! identical reason: it is already IN the source this section reads.
//!
//! **Keybindings are still the FIRST section, unchanged in shape.** Every
//! genuine slash *command* used to live exclusively in the `/` palette, on
//! the theory that listing it here too would be the exact duplication this
//! module's own history (the T7 removal, immediately below) already paid
//! down once. That theory held right up until a brand-new operator's first
//! minute proved it wrong: GUIDE.md had to carry its own disclaimer, and an
//! operator who had not yet read GUIDE.md had no way to discover `/` at
//! all except by already knowing to type it. The keybinding section itself
//! needs no change for this -- V4's prior history (immediately below)
//! still holds for IT.
//!
//! V4 removed the one prior "commands here too" exception
//! (`/thinking`/`/timestamps`, syntactically commands but functionally
//! keyboard-driven view toggles): both are now consolidated into
//! `/settings` (`view/settings.rs`), a genuine command like any other. What
//! DOES stay documented in the KEYBINDINGS section is the settings menu's
//! OWN key handling (`Up`/`Down`/`Enter`/`Left`/`Right`/`Esc`, live only
//! while it's open) -- those are real keybindings, not a command
//! signature, the same distinction that already earns the `/ask` modal /
//! intent-confirm card / permission prompt's own keys a "modal keys" group
//! below.
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
use super::palette;
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
        Context::Mentions => {
            "mention completion (rebindable -- only while an @-mention list is open)"
        }
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

/// The heading over the KEYBINDINGS section -- [`build_body`]'s first
/// section, unchanged in content from before board item
/// `01M1YVH1X49WYSQ9C2Z4D6B4XM`, now explicitly titled since a second
/// section exists to tell it apart from.
const KEYBINDINGS_SECTION_TITLE: &str = "KEYBINDINGS";

/// The heading over the COMMANDS section (board item
/// `01M1YVH1X49WYSQ9C2Z4D6B4XM`, "B3d") -- see this module's own top-of-file
/// doc for why this section exists and why it can never drift from the `/`
/// palette.
const COMMANDS_SECTION_TITLE: &str =
    "COMMANDS (see also the / palette, which filters this same list live)";

/// The full command list `/help`'s COMMANDS section renders -- built-in
/// commands THEN every installed plugin command, in the SAME order the `/`
/// palette itself would show them for a bare `/` (no filter at all).
///
/// **The one and only reason this exists as its own function, rather than
/// inlining the call in [`build_body`]:** `tests::
/// help_command_list_matches_the_palette_list` needs to call the identical
/// thing [`build_body`] does, to prove the two can never drift -- see this
/// module's own top-of-file doc.
pub(crate) fn command_rows(state: &AppState) -> Vec<palette::PaletteRow<'_>> {
    // The catch-all `"/"` fragment: `palette::matches`'s own three-tier
    // ranking collapses to plain declaration order for an empty fragment
    // (tier 1, `starts_with("")`, always matches -- see that function's own
    // doc), so this is exactly "every command, unfiltered."
    palette::matches("/", &state.plugin_commands)
}

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
    body_lines.push(Line::from(Span::styled(
        KEYBINDINGS_SECTION_TITLE,
        theme.emphasized,
    )));
    body_lines.push(Line::from(""));
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
    body_lines.push(Line::from(Span::styled(
        COMMANDS_SECTION_TITLE,
        theme.emphasized,
    )));
    body_lines.push(Line::from(""));
    for row in command_rows(state) {
        body_lines.push(Line::from(vec![
            Span::styled(row.usage.to_string(), theme.help_key),
            Span::raw("  -- "),
            Span::raw(row.description.to_string()),
        ]));
    }
    body_lines.push(Line::from(""));
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
        " HELP -- keybindings and commands ",
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
            r#"{"transcript": {"toggle_tool_output": ["Ctrl-Z"]}}"#,
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

        // Ctrl-Z, not Ctrl-O: Ctrl-O is now the compiled-in default, so a
        // rebind to it could not tell "rebind honoured" from "rebind ignored".
        assert_eq!(toggle_row.keys, "Ctrl-Z", "must show the rebound key");
        assert_ne!(
            toggle_row.keys, "Ctrl-O",
            "must NOT show the compiled-in default once rebound"
        );
    }

    // ---- board item `01M1YVH1X49WYSQ9C2Z4D6B4XM`: the COMMANDS section ----

    use crate::tui::commands;
    use crate::tui::state::PluginCommandEntry;
    use crate::tui::test_support::render_text;
    use conway::AgentId;

    /// Renders every PAGE of the `/help` overlay (`PageDown`-equivalent
    /// scroll steps), concatenating them -- mirrors `view::mod::tests::
    /// render_help_all_pages` (private to that module, so this is a local
    /// copy rather than a cross-module reuse) for the identical reason that
    /// one exists: the overlay's own `CAP_DENOMINATOR` means it legitimately
    /// scrolls at ordinary sizes, so a single `render_text` call is not a
    /// faithful proof that a given string appears SOMEWHERE in the overlay.
    fn render_help_all_pages(state: &mut AppState, width: u16, height: u16) -> String {
        let mut combined = String::new();
        let mut previous: Option<String> = None;
        for _ in 0..200 {
            let page = render_text(state, width, height);
            combined.push_str(&page);
            combined.push('\n');
            if previous.as_deref() == Some(page.as_str()) {
                break;
            }
            previous = Some(page);
            state.modal_scroll = state.modal_scroll.saturating_add(5);
        }
        combined
    }

    /// `/help`'s command list is LITERALLY the `/` palette's own list for a
    /// bare `/`, not a second hand-kept copy that could silently drift from
    /// it.
    #[test]
    fn help_command_list_matches_the_palette_list() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_commands = std::sync::Arc::new(vec![PluginCommandEntry {
            name: "/acme.greet".to_string(),
            description: "greets the operator".to_string(),
        }]);

        let help_rows = command_rows(&state);
        let palette_rows = palette::matches("/", &state.plugin_commands);
        assert_eq!(
            help_rows, palette_rows,
            "/help's command list must be exactly the palette's own list"
        );
    }

    /// Acceptance (1): `/help` lists every command, including a plugin's
    /// own, when that plugin is installed -- `/conway.memory.list` stands
    /// in for any installed plugin command (the real plugin is not
    /// installed in this unit test's fixture; the mechanism is identical
    /// for any name).
    #[test]
    fn help_overlay_lists_a_plugin_command_when_installed() {
        let mut state = AppState::new(AgentId::new());
        state.plugin_commands = std::sync::Arc::new(vec![PluginCommandEntry {
            name: "/conway.memory.list".to_string(),
            description: "list everything conway.memory remembers".to_string(),
        }]);
        state.open_help();

        let text = render_help_all_pages(&mut state, 100, 80);
        assert!(
            text.contains("/conway.memory.list"),
            "the installed plugin command must appear in /help: {text}"
        );
        assert!(
            text.contains("list everything conway.memory remembers"),
            "its description must appear too: {text}"
        );
    }

    /// Every built-in command's name shows up in the rendered overlay --
    /// the direct, render-level companion to
    /// `help_command_list_matches_the_palette_list`.
    #[test]
    fn help_overlay_lists_every_builtin_command() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        let text = render_help_all_pages(&mut state, 100, 80);
        for spec in commands::builtin_commands() {
            assert!(
                text.contains(spec.name),
                "{} must appear in /help's command list: {text}",
                spec.name
            );
        }
    }

    /// The COMMANDS section heading itself must be present, and the stale
    /// "`/help` does not list slash commands" footer text must be gone.
    #[test]
    fn help_overlay_shows_the_commands_heading_not_the_old_disclaimer() {
        let mut state = AppState::new(AgentId::new());
        state.open_help();

        let text = render_help_all_pages(&mut state, 100, 80);
        assert!(text.contains("COMMANDS"), "{text}");
        assert!(
            !text.contains("does not list"),
            "the retired disclaimer must not still be rendered: {text}"
        );
    }
}

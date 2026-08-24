//! The slash-command palette (criterion 3): a pure prefix-filter over the
//! commands `commands.rs` knows how to describe. Typing `/` in the input
//! line shows every command; each further character narrows the list live,
//! since `matches` (the function in this module, not the std macro) is
//! called fresh on every render off the live
//! `AppState::input` -- there is no separate "palette is open" flag to fall
//! out of sync.
//!
//! **No more disclosed duplication (board item
//! `01M0RW29F2ATVGCV0R8H0GQEYH`).** This module used to hand-keep its own
//! `COMMANDS` table, independent of `commands.rs`'s own `SlashCommand`
//! parser -- and it drifted: `/trust` and `/tree` were real, working
//! commands absent from that table, found only once an operator hit the
//! gap. The table is gone; [`matches()`] now builds its built-in half from
//! `commands::builtin_commands()`, generated from an exhaustive `match` over
//! `SlashCommand` with no catch-all arm (`commands::describe`'s own doc
//! argues why that construction, not a second hand-kept table, is what
//! makes the drift impossible to reintroduce). `/ask` and `/agents` are no
//! longer a special case here either: board item `01KZVZ5XV162XCQR96AQKCCCF7`
//! already made both ordinary `SlashCommand` variants reached through
//! `commands::parse` like any other command, so they are described through
//! the identical mechanism as `/steer` or `/trust`, not a separate listing.
//!
//! T7 removed `commands.rs`'s OWN second listing (`HELP_LINES`, formerly
//! dumped into the transcript by `/help`) entirely rather than reconciling
//! it with this one -- `/help` now opens a keybinding-only overlay
//! (`view/help.rs`) that never lists a slash command at all.
//!
//! **Plugin commands are the one
//! entry this module does NOT derive from `commands::builtin_commands()`.**
//! They cannot be: which commands exist is resolved at TUI startup from
//! whichever plugins were installed, not known at compile time.
//! [`matches()`]/[`draw_overlay`] both take an additional
//! `plugin_commands: &[PluginCommandEntry]` slice (`AppState::
//! plugin_commands`, built once by `commands::CommandRegistry::
//! palette_entries`) and merge it in at call time, AFTER the built-ins --
//! so a plugin command is discoverable through the exact SAME surface a
//! built-in is, and `/help`'s own "see / for those" pointer (`view/help.rs`)
//! covers it too, with no separate listing to keep in sync.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};
use ratatui::Frame;

use crate::tui::commands;
use crate::tui::state::PluginCommandEntry;

/// One palette row, borrowed uniformly from either a built-in
/// [`commands::CommandSpec`] (`'static`) or a caller's `plugin_commands`
/// slice -- [`matches()`]'s own return type, so a caller never has to case
/// on where a row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaletteRow<'a> {
    pub name: &'a str,
    pub usage: &'a str,
    pub description: &'a str,
}

/// Every command (built-in, from `commands::builtin_commands()`, THEN every
/// installed plugin command, from `plugin_commands`, in that order) whose
/// name starts with `input`. Empty for input not starting with `/` (module
/// notes: the caller only shows the palette at all when `AppState::input`
/// starts with `/`, but `matches` is total over any `&str` so it never
/// panics if called otherwise).
pub fn matches<'a>(input: &str, plugin_commands: &'a [PluginCommandEntry]) -> Vec<PaletteRow<'a>> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    let builtins = commands::builtin_commands()
        .into_iter()
        .filter(|c| c.name.starts_with(input))
        .map(|c| PaletteRow {
            name: c.name,
            usage: c.usage,
            description: c.description,
        });
    let plugins = plugin_commands
        .iter()
        .filter(|c| c.name.starts_with(input))
        .map(|c| PaletteRow {
            name: &c.name,
            // A plugin command declares only a name + one-line summary
            // (`conway::plugin::CommandSpec`'s own doc: deliberately no
            // separate usage-shape field) -- its own name doubles as its
            // usage form, exactly like a bare built-in (`/help`, `/quit`)
            // already does above.
            usage: &c.name,
            description: &c.description,
        });
    builtins.chain(plugins).collect()
}

/// Draws the live-filtered palette as a floating list directly above
/// `input_area`. A `Block`/border here is fine -- criterion 2's clean-copy
/// guarantee is about the conversation stream (`transcript.rs`), not this
/// on-demand overlay, which never contains conversation content.
///
/// `stem` is the text the match list is anchored to (see
/// [`crate::tui::state::AppState::palette_source`]); `selected`, when
/// `Some(i)`, is the arrow-navigated row to highlight -- clamped here so a
/// shrinking match list can never index out of range.
pub fn draw_overlay(
    frame: &mut Frame,
    input_area: Rect,
    stem: &str,
    selected: Option<usize>,
    plugin_commands: &[PluginCommandEntry],
) {
    let candidates = matches(stem, plugin_commands);
    if candidates.is_empty() {
        return;
    }
    let selected = selected.map(|i| i.min(candidates.len() - 1));
    // Bounded by both a sane maximum and the room actually available above
    // the input box (`input_area.y` is exactly that: everything above it is
    // the transcript, and an optional agent panel). Below 3 rows there is
    // not enough room for a border plus one item, so skip entirely rather
    // than draw a degenerate box.
    let desired = (candidates.len() as u16 + 2).min(10);
    let height = desired.min(input_area.y);
    if height < 3 {
        return;
    }
    let area = Rect {
        x: input_area.x,
        y: input_area.y - height,
        width: input_area.width,
        height,
    };

    let items: Vec<ListItem> = candidates
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let line = Line::from(vec![
                Span::styled(c.usage, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::raw(c.description),
            ]);
            let item = ListItem::new(line);
            if selected == Some(i) {
                // The arrow-highlighted row: reversed so it reads as
                // "this is what Enter/autofill has selected", matching the
                // agent panel's own selection style.
                item.style(Style::default().add_modifier(Modifier::REVERSED))
            } else {
                item
            }
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("commands (↑/↓ select)"),
    );
    frame.render_widget(Clear, area);
    frame.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_prefix_filters_to_ask_only() {
        let found = matches("/as", &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/ask");
    }

    #[test]
    fn slash_alone_lists_every_command() {
        assert_eq!(matches("/", &[]).len(), commands::builtin_commands().len());
    }

    #[test]
    fn a_prefix_matches_both_ask_and_agents() {
        let names: Vec<&str> = matches("/a", &[]).iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/ask", "/agents"]);
    }

    #[test]
    fn unknown_prefix_yields_no_matches() {
        assert!(matches("/zzz", &[]).is_empty());
    }

    /// Board item `01M0RW29F2ATVGCV0R8H0GQEYH`: Item A3 had demoted `/tree`
    /// to a hidden alias -- it parsed (`commands.rs` kept the arm) but
    /// never showed up as completion, which is exactly the "advertised
    /// nowhere, works anyway" defect this item exists to close. `/tree` is
    /// now an ordinary discoverable entry, generated the same way as every
    /// other built-in.
    #[test]
    fn tree_is_now_a_discoverable_palette_entry() {
        let found = matches("/tree", &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/tree");
    }

    /// `/trust` -- the operator's own hit -- must also be discoverable.
    #[test]
    fn trust_is_a_discoverable_palette_entry() {
        let found = matches("/trust", &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/trust");
    }

    /// V4: `/thinking` and `/timestamps` are REMOVED, not aliased --
    /// `/settings` replaces both. Neither name appears in the palette at
    /// all any more (T4 had added them; V4 retired them together with the
    /// standalone commands they backed) -- `commands::builtin_commands()`
    /// has no `SlashCommand` variant to generate a row from, since `parse`
    /// itself no longer recognizes either word.
    #[test]
    fn thinking_and_timestamps_are_gone_from_the_palette() {
        assert!(!commands::builtin_commands()
            .iter()
            .any(|c| c.name == "/thinking"));
        assert!(!commands::builtin_commands()
            .iter()
            .any(|c| c.name == "/timestamps"));
        assert!(matches("/thinking", &[]).is_empty());
        assert!(matches("/timestamps", &[]).is_empty());
    }

    #[test]
    fn non_slash_input_yields_no_matches() {
        assert!(matches("hello", &[]).is_empty());
        assert!(matches("", &[]).is_empty());
    }

    #[test]
    fn exact_command_name_still_matches_itself() {
        let found = matches("/quit", &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/quit");
    }

    #[test]
    fn exit_is_listed_as_an_alias_for_quit() {
        let found = matches("/exit", &[]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/exit");
        assert!(found[0].description.contains("/quit"));
    }

    // Review finding M1: render-layer coverage of the arrow selection.
    #[test]
    fn draw_overlay_renders_the_selected_row_reversed_and_nothing_otherwise() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        // Input box near the bottom so the overlay has room above it.
        let input_area = Rect {
            x: 0,
            y: 7,
            width: 40,
            height: 3,
        };
        let any_reversed = |selected: Option<usize>| {
            let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
            terminal
                .draw(|f| draw_overlay(f, input_area, "/a", selected, &[]))
                .unwrap();
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|c| c.modifier.contains(Modifier::REVERSED))
        };

        // "/a" matches [/ask, /agents]; selecting a row highlights it...
        assert!(
            any_reversed(Some(1)),
            "the selected palette row must render reversed"
        );
        // ...and with no selection, nothing is reversed.
        assert!(
            !any_reversed(None),
            "no row should be reversed when nothing is selected"
        );
    }

    // ---- plugin commands ----

    fn fixture_plugin_commands() -> Vec<PluginCommandEntry> {
        vec![PluginCommandEntry {
            name: "/acme.greet".to_string(),
            description: "greets the operator".to_string(),
        }]
    }

    #[test]
    fn plugin_commands_appear_in_the_palette_alongside_builtins() {
        let plugins = fixture_plugin_commands();
        let found = matches("/", &plugins);
        assert_eq!(found.len(), commands::builtin_commands().len() + 1);
        assert!(found.iter().any(|c| c.name == "/acme.greet"));
    }

    #[test]
    fn plugin_command_prefix_filters_correctly() {
        let plugins = fixture_plugin_commands();
        let found = matches("/acme", &plugins);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/acme.greet");
        assert_eq!(found[0].description, "greets the operator");
    }

    /// The verification anchor's own negative half, restated at the palette
    /// layer: with no plugin commands supplied, nothing plugin-shaped
    /// appears -- proves the merge is additive, never conjuring an entry
    /// from nowhere.
    #[test]
    fn no_plugin_commands_means_no_plugin_rows() {
        assert_eq!(matches("/", &[]).len(), commands::builtin_commands().len());
        assert!(matches("/acme", &[]).is_empty());
    }
}

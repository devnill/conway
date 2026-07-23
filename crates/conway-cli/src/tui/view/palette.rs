//! The slash-command palette (WI-127 criterion 3): a static, hand-kept
//! command table plus a pure prefix-filter over it. Typing `/` in the input
//! line shows every command; each further character narrows the list live,
//! since [`matches`] is called fresh on every render off the live
//! `AppState::input` -- there is no separate "palette is open" flag to fall
//! out of sync.
//!
//! **Disclosed duplication:** `commands.rs` (out of this item's file scope
//! -- see the work item's file-scope note) owns the authoritative
//! `SlashCommand` parser and its own `HELP_LINES` table. This table is a
//! second, independent listing so the palette can discover `/ask` and
//! `/agents` (handled directly in `app.rs`, never reaching `commands.rs` --
//! see that module's `submit` doc) alongside the commands `commands.rs`
//! already parses. A future change to either table can drift from the
//! other; unifying them requires touching `commands.rs`, which is out of
//! scope here.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};
use ratatui::Frame;

/// One palette entry: a command name, its usage form, and a one-line
/// description.
pub struct CommandSpec {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

/// Every slash command the palette can surface -- the commands
/// `commands.rs` parses, plus `/ask` and `/agents` (handled inline in
/// `app.rs::submit`). Order here is display order.
pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "/ask",
        usage: "/ask <text>",
        description: "ask an ephemeral fork a question (does not affect the live session)",
    },
    CommandSpec {
        name: "/agents",
        usage: "/agents",
        description: "toggle the below-chat agent-tree panel",
    },
    CommandSpec {
        name: "/steer",
        usage: "/steer <agent> <text>",
        description: "send a steer message to a running agent",
    },
    CommandSpec {
        name: "/tree",
        usage: "/tree",
        description: "show the whole agent tree",
    },
    CommandSpec {
        name: "/context",
        usage: "/context <agent>",
        description: "show an agent's assembled context",
    },
    CommandSpec {
        name: "/why",
        usage: "/why",
        description: "show the last routing decision",
    },
    CommandSpec {
        name: "/fork",
        usage: "/fork <agent> <directive>",
        description: "fork a live agent with a directive",
    },
    CommandSpec {
        name: "/spawn",
        usage: "/spawn <agent_def> <prompt>",
        description: "spawn a fresh agent",
    },
    CommandSpec {
        name: "/resume",
        usage: "/resume <session-id>",
        description: "resume a prior session",
    },
    CommandSpec {
        name: "/help",
        usage: "/help",
        description: "show this help",
    },
    CommandSpec {
        name: "/quit",
        usage: "/quit",
        description: "exit",
    },
];

/// Every [`CommandSpec`] whose name starts with `input`, in [`COMMANDS`]
/// order. Empty for input not starting with `/` (module notes: the caller
/// only shows the palette at all when `AppState::input` starts with `/`,
/// but `matches` is total over any `&str` so it never panics if called
/// otherwise).
pub fn matches(input: &str) -> Vec<&'static CommandSpec> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(input))
        .collect()
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
pub fn draw_overlay(frame: &mut Frame, input_area: Rect, stem: &str, selected: Option<usize>) {
    let candidates = matches(stem);
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
                // The arrow-highlighted row (WI-130): reversed so it reads as
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
        let found = matches("/as");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/ask");
    }

    #[test]
    fn slash_alone_lists_every_command() {
        assert_eq!(matches("/").len(), COMMANDS.len());
    }

    #[test]
    fn a_prefix_matches_both_ask_and_agents() {
        let names: Vec<&str> = matches("/a").iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/ask", "/agents"]);
    }

    #[test]
    fn unknown_prefix_yields_no_matches() {
        assert!(matches("/zzz").is_empty());
    }

    #[test]
    fn non_slash_input_yields_no_matches() {
        assert!(matches("hello").is_empty());
        assert!(matches("").is_empty());
    }

    #[test]
    fn exact_command_name_still_matches_itself() {
        let found = matches("/quit");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/quit");
    }
}

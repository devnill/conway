//! The `@`-mention completion overlay (board item
//! `01M1YVF4X864GKSGZM4PSCTMEH`): a pure-rendering widget over
//! [`AppState::mention_matches`], drawn directly above the input box --
//! the exact same positioning/shape `palette.rs::draw_overlay` already
//! uses for the slash palette, since the two are mutually exclusive on
//! screen (see `view/mod.rs::draw`'s own call site).

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};
use ratatui::Frame;

use crate::tui::state::AppState;

/// Draws the live-filtered mention list, if one is open
/// ([`AppState::mention_open`]) -- a no-op otherwise. Review finding
/// (CRITICAL, round 1): a Path-mode mention's candidates can genuinely be
/// empty while a background walk is still in flight
/// ([`AppState::mention_scan_pending`]) -- that case draws a one-line
/// "scanning..." placeholder rather than either silently drawing nothing
/// (which would read as "no matches" when the true answer has not arrived
/// yet) or blocking to wait for it. Zero candidates with NO scan pending
/// (a fragment genuinely matches nothing) still draws nothing, mirroring
/// `palette::draw_overlay`'s own "nothing to show" early-out. The title
/// names the candidate kind (`files`/`agents`) and says when the underlying
/// walk was capped, so a truncated listing never looks complete.
pub fn draw_overlay(frame: &mut Frame, input_area: Rect, state: &AppState) {
    if !state.mention_open() {
        return;
    }
    let candidates = state.mention_matches();
    let scanning = candidates.is_empty() && state.mention_scan_pending();
    if candidates.is_empty() && !scanning {
        return;
    }
    // Reserve exactly one row for the "scanning..." placeholder when there
    // are no real candidates yet -- `candidates.len() - 1` below would
    // otherwise underflow a `usize` on an empty list.
    let selected = if candidates.is_empty() {
        None
    } else {
        state.mention_selected.map(|i| i.min(candidates.len() - 1))
    };
    let row_count = candidates.len().max(1) as u16;
    let desired = (row_count + 2).min(10);
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

    let items: Vec<ListItem> = if scanning {
        vec![ListItem::new(Line::from("scanning..."))
            .style(Style::default().add_modifier(Modifier::DIM))]
    } else {
        candidates
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let item = ListItem::new(Line::from(*c));
                if selected == Some(i) {
                    item.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    item
                }
            })
            .collect()
    };
    let kind = match state.mention_mode_for_display() {
        crate::tui::mentions::MentionMode::Path => "files",
        crate::tui::mentions::MentionMode::Agent => "agents",
    };
    let title = if scanning {
        format!("{kind} (scanning...)")
    } else if state.mention_capped() {
        format!("{kind} (↑/↓ select, capped)")
    } else {
        format!("{kind} (↑/↓ select)")
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(Clear, area);
    frame.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use conway::AgentId;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn tempdir(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "conway-view-mentions-test-{}-{}-{name}",
            std::process::id(),
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static COUNTER: AtomicU64 = AtomicU64::new(0);
                COUNTER.fetch_add(1, Ordering::Relaxed)
            }
        ));
        std::fs::create_dir_all(&path).expect("tempdir must be creatable");
        path
    }

    fn input_area() -> Rect {
        Rect {
            x: 0,
            y: 7,
            width: 40,
            height: 3,
        }
    }

    #[test]
    fn closed_mention_draws_nothing() {
        let state = AppState::new(AgentId::new());
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| draw_overlay(f, input_area(), &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.trim().is_empty());
    }

    #[test]
    fn an_open_mention_renders_its_candidates_and_the_selected_row_reversed() {
        let root = tempdir("render");
        std::fs::write(root.join("main.rs"), "fn main() {}").unwrap();
        let mut state = AppState::new(AgentId::new());
        state.mention_scan_root = root.clone();
        state.input = "@ma".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        let request = state
            .take_pending_mention_scan_request()
            .expect("a cache-miss mention must queue a scan request");
        let scan =
            crate::tui::mentions::scan_paths(&request.root, crate::tui::mentions::DEFAULT_SCAN_CAP);
        state.apply_mention_scan_result(request.anchor, request.root, scan);

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| draw_overlay(f, input_area(), &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("main.rs"), "{text}");
        assert!(
            !terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|c| c.modifier.contains(Modifier::REVERSED)),
            "nothing is reversed with no arrow-selection yet"
        );

        state.mention_selected = Some(0);
        terminal
            .draw(|f| draw_overlay(f, input_area(), &state))
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .any(|c| c.modifier.contains(Modifier::REVERSED)),
            "the selected row must render reversed"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_capped_scan_says_so_in_the_title() {
        let mut state = AppState::new(AgentId::new());
        state.input = "@x".to_string();
        state.cursor = state.input.chars().count();
        state.set_mention_state_for_test(
            crate::tui::mentions::MentionMode::Path,
            vec!["xray.rs".to_string()],
            true,
        );

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| draw_overlay(f, input_area(), &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("capped"), "{text}");
    }

    #[test]
    fn an_uncapped_scan_omits_the_word_capped() {
        let mut state = AppState::new(AgentId::new());
        state.input = "@x".to_string();
        state.cursor = state.input.chars().count();
        state.set_mention_state_for_test(
            crate::tui::mentions::MentionMode::Path,
            vec!["xray.rs".to_string()],
            false,
        );

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| draw_overlay(f, input_area(), &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(!text.contains("capped"), "{text}");
    }

    /// Review finding (CRITICAL, round 1): a fresh `@`-mention with its
    /// background scan still in flight renders a "scanning..." placeholder
    /// -- never a blank overlay (which would misleadingly read as "no
    /// matches") and never a panic from an empty candidate list.
    #[test]
    fn a_pending_scan_renders_a_scanning_placeholder() {
        let mut state = AppState::new(AgentId::new());
        state.input = "@ma".to_string();
        state.cursor = state.input.chars().count();
        state.sync_mention(false);
        assert!(state.mention_scan_pending());
        assert!(state.mention_matches().is_empty());

        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|f| draw_overlay(f, input_area(), &state))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("scanning"), "{text}");
    }
}

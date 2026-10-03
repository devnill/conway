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
//! it with this one. Board item `01M1YVH1X49WYSQ9C2Z4D6B4XM` ("B3d") later
//! gave `/help` (`view/help.rs`) a commands section of its own, but NOT a
//! second listing: it reads straight from [`matches()`] (this module's
//! OWN function, called with the catch-all `"/"` fragment), so this
//! remains the single place a command's name/usage/description is
//! filtered and ranked, no matter which surface is asking.
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
//! built-in is, and `/help`'s own commands section (`view/help.rs::
//! command_rows`, reading straight from [`matches()`]) covers it too, with
//! no separate listing to keep in sync.
//!
//! **A colon-typed plugin-command prefix now self-corrects (dogfooding
//! finding `01M1M3RKHNAJP1TRNTT3ENH68W`, filed during the capstone
//! virgin-walk board item `01M0X1GRJ52SF38FV8E0V7V7B4`).**
//! A translated Claude Code skill's real, registered name always uses
//! conway's own `.` namespace separator (`/ideate.refine`, never
//! `/ideate:refine` -- see `docs/plugins/claude-compat.md`'s "What runs,
//! with real caveats" section for why), and `commands::parse` has long
//! accepted a leading `:` as a typed-input ALIAS for `.` on a FINISHED
//! command word. This module's own live filter did not: typing
//! `/ideate:re` matched nothing at all, silently, even though finishing
//! the word to `/ideate:refine` and pressing Enter would have worked --
//! the one surface an operator actually watches while typing disagreed
//! with the one that resolves. [`matches()`] now runs `input` through
//! `commands::translate_colon_alias` before filtering, so a colon-typed
//! prefix now finds and displays the SAME `/ideate.refine` row a dot-typed
//! prefix already did -- the palette suggests the working form instead of
//! looking empty.
//!
//! **Three-tier ranking, not a flat prefix filter (board item
//! `01M1YVH1X49WYSQ9C2Z4D6B4XM`, "B3d").** A plugin command's full name is
//! always namespaced (`/conway.history.rewind`), so a plain
//! `starts_with` left an operator typing the ACTION they remember
//! (`/rewind`) with no matches at all -- the prefix they knew was buried
//! past a namespace they had to already know to type. [`matches()`] now
//! ranks each candidate into the best of three tiers it qualifies for
//! (never more than one row per candidate):
//!
//! 1. **Exact prefix** -- the unchanged original behavior, `name.
//!    starts_with(fragment)`; ties keep [`commands::builtin_commands`]'s own
//!    declaration order (never re-sorted within this tier), which is what
//!    `tests::a_prefix_matches_ask_agents_and_await` pins.
//! 2. **Namespace-stripped prefix** -- the LAST `.`-separated segment of the
//!    name (`"rewind"` for `/conway.history.rewind`) starts with the
//!    fragment; `/rewind` lands here. Declaration order again, same
//!    reasoning as tier 1.
//! 3. **Subsequence/fuzzy** -- `crate::tui::mentions::score` (the SAME
//!    scorer `@`-mention completion uses, reused rather than
//!    hand-duplicated -- see that function's own doc) run against the bare
//!    command name; ranked by the scorer's own `(start, gap, length)`
//!    tuple, best first. **Name only, never the description, and never
//!    below `MIN_FUZZY_FRAGMENT_LEN` characters** -- that constant's own
//!    doc explains why: a short fragment is a near-certain subsequence of
//!    almost any prose, and scoring descriptions at any length flooded the
//!    result list for exactly the short, common prefixes (`/a`, `/as`) a
//!    whole pre-existing test suite already pins to a precise, narrow
//!    result.
//!
//! A candidate absent from all three tiers is dropped. An empty fragment
//! (bare `/`) matches everything in tier 1 (`starts_with("")` is always
//! true), so tiers 2/3 never actually fire for that input -- matching the
//! pre-existing `slash_alone_lists_every_command` behavior exactly.

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

/// A candidate's rank: which tier it qualified for (`0` = exact prefix,
/// `1` = namespace-stripped prefix, `2` = subsequence/fuzzy -- this
/// module's own top-of-file doc spells out each), then a tie-breaker.
/// Tiers 0/1 carry the constant `(0, 0, 0)` tie-breaker so `slice::sort_by`
/// (a STABLE sort) leaves their relative order exactly as `candidates`
/// produced it -- i.e. `commands::builtin_commands`'s own declaration
/// order, never re-sorted within a tier (the module doc's own citation of
/// `tests::a_prefix_matches_ask_agents_and_await`). Tier 2's tie-breaker is
/// the real `crate::tui::mentions::score` tuple, so fuzzy matches really
/// do rank best-first rather than merely "matched, in whatever order."
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Rank(u8, (usize, usize, usize));

/// The namespace-stripped tail of a command name: everything after its
/// LAST `.`, or the whole (already-`/`-stripped) name when it has none --
/// `"rewind"` for `"conway.history.rewind"`, `"quit"` for `"quit"`. Used
/// only for tier 1's prefix check (module doc).
fn namespace_tail(bare_name: &str) -> &str {
    bare_name.rsplit('.').next().unwrap_or(bare_name)
}

/// Tier 2 (subsequence/fuzzy) never fires below this fragment length.
///
/// **Why a floor exists at all, and why the NAME is the only thing tier 2
/// scores against (never the description, despite this module's own
/// top-of-file doc having first described the wider shape).** A fragment
/// this short is a near-certain subsequence of almost anything: a
/// one-character fragment (`"a"`) or two-character one (`"as"`) is
/// contained, scattered, in most English prose, so scoring against
/// DESCRIPTION text at any length flooded the result list back to "nearly
/// every command" -- the exact regression `tests::
/// a_prefix_matches_ask_agents_and_await`/`tests::
/// as_prefix_filters_to_ask_only` already pinned down (verified to fail:
/// scoring the full `"name description"` haystack with no floor made `/a`
/// return 19 of 20 commands instead of the 3 real prefix matches). `3` is
/// the shortest floor that keeps every one of those pre-existing,
/// behavior-locked tests passing while still leaving real room for tier 2
/// to do useful work (`tests::fuzzy_tier_ranks_a_contained_match_below_a_
/// namespace_prefix_match` exercises it with a 3-character fragments).
const MIN_FUZZY_FRAGMENT_LEN: usize = 3;

/// This candidate's [`Rank`] against `normalized` (the full, `/`-prefixed,
/// colon-aliased-to-dot input) and `bare_fragment` (`normalized` with its
/// leading `/` stripped, for the two tiers that compare against a
/// namespace-free or whole-text shape) -- `None` when it qualifies for no
/// tier at all.
fn rank(row: &PaletteRow<'_>, normalized: &str, bare_fragment: &str) -> Option<Rank> {
    if row.name.starts_with(normalized) {
        return Some(Rank(0, (0, 0, 0)));
    }
    if bare_fragment.is_empty() {
        // An empty fragment already matched every candidate in tier 0
        // above (`starts_with("")` is always `true`) -- unreachable in
        // practice, kept only so this function stays total.
        return None;
    }
    let bare_name = row.name.trim_start_matches('/');
    if namespace_tail(bare_name).starts_with(bare_fragment) {
        return Some(Rank(1, (0, 0, 0)));
    }
    if bare_fragment.chars().count() < MIN_FUZZY_FRAGMENT_LEN {
        return None;
    }
    crate::tui::mentions::score(bare_name, bare_fragment).map(|score| Rank(2, score))
}

/// Every command (built-in, from `commands::builtin_commands()`, and every
/// installed plugin command, from `plugin_commands`), ranked by `rank`
/// and sorted best-first (module doc's three tiers). Empty for input not
/// starting with `/` (module notes: the caller only shows the palette at
/// all when `AppState::input` starts with `/`, but `matches` is total over
/// any `&str` so it never panics if called otherwise).
pub fn matches<'a>(input: &str, plugin_commands: &'a [PluginCommandEntry]) -> Vec<PaletteRow<'a>> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    // `commands::parse` already accepts a leading `:` as an alias for `.`
    // on a finished plugin-command word -- normalizing the live prefix the
    // identical way before filtering keeps this palette from disagreeing
    // with what will actually resolve (module doc above).
    let normalized = commands::translate_colon_alias(input);
    let bare_fragment = normalized.trim_start_matches('/');
    let builtins = commands::builtin_commands()
        .into_iter()
        .map(|c| PaletteRow {
            name: c.name,
            usage: c.usage,
            description: c.description,
        });
    let plugins = plugin_commands.iter().map(|c| PaletteRow {
        name: &c.name,
        // A plugin command declares only a name + one-line summary
        // (`conway::plugin::CommandSpec`'s own doc: deliberately no
        // separate usage-shape field) -- its own name doubles as its
        // usage form, exactly like a bare built-in (`/help`, `/quit`)
        // already does above.
        usage: &c.name,
        description: &c.description,
    });
    let mut ranked: Vec<(Rank, PaletteRow<'a>)> = builtins
        .chain(plugins)
        .filter_map(|row| rank(&row, &normalized, bare_fragment).map(|r| (r, row)))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    ranked.into_iter().map(|(_, row)| row).collect()
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
    fn a_prefix_matches_ask_agents_and_await() {
        let names: Vec<&str> = matches("/a", &[]).iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/ask", "/agents", "/await"]);
    }

    #[test]
    fn unknown_prefix_yields_no_matches() {
        assert!(matches("/zzz", &[]).is_empty());
    }

    /// Board item `01M0RW29F2ATVGCV0R8H0GQEYH`: `/tree` had been demoted
    /// to a hidden alias -- it parsed (`commands.rs` kept the arm) but
    /// never showed up as completion, which is exactly the "advertised
    /// nowhere, works anyway" defect fixed here. `/tree` is
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

    /// `/thinking` and `/timestamps` are REMOVED, not aliased --
    /// `/settings` replaces both. Neither name appears in the palette at
    /// all any more -- `commands::builtin_commands()`
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

    /// Dogfooding finding `01M1M3RKHNAJP1TRNTT3ENH68W` (filed during the
    /// capstone virgin-walk board item `01M0X1GRJ52SF38FV8E0V7V7B4`):
    /// typing the colon form Claude Code
    /// itself would have you type used to find nothing at all, live, even
    /// though `commands::parse` already accepted the finished word. A
    /// still-being-typed colon prefix must now surface the SAME dot-named
    /// row a dot-typed prefix already does.
    #[test]
    fn a_colon_typed_prefix_still_finds_the_dot_form_plugin_command() {
        let plugins = fixture_plugin_commands();
        let found = matches("/acme:gr", &plugins);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "/acme.greet");
    }

    /// A finished colon-typed word behaves identically to its dot-typed
    /// twin, not merely a superset match -- both resolve to the exact same
    /// one row.
    #[test]
    fn a_colon_typed_prefix_matches_identically_to_the_dot_form() {
        let plugins = fixture_plugin_commands();
        assert_eq!(
            matches("/acme:greet", &plugins),
            matches("/acme.greet", &plugins)
        );
    }

    // ---- board item `01M1YVH1X49WYSQ9C2Z4D6B4XM`: ranked/fuzzy matching ----

    fn rewind_and_brewery_plugin_commands() -> Vec<PluginCommandEntry> {
        vec![
            PluginCommandEntry {
                name: "/conway.history.rewind".to_string(),
                description: "roll the session back to an earlier point".to_string(),
            },
            // A namespaced command whose final segment CONTAINS "rew" but
            // does not START with it -- qualifies only for tier 3
            // (subsequence), never tier 2 (namespace-stripped prefix).
            PluginCommandEntry {
                name: "/conway.other.brewery".to_string(),
                description: "an unrelated command sharing three letters".to_string(),
            },
        ]
    }

    /// Acceptance (2): `/rew` finds `/conway.history.rewind` even though
    /// the operator never typed the plugin's own `conway.history`
    /// namespace -- tier 2 (namespace-stripped prefix).
    #[test]
    fn a_namespace_stripped_prefix_finds_a_plugin_command_by_its_final_segment() {
        let plugins = rewind_and_brewery_plugin_commands();
        let found = matches("/rew", &plugins);
        assert!(
            found.iter().any(|r| r.name == "/conway.history.rewind"),
            "found: {found:?}"
        );
    }

    /// The ranked ordering itself (constraints: "ranked ... exact prefix
    /// first, then namespace-stripped prefix, then subsequence"): a
    /// namespace-stripped PREFIX match (`rewind`) must outrank a merely
    /// CONTAINED subsequence match (`brewery`) for the identical fragment.
    #[test]
    fn fuzzy_tier_ranks_a_contained_match_below_a_namespace_prefix_match() {
        let plugins = rewind_and_brewery_plugin_commands();
        let found = matches("/rew", &plugins);
        let names: Vec<&str> = found.iter().map(|r| r.name).collect();
        let rewind_pos = names
            .iter()
            .position(|n| *n == "/conway.history.rewind")
            .expect("rewind must be found at all");
        let brewery_pos = names
            .iter()
            .position(|n| *n == "/conway.other.brewery")
            .expect("brewery must be found too, via the fuzzy tier");
        assert!(
            rewind_pos < brewery_pos,
            "a namespace-stripped prefix match must rank above a merely \
             contained subsequence match: {names:?}"
        );
    }

    /// A fragment below `MIN_FUZZY_FRAGMENT_LEN` never reaches the fuzzy
    /// tier at all, even when it IS a genuine (if trivial) subsequence of a
    /// command name -- the floor this module's own doc explains is what
    /// keeps a one/two-character fragment from flooding the result list.
    #[test]
    fn a_too_short_fragment_never_triggers_the_fuzzy_tier() {
        let plugins = rewind_and_brewery_plugin_commands();
        // "re" (2 chars) is a genuine subsequence of "brewery" too, but
        // below the floor -- `/conway.other.brewery` must not appear.
        let found = matches("/re", &plugins);
        assert!(
            !found.iter().any(|r| r.name == "/conway.other.brewery"),
            "a 2-character fragment must not trigger the fuzzy tier: {found:?}"
        );
    }

    /// Every pre-existing, behavior-locked short-prefix test the fuzzy tier
    /// risked flooding, restated together as one regression guard: `/a`
    /// (1 char) and `/as` (2 chars) must still return EXACTLY their real
    /// prefix matches, nothing more -- an un-floored fuzzy tier scoring the
    /// full `"name description"` text produced `/a` returning 19 of 20
    /// commands instead of 3 (`MIN_FUZZY_FRAGMENT_LEN`'s own doc).
    #[test]
    fn short_fragments_still_return_only_their_real_prefix_matches() {
        let a_names: Vec<&str> = matches("/a", &[]).iter().map(|c| c.name).collect();
        assert_eq!(a_names, vec!["/ask", "/agents", "/await"]);

        let as_names: Vec<&str> = matches("/as", &[]).iter().map(|c| c.name).collect();
        assert_eq!(as_names, vec!["/ask"]);
    }
}

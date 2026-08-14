//! The bottom status line (T3): a single, always-visible plain line -- no
//! border -- summarizing the focused agent's turn at a glance. The line is
//! an ordered, configurable set of fields driven by `[tui.status_line]` in
//! `settings.json` (schema: `conway::config::schema::StatusLineConfig`).
//! Each field renders only when it is both listed in the configured `fields`
//! order AND has data to show (e.g. `git` is omitted when not in a repo,
//! `model` is omitted before the first `ModelDecision`).
//!
//! Default Lean line: `session | lineage | mode | model | ctx | tokens |
//! activity | hint`.
//!
//! - `session` -- **NEW** (this item, correcting a T6 requirement miss --
//!   see `view/header.rs`'s module doc for the full story): `session <id>`,
//!   the session's own root agent's short id. This is application chrome
//!   ("what session am I in"), unconditional (always renders), which is
//!   exactly why it belongs HERE and not on the scroll-triggered sticky
//!   overlay T6 originally misfiled it onto -- chrome that flickers with
//!   scroll position is noise, and this field never has. **Widened (board
//!   item) to `session <id>@<seq>`** once
//!   `AppState::session_head_seq` is known: this session's own persisted
//!   log head, in the exact `<session-id>[@<seq>]` notation
//!   `session_ref.rs`'s `--fork-from` flag already established -- the
//!   number `/conway.history.rewind <seq>` (`conway-plugin-history`) takes.
//!   Before this item, nothing in the TUI showed an operator ANY `LogSeq`
//!   at all; degrades to the bare `session <id>` form (unchanged from
//!   before this item) whenever the head is not yet known.
//! - `lineage` -- **NEW** (this item; V5's content, relocated). Off-root,
//!   `agent <id>` growing a `via` clause naming how the focused agent came
//!   to be: `agent <id> via root → fork @seq 3 → @reviewer`. Empty (omitted)
//!   while the transcript shows the session's own root -- the common
//!   single-agent case stays uncluttered, same as the pre-move behavior.
//!   Degrades through shorter COMPLETE forms under a narrow terminal or a
//!   deep ancestry chain (see [`LineageDetail`]) rather than being clipped
//!   mid-word -- V5's own width-degrade machinery, moved here unchanged.
//!   Metadata only, never an ancestor's actual transcript content -- see
//!   [`agent_field`]'s own doc for the fork-vs-spawn trap this was written
//!   to sidestep.
//! - `mode` -- `ready`/`awaiting permission`/`ask`/`intent` (the TUI's
//!   current top-level mode).
//! - `model` -- the focused agent's serving model display name from
//!   `Event::ModelDecision` (e.g. `anthropic/claude-sonnet-4-6`); omitted
//!   before the first turn routes.
//! - `ctx` -- context-window occupancy: `ctx 42%` when the focused model's
//!   max context is known from `[models.metadata_path]`, else the raw
//!   cumulative token estimate `ctx 12.3k`. The numerator is the cumulative
//!   sum of `Event::ContextSegmentAdded { tokens_est }` on the focused
//!   agent's stream (session-wide, NOT per-turn).
//! - `tokens` -- the focused agent's cumulative token spend as
//!   `<total> tok (<n%> cached)`, where `total` is the sum of every
//!   `Usage` field (input + output + both cache dimensions + reasoning)
//!   and `n%` is the cache hit rate `cache_read / (input + cache_read +
//!   cache_write)`. The parenthetical is omitted when its denominator is 0
//!   (no cache activity yet) -- the field then reads `<total> tok`.
//! - `activity` -- T2's working indicator: a braille spinner glyph plus the
//!   activity word plus live elapsed plus new-segment tokens added this
//!   turn, e.g. `⠋ thinking… 12s · +45 tok`, pulsing through
//!   a steady `theme.spinner` style (V6 removed T2's color pulse -- the
//!   advancing braille frames carry liveness without strobing). While
//!   idle: just `idle`.
//! - `hint` -- a persistent keybinding/affordance hint:
//!   `Enter submit · Ctrl-E expand · /help · /agents to {view|hide}`,
//!   plus `focused: <id>` when the transcript is focused on a non-root
//!   agent **and `lineage` is NOT part of the resolved field list** (this
//!   item: `lineage` already names the focused agent off-root, so keeping
//!   this note unconditionally would say the same thing twice on the
//!   default line; it survives only as a fallback for an older pinned
//!   `[tui.status_line] fields` list that predates `lineage` and so never
//!   gained it). T7 confirmed the rest of this hint against the real
//!   binding set (`view/help.rs`'s own doc enumerates it in full) and found
//!   it still accurate -- `/help` now opens the keybinding overlay instead
//!   of dumping a command list into the transcript, but it is still the
//!   correct, and only, thing to name here for "how do I see the rest of
//!   the bindings".
//! - `git` -- the current `git rev-parse --abbrev-ref HEAD` branch, read
//!   once at startup; omitted when not in a git repo.
//! - `cwd` -- the session's working directory; omitted when unset.
//!
//! The whole line uses `theme.status_mode` (reversed) as its base style;
//! the activity spinner/phrase pulse and the dim `hint` field overlay
//! their own styles on top. V7: the non-default permission-mode label
//! overlays `theme.emphasized` (bold, `plan`) or `theme.fatal_error` (red +
//! bold, `AUTO-ALLOW`) -- see [`mode_ladder`] for why the two no longer
//! share a style.
//!
//! **Width-aware assembly (review finding).** Adding `session`/`lineage`
//! made the default line long enough (~106 chars) that a plain
//! "render every field's full text, let the terminal clip whatever falls
//! off the right edge" approach silently ate `hint` -- the line's only
//! pointer to `/help` and the `/agents` toggle -- on anything narrower than
//! about 106 columns, and ate it ENTIRELY below ~40. [`status_line_spans`]
//! now treats the line as one width-budgeted whole: every field is built as
//! a small ladder of shorter-but-still-COMPLETE forms (the same shape
//! `header.rs`'s `footer_text` and this module's own [`LineageDetail`]
//! already used), and when the full assembly doesn't fit, fields degrade
//! one step at a time in a fixed PRIORITY order until it does (or until
//! nothing more can be shrunk, which is then the defined outcome, not an
//! accident).
//!
//! **The priority order (dropped/shrunk first -> last), and why:**
//! 1. `cwd`, `git` -- ambient chrome with no bearing on the current turn;
//!    already omitted whenever unset, so extending "omitted" to "omitted
//!    under width pressure too" changes nothing about their character.
//! 2. `model`, `ctx`, `tokens` -- point-in-time telemetry. Useful, but
//!    reconstructable from the transcript/turn-end summary if briefly
//!    absent from this one line.
//! 3. `session`, then `lineage` (degrades through [`LineageDetail`]'s own
//!    Full -> Compact -> Bare first, THEN drops entirely) -- orientation:
//!    "which session/agent am I in". Below `hint`/`mode` because losing
//!    orientation for one narrow render is recoverable by widening the
//!    terminal or checking `/agents`; it is not a safety gap and it is not
//!    the line's only route to a keybinding.
//! 4. `activity` -- degrades to spinner + phrase (dropping the elapsed/token
//!    tail) and, if the line still doesn't fit, is omitted entirely. It is
//!    the last field to give up ALL of its space before `hint`/`mode`, which
//!    is deliberate: on a genuinely tiny terminal, "is it working right
//!    now" is the thing to sacrifice so that "how do I get help" and "am I
//!    in a dangerous mode" do not have to compete with it for the last few
//!    columns.
//! 5. `hint` -- discoverability. Degrades full -> `/help · /agents to
//!    {view|hide}` -> bare `/help`, and is dropped entirely only as the
//!    very last resort before touching `mode`. This is placed ABOVE
//!    `session`/`lineage`/telemetry deliberately: those name facts about
//!    the CURRENT state, but `hint` is the only field that tells a reader
//!    how to get UNSTUCK (find more bindings, toggle the agent view) -- it
//!    earns the second-to-last slot, not the first to go.
//! 6. `mode` -- NEVER dropped. Its own ladder has exactly one degrade step
//!    (drop the `ready`/`awaiting permission` UI word, keep the non-default
//!    permission-mode label alone) and that step exists specifically so
//!    `AUTO-ALLOW` -- a genuine safety signal per `PermissionMode::label`'s
//!    own doc: an operator who forgets they're in it is the exact failure
//!    this guards against -- is the LAST thing on the line to ever lose
//!    space, and is never itself removed. See [`mode_ladder`].
//!
//! **This width guarantee only protects a field that is actually in the
//! resolved list (adversarial review finding 1, critical).** [`resolve_fields`]
//! used to accept a configured `fields` list verbatim even when it simply
//! never named `mode` -- the ladder's survival guarantee is worthless if
//! `mode` never enters the list at all, and this was a real, silent way to
//! disable `AUTO-ALLOW` via config (a hand-pinned `settings.json`, or
//! `CONWAY_TUI__STATUS_LINE__FIELDS` set without it), not just a width
//! accident. Fixed at [`resolve_fields`]: while `permission_mode` is
//! non-default (`Plan`/`AutoAllow`), `mode` is forced into the resolved
//! list even when the configured `fields` omits it -- unconditionally, not
//! configurable, and reached uniformly from every `StatusLineConfig`
//! source. See that function's own doc for why this is the "appear only
//! when it carries information" option rather than "always force it in".
//!
//! **Never a silent clip.** Every ladder's shorter rungs are complete
//! phrasings on their own (never a truncated fragment of a longer one --
//! the same rule `header.rs::footer_text` already follows); when even the
//! lowest rung of everything degradable doesn't fit, the assembly stops
//! trying (there is nothing shorter left to say) rather than trimming a
//! span mid-character DURING THAT LOOP. Below the floor -- a pathological
//! width narrower than even the most degraded line -- [`clamp_to_width`]
//! (adversarial review finding 3) makes the LAST resort explicit instead of
//! silent: the assembled line is cut at a character boundary and marked
//! with a trailing `…`, rather than being handed over-length to a
//! `Paragraph` with no `.wrap()` and letting ratatui truncate wherever the
//! render `Rect` happens to end (verified empirically pre-fix: width 10
//! rendered `" AUTO-ALLO"`, width 5 rendered `" AUTO"` -- indistinguishable
//! from an accident). Width accounting throughout is in terminal COLUMNS,
//! not `char` count (adversarial review finding 2): a field's text is not
//! restricted to ASCII (`lineage`'s `@{agent_def}` hop names are
//! user-chosen), and a CJK character or emoji is one `char` but two
//! columns -- [`ladder_width`] and [`clamp_to_width`] both measure via
//! `Span::width()` (ratatui's own display-width helper) rather than
//! `.chars().count()`.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::agents;
use super::theme::Theme;
use conway::{AgentId, PermissionMode};

use crate::tui::state::{should_animate, Activity, AppState, Mode, SPINNER_FRAMES};

pub fn draw(frame: &mut Frame, area: Rect, state: &AppState, theme: &Theme) {
    let line = status_line_spans(state, theme, area.width);
    let paragraph = Paragraph::new(line).style(theme.status_mode);
    frame.render_widget(paragraph, area);
}

/// A generous width for tests that don't care about the `lineage` field's
/// own width-fit degrade -- large enough that no ordinary status-line test
/// accidentally exercises it (mirrors the pre-move `header.rs::WIDE`
/// constant); the dedicated degrade tests below drive
/// [`status_line_spans`] directly with a narrow width instead.
#[cfg(test)]
const WIDE: u16 = 200;

/// Pure formatting, split out from [`draw`] so it is testable with no
/// `Frame`/terminal at all. Returns the plain-text content of the status
/// line (no styling) -- the styled path ([`status_line_spans`]) is what
/// [`draw`] actually renders, and the two share the same text content via
/// [`flatten`] so they can never drift apart. Test-only: the production
/// render path uses [`status_line_spans`] directly, but this plain-text
/// view stays the most ergonomic seam for the existing `contains(..)`
/// status-line tests.
#[cfg(test)]
pub fn status_line(state: &AppState) -> String {
    let line = status_line_spans(state, &Theme::default(), WIDE);
    flatten(&line)
}

/// Flattens a `Line`'s spans into one plain string -- used by
/// [`status_line`] to keep the plain-text path in lockstep with the styled
/// path without duplicating the formatting logic.
#[cfg(test)]
fn flatten(line: &Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// One orderable status-line field (T3). The configured `fields` list
/// (from `[tui.status_line]`) is parsed into this enum at render time;
/// unknown names are dropped (never a panic on untrusted config).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusLineField {
    /// **NEW** (this item). `session <id>` -- see this module's own doc.
    Session,
    /// **NEW** (this item; V5's content relocated). The off-root lineage
    /// breadcrumb -- see this module's own doc and [`agent_field`].
    Lineage,
    Mode,
    Model,
    Ctx,
    Tokens,
    Activity,
    Hint,
    Git,
    Cwd,
}

impl StatusLineField {
    /// Parses one configured field name. Unknown names return `None`
    /// (the caller drops them silently -- never a panic).
    fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "session" => Some(Self::Session),
            "lineage" => Some(Self::Lineage),
            "mode" => Some(Self::Mode),
            "model" => Some(Self::Model),
            "ctx" => Some(Self::Ctx),
            "tokens" => Some(Self::Tokens),
            "activity" => Some(Self::Activity),
            "hint" => Some(Self::Hint),
            "git" => Some(Self::Git),
            "cwd" => Some(Self::Cwd),
            _ => None,
        }
    }
}

/// Resolves the configured `fields` list into an ordered, validated
/// `Vec<StatusLineField>`, dropping unknown names rather than panicking on
/// them. Falls back to the default Lean order when the configured list is empty
/// (an empty `fields = []` would otherwise render a blank line -- treat that as
/// "user wanted defaults" rather than "user wanted nothing").
///
/// **Adversarial review finding 1 (critical).** `mode_ladder`'s width
/// -survival guarantee (`AUTO-ALLOW` is never dropped once space runs
/// out -- see this module's own doc) only holds for a field that is in
/// this resolved list AT ALL. A configured `fields` list that simply never
/// names `mode` -- a hand-pinned `settings.json`, or
/// `CONWAY_TUI__STATUS_LINE__FIELDS` set without it -- used to sail
/// through unmodified (the empty-list fallback above only fires when the
/// list is empty AFTER filtering unknown names, not when a KNOWN field is
/// simply absent), which meant a config change alone could silently turn
/// off the one genuine safety signal on the line with no error, no
/// warning, and no visual sign anything was missing.
///
/// Fix (Option 3 of the three the review named, over Option 1 "always
/// force `mode` in" and Option 2 "validate/refuse at config load"): while
/// `permission_mode` is non-default (`Plan`/`AutoAllow`), `mode` is forced
/// into the resolved list even when `fields` omits it. This is not
/// user-disableable -- it depends only on the ACTIVE permission mode, not
/// on anything in `config`. Picked over Option 1 because it means the
/// field appears exactly when it carries information (an ordinary
/// `Prompt`-mode session with a `fields` list that genuinely omits `mode`
/// keeps rendering exactly as configured -- no `ready`/`awaiting
/// permission` text appears out of nowhere); picked over Option 2 because
/// this same function is reached from both the file-based `settings.json`
/// path and the `CONWAY_TUI__STATUS_LINE__FIELDS` env-var path (both feed
/// the same `StatusLineConfig`), so fixing it here covers both sources
/// uniformly without needing a second enforcement point at config load.
fn resolve_fields(
    config: &conway::config::schema::StatusLineConfig,
    permission_mode: PermissionMode,
) -> Vec<StatusLineField> {
    let mut parsed: Vec<StatusLineField> = config
        .fields
        .iter()
        .filter_map(|name| StatusLineField::parse(name))
        .collect();
    if parsed.is_empty() {
        // Empty / all-unknown config: fall back to the Lean order rather
        // than rendering a blank line (bad input never produces a
        // broken UI -- it falls back to defaults). The Lean order already
        // includes `mode`, so the forced-in step below is a no-op here.
        return resolve_fields(
            &conway::config::schema::StatusLineConfig::default(),
            permission_mode,
        );
    }
    if permission_mode != PermissionMode::Prompt && !parsed.contains(&StatusLineField::Mode) {
        parsed.push(StatusLineField::Mode);
    }
    parsed
}

/// Builds the status line as a styled [`Line`] (T3): an ordered,
/// configurable field set joined by ` | `, each field rendered only when
/// present+enabled, all under the `theme.status_mode` base style. The
/// `activity` field (T2) overlays its spinner pulse color and dim
/// elapsed/tokens tail; the `hint` field overlays `theme.status_dim`.
///
/// **Width-aware assembly (review finding -- see this module's own doc for
/// the priority order and reasoning).** `width` is the status line's own
/// render `Rect` width (`draw`'s caller, `view/mod.rs`). Every field is
/// first built as a [`field_ladder`] -- an ordered list of that field's own
/// candidate texts, most detailed first, each one a complete phrasing on
/// its own (never a truncated fragment of a longer one). Starting with
/// every field at its fullest (index 0), this repeatedly finds the
/// LOWEST-priority field (via `drop_priority`) that still has a shorter
/// rung available and steps it down, until the assembled line's total
/// width fits `width` or nothing more can be shrunk -- so a field only ever
/// gives up space once every field with a weaker claim on it already has.
pub fn status_line_spans(state: &AppState, theme: &Theme, width: u16) -> Line<'static> {
    let fields = resolve_fields(&state.status_line_config, state.permission_mode);
    // This item: `hint`'s own `focused: <id>` note is suppressed whenever
    // `lineage` is part of the resolved field list, so the two never say the
    // same thing twice -- see `hint_ladder`'s own doc. Based on CONFIGURED
    // membership only (not on whether `lineage` ends up actually rendering
    // once width pressure is applied) -- deliberately simple and matches
    // this field's pre-existing semantics.
    let lineage_present = fields.contains(&StatusLineField::Lineage);

    let ladders: Vec<Vec<Vec<Span<'static>>>> = fields
        .iter()
        .map(|&f| field_ladder(f, state, theme, lineage_present))
        .collect();

    // The order in which fields give up space, lowest-priority first -- see
    // `drop_priority`'s own doc for the full reasoning.
    let mut give_up_order: Vec<usize> = (0..fields.len()).collect();
    give_up_order.sort_by_key(|&i| drop_priority(fields[i]));

    let mut rung = vec![0usize; fields.len()];
    let budget = width as usize;
    while ladder_width(&ladders, &rung) > budget {
        match give_up_order
            .iter()
            .find(|&&i| rung[i] + 1 < ladders[i].len())
        {
            Some(&i) => rung[i] += 1,
            // Nothing left to shrink -- every field is already at its own
            // floor. This is the defined "cannot fit even the most
            // degraded form" outcome, not an accidental clip: nothing
            // further is silently trimmed mid-span past this point.
            None => break,
        }
    }

    let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
    let mut first = true;
    for (i, ladder) in ladders.into_iter().enumerate() {
        let mut chosen = ladder.into_iter().nth(rung[i]).unwrap_or_default();
        if chosen.is_empty() {
            continue;
        }
        if !first {
            spans.push(Span::raw(" | "));
        }
        first = false;
        spans.append(&mut chosen);
    }
    spans.push(Span::raw(" "));
    // Adversarial review finding 3: the give-up loop above can legitimately
    // exit with every field already at its own floor and the assembly
    // STILL over `width` (a floor is not guaranteed to fit an arbitrarily
    // narrow terminal -- `AUTO-ALLOW` alone is 10 columns wide and cannot
    // shrink further). Handing that over-length `Line` straight to a
    // `Paragraph` with no `.wrap()` (`draw`, above) used to let ratatui
    // silently truncate INSIDE a field's text -- contradicting this
    // module's own "never a silent clip" promise (verified empirically:
    // width 10 rendered `" AUTO-ALLO"`, width 5 rendered `" AUTO"`, no
    // visible sign anything was cut). `clamp_to_width` makes that
    // truncation explicit instead: an over-length line is cut at a
    // character boundary and marked with a trailing `…`, so a pathological
    // width still degrades honestly rather than looking like an accident.
    Line::from(clamp_to_width(spans, budget))
}

/// The total rendered width (terminal COLUMNS, not characters) of the
/// currently SELECTED rung of each field's ladder, including the ` | `
/// separators between non-empty fields and the line's own leading/trailing
/// single-space padding -- the exact same accounting
/// [`status_line_spans`]'s assembly loop below actually produces, so the
/// fit check can never disagree with what gets drawn.
///
/// **Adversarial review finding 2 (critical).** This used to sum
/// `.content.chars().count()` -- one CJK character or emoji is ONE `char`
/// but TWO terminal columns, and ratatui accounts for display width when
/// it writes cells (`Span::width()`/`Line::width()`, both backed by
/// `unicode-width` internally), so the old arithmetic could be wrong by up
/// to 2x for any field whose text is not ASCII-only. `lineage` embeds
/// `agent_def` names verbatim (`"@{def}"`, `view/agents.rs`'s
/// `recipe_parts`) -- arbitrary user-chosen text with no ASCII restriction
/// -- so this was reachable, not theoretical: an undercounted `lineage`
/// rung could make the fit-check believe the line had more room than it
/// actually did, and the real overflow landed on whichever field ended up
/// last in the assembled text (see `clamp_to_width`'s doc for what used to
/// happen to that overflow). Fixed by summing each span's own `.width()`
/// (ratatui's built-in display-width helper: no new dependency)
/// instead of its character count.
fn ladder_width(ladders: &[Vec<Vec<Span<'static>>>], rung: &[usize]) -> usize {
    let mut non_empty = 0usize;
    let mut content = 0usize;
    for (i, ladder) in ladders.iter().enumerate() {
        let spans = &ladder[rung[i]];
        if spans.is_empty() {
            continue;
        }
        non_empty += 1;
        content += spans.iter().map(Span::width).sum::<usize>();
    }
    content + non_empty.saturating_sub(1) * 3 /* " | " */ + 2 /* leading + trailing space */
}

/// **Adversarial review finding 3.** Clamps an assembled status-line span
/// list to `budget` COLUMNS, explicitly and at a character boundary,
/// rather than handing an over-length `Line` to a `Paragraph` with no
/// `.wrap()` and letting ratatui truncate wherever the `Rect` happens to
/// end. The give-up loop in [`status_line_spans`] can legitimately exit
/// with every field already at its own floor and the total still over
/// `budget` -- a floor is not guaranteed to fit an arbitrarily narrow
/// terminal (`mode_ladder`'s own floor, the bare `AUTO-ALLOW` label, is 10
/// columns wide on its own and has nowhere shorter to go). Pre-fix, THAT
/// overflow reached `Paragraph` unclamped and ratatui cut it silently
/// wherever the `Rect` boundary fell -- verified empirically: width 10
/// rendered `" AUTO-ALLO"`, width 8 rendered `" AUTO-AL"`, width 5
/// rendered `" AUTO"`, each indistinguishable from an accident and each a
/// direct contradiction of this module's own "never a silent clip" promise
/// (see the module doc).
///
/// This function walks the spans in order, keeping whole spans that still
/// fit, and truncates the first span that does not at a character
/// boundary, reserving one column for a trailing `…` marker whenever any
/// content had to be cut. A span that fits EXACTLY (no cut needed) is kept
/// whole with no `…` -- e.g. at exactly the floor's own width, the trailing
/// pad space is what gets dropped, not the label (see the width-11 test
/// case below). Below this function, nothing is ever silently trimmed
/// mid-character again: whatever the assembled line shows at a
/// pathological width, it says so.
fn clamp_to_width(spans: Vec<Span<'static>>, budget: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(Span::width).sum();
    if total <= budget {
        return spans;
    }
    if budget == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let w = span.width();
        if used + w <= budget {
            result.push(span);
            used += w;
            continue;
        }
        // This span is the one that overflows -- truncate IT at a
        // character boundary and stop; everything after it is dropped
        // wholesale (never a fragment of a LATER span either).
        let remaining = budget - used;
        // Reserve one column for the `…` marker whenever this span itself
        // does not fit whole (there is more content after the cut point,
        // by definition -- this span alone already exceeds `remaining`).
        let target = remaining.saturating_sub(1);
        let truncated = truncate_to_width(span.content.as_ref(), target);
        let truncated_width: usize = Span::raw(truncated.clone()).width();
        if truncated_width < remaining {
            // There is room for the `…` marker after the truncated text.
            let mut content = truncated;
            content.push('…');
            result.push(Span::styled(content, span.style));
        } else if remaining > 0 {
            // No room even for one character plus the marker -- the
            // marker alone still fits (`remaining >= 1` here, since the
            // `used + w <= budget` check above already failed and
            // `remaining > 0`, `…` is one column wide).
            result.push(Span::raw("…"));
        }
        break;
    }
    result
}

/// Truncates `s` to the longest prefix whose display width does not exceed
/// `target` columns, at a character boundary (never splitting a multi-byte
/// `char`). Used only by [`clamp_to_width`]'s own pathological-width
/// fallback -- ordinary rendering never reaches this, every field's own
/// ladder rung is already a complete phrasing (this module's "never a
/// truncated fragment" rule).
fn truncate_to_width(s: &str, target: usize) -> String {
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = Span::raw(ch.to_string()).width();
        if w + cw > target {
            break;
        }
        w += cw;
        out.push(ch);
    }
    out
}

/// The order fields give up space in when the line does not fit, LOWEST
/// number first -- see this module's own doc for the full "why this order"
/// reasoning. Summary: ambient chrome and point-in-time telemetry go first
/// (0-4); orientation (`session`/`lineage`) next (5-6); the liveness signal
/// `activity` after that (7); `hint` -- discoverability -- second-to-last
/// (8); `mode` last (9), and `mode`'s own ladder (`mode_ladder`) never
/// drops to nothing, so `AUTO-ALLOW` is the one thing guaranteed to survive
/// as long as anything at all does.
fn drop_priority(field: StatusLineField) -> u8 {
    match field {
        StatusLineField::Cwd => 0,
        StatusLineField::Git => 1,
        StatusLineField::Model => 2,
        StatusLineField::Ctx => 3,
        StatusLineField::Tokens => 4,
        StatusLineField::Session => 5,
        StatusLineField::Lineage => 6,
        StatusLineField::Activity => 7,
        StatusLineField::Hint => 8,
        StatusLineField::Mode => 9,
    }
}

/// One field's ladder of candidate renderings, most detailed first (index
/// 0) down to its floor -- the shortest form this field will ever show. A
/// field that can vanish entirely under width pressure ends its ladder
/// with an empty `Vec` (the omitted state, identical to "absent" -- e.g. no
/// git branch); a field whose floor must always show SOMETHING (`Mode`,
/// `Lineage` off-root, `Activity` while animating) never does. Every rung
/// is a complete phrasing on its own, matching the "shorter COMPLETE form,
/// never a mid-word clip" rule `header.rs::footer_text` and this module's
/// own [`LineageDetail`] already established -- this is that same shape
/// generalized to the whole line instead of one field.
fn field_ladder(
    field: StatusLineField,
    state: &AppState,
    theme: &Theme,
    lineage_present: bool,
) -> Vec<Vec<Span<'static>>> {
    match field {
        // `@<seq>` appended once
        // `AppState::session_head_seq` is known -- see this module's own
        // doc for the notation's precedent (`session_ref.rs`) and why this
        // is the field that had to carry it.
        StatusLineField::Session => vec![
            vec![Span::raw(match state.session_head_seq {
                Some(seq) => format!(
                    "session {}@{}",
                    agents::short_agent_id(state.root_agent()),
                    seq.0
                ),
                None => format!("session {}", agents::short_agent_id(state.root_agent())),
            })],
            vec![],
        ],
        StatusLineField::Lineage => lineage_ladder(state),
        StatusLineField::Mode => mode_ladder(state, theme),
        StatusLineField::Model => match state.focused_model.as_deref() {
            Some(name) => vec![vec![Span::raw(name.to_string())], vec![]],
            None => vec![vec![]],
        },
        StatusLineField::Ctx => vec![vec![Span::raw(ctx_label(state))], vec![]],
        StatusLineField::Tokens => vec![vec![Span::raw(tokens_label(state))], vec![]],
        StatusLineField::Activity => activity_ladder(state, theme),
        StatusLineField::Hint => hint_ladder(state, theme, lineage_present),
        StatusLineField::Git => match state.git_branch.as_deref() {
            Some(b) => vec![vec![Span::raw(b.to_string())], vec![]],
            None => vec![vec![]],
        },
        StatusLineField::Cwd => match state.cwd_display.as_deref() {
            Some(c) => vec![vec![Span::raw(c.to_string())], vec![]],
            None => vec![vec![]],
        },
    }
}

/// The `mode` field's text.
fn mode_label(mode: &Mode) -> String {
    match mode {
        Mode::Normal => "ready".to_string(),
        Mode::AwaitingPermission(_) => "awaiting permission".to_string(),
        // B5: the /ask modal owns the screen -- the status line says so.
        Mode::AskModal(_) => "ask".to_string(),
        // C2: the NL intent confirmation card owns the screen.
        Mode::IntentConfirm(_) => "intent".to_string(),
    }
}

/// The `mode` field's ladder (V2/V7, ladder shape added by this item's
/// width-aware assembly). While `Prompt` (the default) there is no
/// non-default label to preserve, so the ladder is a single rung: the UI
/// word alone (`ready`/`awaiting permission`/…) -- naming the ordinary case
/// every frame would train the operator to ignore the field, so it is
/// never made MORE prominent than that, but it also never needs a shorter
/// form to fall back to.
///
/// While `AUTO-ALLOW` or `plan` is active, the full rung is the UI word
/// plus the permission-mode label; the ONE degrade step drops the UI word
/// and keeps the label alone. That order is deliberate, not incidental: the
/// UI word (`ready`/…) is the less load-bearing half here, while the
/// permission-mode label is what a distracted operator most needs to
/// notice, especially `AUTO-ALLOW` -- `PermissionMode::label`'s own doc
/// names "an operator who has forgotten they are in it" as the exact
/// failure this must avoid. `status_line_spans`'s `drop_priority` places
/// `Mode` last among every field AND this ladder never ends in an empty
/// `Vec` -- between the two, `AUTO-ALLOW`/`plan` is the one piece of the
/// status line guaranteed to survive as long as anything else does.
///
/// V7: the two non-default modes are not equally risky, so they no longer
/// share a style. `plan` only ever RESTRICTS what runs (it denies mutating
/// categories outright) -- `theme.emphasized` (bold, no color) is enough to
/// flag "not the default" without implying danger. `AUTO-ALLOW` gets
/// `theme.fatal_error` (red + bold), the palette's one highest-alert accent
/// (see `view/theme.rs`'s module doc).
fn mode_ladder(state: &AppState, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    let ui = mode_label(&state.mode);
    match state.permission_mode {
        PermissionMode::Prompt => vec![vec![Span::raw(ui)]],
        PermissionMode::AutoAllow => vec![
            vec![
                Span::raw(ui),
                Span::raw(" · "),
                Span::styled(state.permission_mode.label().to_string(), theme.fatal_error),
            ],
            vec![Span::styled(
                state.permission_mode.label().to_string(),
                theme.fatal_error,
            )],
        ],
        other => vec![
            vec![
                Span::raw(ui),
                Span::raw(" · "),
                Span::styled(other.label().to_string(), theme.emphasized),
            ],
            vec![Span::styled(other.label().to_string(), theme.emphasized)],
        ],
    }
}

/// The `ctx` field's text: `ctx 42%` when the focused model's max context
/// is known, else `ctx 12.3k` (raw tokens, compact-suffixed). Guards
/// divide-by-zero on the max.
///
/// The `pct.min(100)` cap below is a DELIBERATE lossy clamp, not just noise
/// avoidance: `focused_ctx_tokens` is a segment-id-deduped estimate, and an
/// estimate that exceeds the declared `max_context_tokens` (headroom,
/// rounding, a metadata file that under-declares the real window) is shown
/// as `ctx 100%` rather than `ctx 137%` so the status line never looks like
/// a bug to the user. The tradeoff is that this CAN hide a genuine overshoot
/// -- an agent whose context really has grown past its declared max still
/// reads `ctx 100%`, not `ctx 137%`. That is accepted here: the authoritative
/// token total lands via the turn-end summary (T4), and a proper re-fetch of
/// the runtime's true context total on focus is tracked as a separate
/// follow-up No behavior change vs. the original cap -- only the
/// intent is now documented.
///
/// `pub(super)` (T6): the sticky context header (`view/header.rs`) shows the
/// same `ctx%`/raw-tokens figure and reuses this function directly rather
/// than recomputing the percentage formula a second time, so the header and
/// the status line's `ctx` field can never drift apart on the cap/fallback
/// logic.
pub(super) fn ctx_label(state: &AppState) -> String {
    match state.focused_model_max_context {
        Some(max) if max > 0 => {
            let pct = (state.focused_ctx_tokens * 100) / u64::from(max);
            // Deliberate lossy clamp -- see the doc comment above.
            let pct = pct.min(100);
            format!("ctx {pct}%")
        }
        _ => format!("ctx {}", compact_tokens(state.focused_ctx_tokens)),
    }
}

/// The `tokens` field's text: `<total> tok (<n%> cached)` when the cache
/// denominator is non-zero, else `<total> tok`. `total` is the sum of
/// every `Usage` field (input + output + both cache dimensions +
/// reasoning); the cache hit rate is `cache_read / (input + cache_read +
/// cache_write)`.
fn tokens_label(state: &AppState) -> String {
    let usage = &state.focused_agent_usage;
    let total = spent_tokens(usage);
    let denom = u64::from(usage.input_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens);
    if denom == 0 || usage.cache_read_tokens == 0 {
        return format!("{total} tok");
    }
    let pct = (u64::from(usage.cache_read_tokens) * 100) / denom;
    format!("{total} tok ({pct}% cached)")
}

/// The `activity` field's ladder (T2; ladder shape added by this item's
/// width-aware assembly): spinner glyph + activity word in the current
/// pulse color, plus live elapsed + new-segment tokens added this turn
/// (dim) while active; just `idle` while idle. The first degrade step
/// (while active) drops the elapsed/token tail and keeps the glyph+phrase
/// alone -- "is something happening at all" is the more load-bearing half.
/// The LAST step omits the field entirely: `activity` sits BELOW `hint` and
/// `mode` in `drop_priority` (liveness is useful, but neither a
/// discoverability nor a safety signal), so at the point this field is
/// asked to give up its last rung, `hint` has not lost anything yet and
/// `mode` has not been touched at all -- letting this go the rest of the
/// way is what keeps THOSE guarantees intact on a genuinely tiny terminal
/// rather than the two of them fighting `activity` for the last few
/// columns.
fn activity_ladder(state: &AppState, theme: &Theme) -> Vec<Vec<Span<'static>>> {
    if !should_animate(&state.activity) {
        return vec![vec![Span::raw("idle")], vec![]];
    }
    // V6: a single steady style. T2 cycled this through
    // `Theme::spinner_palette` on every 125ms tick, which read as a pulse in
    // the corner of the eye and was more distracting than informative. The
    // braille frames below still advance, so liveness is still visible --
    // motion carries that signal perfectly well without also strobing color.
    let style = theme.spinner;
    let glyph = SPINNER_FRAMES
        .get(state.spinner_frame % SPINNER_FRAMES.len())
        .copied()
        .unwrap_or("");
    let phrase = activity_phrase(&state.activity);
    let elapsed = state
        .turn_started_at
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    vec![
        vec![
            Span::styled(format!("{glyph} {phrase}"), style),
            Span::styled(
                format!(" {elapsed}s · +{} tok", state.turn_running_tokens),
                theme.status_dim,
            ),
        ],
        vec![Span::styled(format!("{glyph} {phrase}"), style)],
        vec![],
    ]
}

/// The `hint` field's ladder (T3; ladder shape added by this item's
/// width-aware assembly): a persistent keybinding/affordance hint, rendered
/// dim. The full rung includes the `/agents` toggle affordance and, when
/// the transcript is focused on a non-root agent AND `lineage` is not part
/// of the resolved field list, a `focused: <id>` note. Degrades through
/// `/help · /agents to {view|hide}` to bare `/help` -- the one pointer this
/// field exists for -- before finally being omitted entirely as the very
/// last resort ahead of `mode` (see `drop_priority`'s own doc for why
/// `hint` sits this high in the give-up order).
fn hint_ladder(state: &AppState, theme: &Theme, lineage_present: bool) -> Vec<Vec<Span<'static>>> {
    let agents_hint = if state.agent_view_open {
        "/agents to hide"
    } else {
        "/agents to view"
    };
    // V6: the footer names KEYS, not commands. It used to enumerate
    // `/help`, `/thinking`, and `/timestamps`; the user's note was that a
    // footer should not be a command list. `/help` stays as the single
    // pointer -- it is the keybinding overlay (T7), so it is where the rest
    // of this information actually lives -- and the display toggles move to
    // the settings menu (V4). `/agents` keeps its affordance because it is a
    // stateful toggle whose current state the hint reports.
    let mut full = format!("Enter submit · Ctrl-E expand · /help · {agents_hint}");
    // name which agent's conversation is currently shown whenever
    // it is not the root -- the root case stays silent (an always-on
    // "focused: root" would be noise for the overwhelmingly common case).
    //
    // This item: `lineage` already names the focused agent off-root (and,
    // where relevant, its whole lineage) -- appending this note too would
    // say the same fact twice on the default line. It survives only when
    // `lineage` is NOT in the resolved field list, so an older pinned
    // `[tui.status_line] fields` config (from before this item added
    // `lineage`) does not silently lose "which agent is this?" entirely.
    if !state.is_root_focused() && !lineage_present {
        full.push_str(&format!(" · focused: {}", state.focused_agent));
    }
    let compact = format!("/help · {agents_hint}");
    vec![
        vec![Span::styled(full, theme.status_dim)],
        vec![Span::styled(compact, theme.status_dim)],
        vec![Span::styled("/help".to_string(), theme.status_dim)],
        vec![],
    ]
}

/// The `lineage` field's ladder (V5's content, relocated from
/// `view/header.rs`, ladder shape added by this item's width-aware
/// assembly). Empty (a single empty rung) while the transcript shows the
/// session's own root -- the common single-agent case stays uncluttered,
/// unchanged from the pre-move behavior. Off-root, the ladder is
/// [`LineageDetail`]'s own Full -> Compact -> Bare -- `Bare` is the floor
/// (never touches the tree at all, guaranteed cheap) and this ladder never
/// ends in an empty `Vec`: once `lineage` is showing anything at all, it
/// always names at least the focused agent's own short id.
fn lineage_ladder(state: &AppState) -> Vec<Vec<Span<'static>>> {
    if state.is_root_focused() {
        return vec![vec![]];
    }
    [
        LineageDetail::Full,
        LineageDetail::Compact,
        LineageDetail::Bare,
    ]
    .into_iter()
    .map(|detail| vec![Span::raw(agent_field(state, detail))])
    .collect()
}

/// V5: how much of the focused agent's ancestry [`agent_field`] renders,
/// tried in this order (most informative first) by [`lineage_ladder`]'s
/// rungs -- the same "shorter COMPLETE form, never a mid-word clip" shape
/// `header.rs::footer_text` already uses for the floating footer.
#[derive(Clone, Copy)]
enum LineageDetail {
    /// The whole chain, one hop per ancestor: `agent <id> via root →
    /// <hop1> → ... → <hopN>`.
    Full,
    /// Ancestors between the head and the immediate parent collapse to a
    /// `…(N)` count; the head and the LAST hop (how the focused agent
    /// itself came to be -- the single most locally relevant fact) stay
    /// named.
    Compact,
    /// The pre-V5 field with no lineage at all: `agent <id>`.
    Bare,
}

/// Builds the off-root `agent` field at `detail`'s verbosity (V5, relocated
/// here by this item -- see the module doc). `Bare` never touches the tree at
/// all (guaranteed cheap, and the guaranteed-fits fallback). `Full`/ `Compact`
/// walk [`agents::ancestor_chain`] (bounded, so untrusted depth cannot run
/// away) and label each hop with [`agents::hop_label`] -- the SAME provenance
/// text `view/agents.rs`'s panel row already shows for that node, so the
/// breadcrumb and the panel can never disagree about how a given agent came to
/// exist. A node with `kind: None` (the root itself, or one seeded out-of-band
/// via `ensure_agent_tracked`, which never saw a spawn event) has no recipe
/// text to show; `hop_label` already falls back to that node's own short id
/// there, so it renders as "here's WHO" rather than being mislabeled as a fork
/// or a spawn it never was.
///
/// **Never the ancestor's actual transcript content, only metadata** -- a
/// spawn child inherits nothing from its parent, and rendering parent
/// content next to it would show the user information the agent itself
/// never saw. This is the same trap [`crate::tui::state::AppState::focus_agent`]'s
/// own doc discusses; staying at metadata here sidesteps it for both fork
/// and spawn children uniformly rather than risking getting the distinction
/// wrong.
fn agent_field(state: &AppState, detail: LineageDetail) -> String {
    let bare = format!("agent {}", agents::short_agent_id(state.focused_agent));
    if matches!(detail, LineageDetail::Bare) {
        return bare;
    }

    // Root-first: `chain[0]` is the topmost ancestor reached (normally the
    // session root itself), `chain.last()` is the focused agent.
    let chain = agents::ancestor_chain(state, state.focused_agent);
    if chain.len() <= 1 {
        // The focused agent has no tree node at all (should not happen for
        // anything `focus_agent` was actually called with, but
        // `is_focused_agent_live` already fails open for this same case
        // elsewhere) -- nothing to walk, so no lineage claim is made.
        return bare;
    }

    let hop_text = |id: &AgentId| -> String {
        state
            .tree
            .nodes
            .iter()
            .find(|n| n.agent_id == *id)
            .map(agents::hop_label)
            .unwrap_or_else(|| agents::short_agent_id(*id))
    };
    // The chain's own head reads as literal "root" when it actually IS the
    // session root (the overwhelmingly common case, and already named by
    // the status line's own leading `session <id>` field) -- otherwise (the
    // walk was cut short by that bound or a missing node) it gets the
    // same hop treatment as everything after it, so a truncated chain
    // still names what it actually reached instead of silently claiming
    // "root".
    let head = if chain[0] == state.root_agent() {
        "root".to_string()
    } else {
        hop_text(&chain[0])
    };
    let hops: Vec<String> = chain[1..].iter().map(hop_text).collect();

    let via = match detail {
        LineageDetail::Full => std::iter::once(head)
            .chain(hops)
            .collect::<Vec<_>>()
            .join(" → "),
        LineageDetail::Compact if hops.len() <= 1 => std::iter::once(head)
            .chain(hops)
            .collect::<Vec<_>>()
            .join(" → "),
        LineageDetail::Compact => {
            let omitted = hops.len() - 1;
            format!(
                "{head} → …({omitted}) → {}",
                hops.last().expect("hops.len() > 1 checked above")
            )
        }
        LineageDetail::Bare => unreachable!("returned above"),
    };

    format!("{bare} via {via}")
}

/// A short, human-readable phrase for [`Activity`] (module doc's "primary
/// 'is it working?' signal"). Uses an ellipsis (`…`) for the working states,
/// matching the T2 spec's `⠋ thinking…` shape.
fn activity_phrase(activity: &Activity) -> String {
    match activity {
        Activity::Idle => "idle".to_string(),
        Activity::Thinking => "thinking…".to_string(),
        Activity::Responding => "responding…".to_string(),
        Activity::RunningTool(name) => format!("running {name}…"),
        Activity::AwaitingPermission => "awaiting permission…".to_string(),
    }
}

/// The focused agent's cumulative token spend as one plain integer: every
/// `Usage` field summed (input + output + both cache dimensions +
/// reasoning) -- all of them are tokens the model actually processed for
/// this agent's own turns, not a single privileged subset. Deliberately NOT
/// `ContextReport.total_tokens_est` (that number is context-WINDOW
/// occupancy, a different question -- see `SessionHandle::session_usage`'s
/// own doc).
fn spent_tokens(usage: &conway::Usage) -> u64 {
    u64::from(usage.input_tokens)
        + u64::from(usage.output_tokens)
        + u64::from(usage.cache_read_tokens)
        + u64::from(usage.cache_write_tokens)
        + u64::from(usage.reasoning_tokens)
}

/// Compact token-count formatting for the `ctx` field's raw-tokens
/// fallback (unknown max context): `< 1000` renders as-is, `>= 1000`
/// renders as `{k}.{tenths}k` (e.g. `12345` -> `12.3k`). Keeps the field
/// short for very large context windows.
fn compact_tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    let k = n / 1000;
    let tenths = (n % 1000) / 100;
    format!("{k}.{tenths}k")
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use conway::config::schema::StatusLineConfig;
    use conway::AgentId;

    use super::*;

    fn cfg(fields: &[&str]) -> StatusLineConfig {
        StatusLineConfig {
            fields: fields.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn status_line_reports_ready_by_default() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        assert!(line.contains("ready"));
    }

    #[test]
    fn status_line_reflects_agent_view_toggle() {
        let mut state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("/agents to view"));
        state.toggle_agent_view();
        assert!(status_line(&state).contains("/agents to hide"));
    }

    // the focused agent must be clearly indicated.
    #[test]
    fn status_line_says_nothing_extra_while_focused_on_root() {
        let state = AppState::new(AgentId::new());
        assert!(!status_line(&state).contains("focused:"));
    }

    /// This item reconciled the overlap: the default field list now
    /// includes `lineage`, which already names the focused agent off-root,
    /// so `hint`'s own `focused: <id>` note is suppressed rather than
    /// saying the same fact twice.
    #[test]
    fn status_line_names_the_focused_agent_once_switched_off_root() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);
        let line = status_line(&state);
        assert!(
            line.contains(&format!("agent {}", agents::short_agent_id(child))),
            "the `lineage` field must name the focused agent off-root: {line}"
        );
        assert!(
            !line.contains("focused:"),
            "hint's own note must be suppressed once `lineage` already says \
             the same thing: {line}"
        );
    }

    /// An older pinned `[tui.status_line] fields` config that predates
    /// `lineage` (this item) must not silently lose "which agent is this?"
    /// entirely -- `hint`'s own `focused: <id>` note survives as a fallback
    /// exactly when `lineage` is absent from the configured list.
    #[test]
    fn hint_keeps_naming_the_focused_agent_when_lineage_is_not_configured() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.status_line_config = cfg(&["mode", "hint"]);
        let child = AgentId::new();
        state.focus_agent(child);
        let line = status_line(&state);
        assert!(
            line.contains("focused:"),
            "a config without `lineage` must keep hint's own fallback note: {line}"
        );
        assert!(line.contains(&child.to_string()), "{line}");
    }

    // --: activity indicator + token
    // spend, both scoped to the focused agent. ----

    #[test]
    fn status_line_shows_idle_by_default() {
        let state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("idle"));
    }

    #[test]
    fn status_line_reflects_every_activity_state() {
        let mut state = AppState::new(AgentId::new());

        state.activity = Activity::Thinking;
        assert!(status_line(&state).contains("thinking"));

        state.activity = Activity::Responding;
        assert!(status_line(&state).contains("responding"));

        state.activity = Activity::RunningTool("bash".to_string());
        let line = status_line(&state);
        assert!(line.contains("running"));
        assert!(line.contains("bash"));

        state.activity = Activity::AwaitingPermission;
        assert!(status_line(&state).contains("awaiting permission"));

        state.activity = Activity::Idle;
        assert!(status_line(&state).contains("idle"));
    }

    #[test]
    fn status_line_reports_zero_tokens_by_default() {
        let state = AppState::new(AgentId::new());
        assert!(status_line(&state).contains("0 tok"));
    }

    #[test]
    fn status_line_reports_the_focused_agents_cumulative_token_spend() {
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            output_tokens: 23,
            cache_read_tokens: 2,
            cache_write_tokens: 0,
            reasoning_tokens: 5,
        };
        // 100 + 23 + 2 + 0 + 5
        assert!(status_line(&state).contains("130 tok"));
    }

    #[test]
    fn focusing_a_different_agent_resets_the_token_figure_and_activity() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.activity = Activity::Responding;
        state.focused_agent_usage = conway::Usage {
            input_tokens: 50,
            ..Default::default()
        };

        state.focus_agent(AgentId::new());

        assert_eq!(state.activity, Activity::Idle);
        assert_eq!(state.focused_agent_usage, conway::Usage::default());
        assert!(status_line(&state).contains("0 tok"));
        assert!(status_line(&state).contains("idle"));
    }

    // ---- T2: spinner + elapsed + new-segment tokens added this turn ----

    #[test]
    fn status_line_shows_elapsed_and_running_tokens_while_active() {
        let mut state = AppState::new(AgentId::new());
        state.activity = Activity::Thinking;
        // 12s ago -- the elapsed renderer computes `Instant::now() - turn_started_at`.
        state.turn_started_at = Some(Instant::now() - Duration::from_secs(12));
        state.turn_running_tokens = 320;

        let line = status_line(&state);
        assert!(
            line.contains("12s"),
            "the working indicator must render live elapsed seconds while active: {line}"
        );
        assert!(
            line.contains("+320 tok"),
            "the working indicator must render the new-segment tokens with a `+` prefix while active: {line}"
        );
        assert!(
            line.contains("thinking"),
            "the activity word must still be present: {line}"
        );
        // A spinner glyph from the braille sequence must lead the activity
        // phrase.
        assert!(
            SPINNER_FRAMES.iter().any(|g| line.contains(g)),
            "the spinner glyph must precede the activity phrase: {line}"
        );
    }

    #[test]
    fn status_line_shows_no_elapsed_or_running_tokens_while_idle() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        // No "Ns ·" elapsed pattern and no spinner glyph while idle.
        assert!(
            !SPINNER_FRAMES.iter().any(|g| line.contains(g)),
            "no spinner glyph while idle: {line}"
        );
        // The idle activity slot is just `idle` -- no `+`-prefixed
        // new-token figure and no `<Ns> ·` elapsed prefix. (The hint field
        // itself uses ` · ` as a separator between affordances, so we
        // assert against the activity-specific `· +` pattern instead of
        // the bare ` · ` the T2 test used before the hint grew `·`.)
        assert!(
            !line.contains("· +"),
            "no `· +N tok` new-segment figure while idle: {line}"
        );
        // No `Ns ·` elapsed prefix: a `\d+s ·` pattern only the activity
        // field's elapsed tail produces.
        assert!(
            !line.contains("s · +"),
            "no `Ns · +N tok` elapsed/tail while idle: {line}"
        );
    }

    // ---- T3: ordered configurable field set ----

    #[test]
    fn default_field_order_is_mode_model_ctx_tokens_activity_hint() {
        // The default Lean line: mode | model | ctx | tokens | activity | hint.
        // `model` is omitted before the first ModelDecision, so the default
        // state's line is `ready | ctx 0% | 0 tok | idle | <hint>`.
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        let ready = line.find("ready").unwrap();
        let ctx = line.find("ctx").unwrap();
        let tok = line.find("0 tok").unwrap();
        let idle = line.find("idle").unwrap();
        let hint = line.find("Ctrl-E").unwrap();
        // `model` is omitted (no ModelDecision yet) -- assert it's absent.
        assert!(!line.contains("anthropic/"));
        // Order: ready < ctx < tok < idle < hint.
        assert!(ready < ctx, "mode precedes ctx: {line}");
        assert!(ctx < tok, "ctx precedes tokens: {line}");
        assert!(tok < idle, "tokens precedes activity: {line}");
        assert!(idle < hint, "activity precedes hint: {line}");
    }

    #[test]
    fn each_enabled_field_renders_in_configured_order() {
        // Reverse the order and add git/cwd -- every present field must
        // appear, in the configured order.
        let mut state = AppState::new(AgentId::new());
        state.focused_model = Some("anthropic/claude-sonnet-4-6".to_string());
        state.focused_model_max_context = Some(200_000);
        state.focused_ctx_tokens = 50_000; // 25%
        state.git_branch = Some("main".to_string());
        state.cwd_display = Some("/home/user/conway".to_string());
        state.status_line_config = cfg(&[
            "cwd", "git", "hint", "activity", "tokens", "ctx", "model", "mode",
        ]);

        let line = status_line(&state);
        let cwd = line.find("/home/user/conway").unwrap();
        let git = line.find("main").unwrap();
        let hint = line.find("Ctrl-E").unwrap();
        let idle = line.find("idle").unwrap();
        let tok = line.find("0 tok").unwrap();
        let ctx = line.find("ctx 25%").unwrap();
        let model = line.find("anthropic/claude-sonnet-4-6").unwrap();
        let mode = line.find("ready").unwrap();
        assert!(cwd < git, "{line}");
        assert!(git < hint, "{line}");
        assert!(hint < idle, "{line}");
        assert!(idle < tok, "{line}");
        assert!(tok < ctx, "{line}");
        assert!(ctx < model, "{line}");
        assert!(model < mode, "{line}");
    }

    #[test]
    fn disabled_field_is_omitted() {
        // Drop `tokens` from the configured fields -- it must NOT render.
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            ..Default::default()
        };
        state.status_line_config = cfg(&["mode", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(
            !line.contains("100 tok"),
            "a disabled field must not render: {line}"
        );
        assert!(
            !line.contains("ctx"),
            "a disabled field must not render: {line}"
        );
    }

    #[test]
    fn missing_git_field_is_omitted_gracefully() {
        // No git branch set -> `git` field is omitted even when configured.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["mode", "git", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        // No dangling separator where `git` would have been: the line has
        // exactly one ` | ` (between mode and activity).
        assert_eq!(
            line.matches(" | ").count(),
            1,
            "missing field must not leave a dangling separator: {line}"
        );
    }

    #[test]
    fn missing_model_field_is_omitted_before_first_model_decision() {
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["mode", "model", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(
            !line.contains("anthropic/"),
            "model must be omitted before the first ModelDecision: {line}"
        );
        assert_eq!(
            line.matches(" | ").count(),
            1,
            "omitted model must not leave a dangling separator: {line}"
        );
    }

    #[test]
    fn tokens_field_with_cache_data_renders_cache_percentage() {
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 300,
            cache_write_tokens: 100,
            reasoning_tokens: 0,
        };
        // total = 100 + 50 + 300 + 100 + 0 = 550
        // cache% = 300 / (100 + 300 + 100) = 300 / 500 = 60%
        let line = status_line(&state);
        assert!(
            line.contains("550 tok (60% cached)"),
            "tokens field must render total + cache%%: {line}"
        );
    }

    #[test]
    fn tokens_field_without_cache_data_renders_bare_total() {
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            output_tokens: 23,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 5,
        };
        // No cache denominator -> bare total, no parenthetical.
        let line = status_line(&state);
        assert!(
            line.contains("128 tok"),
            "tokens field must render bare total: {line}"
        );
        assert!(
            !line.contains("cached"),
            "no `cached` parenthetical without cache activity: {line}"
        );
    }

    #[test]
    fn tokens_field_cache_only_write_still_no_parenthetical() {
        // cache_write but no cache_read -> cache_read is 0, so the
        // parenthetical is suppressed (no hits to report a rate from).
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 100,
            cache_write_tokens: 200,
            ..Default::default()
        };
        let line = status_line(&state);
        assert!(line.contains("300 tok"), "total renders: {line}");
        assert!(
            !line.contains("cached"),
            "no `cached` parenthetical when cache_read is 0: {line}"
        );
    }

    #[test]
    fn ctx_field_renders_percentage_when_max_known() {
        let mut state = AppState::new(AgentId::new());
        state.focused_model_max_context = Some(200_000);
        state.focused_ctx_tokens = 50_000; // 25%
        let line = status_line(&state);
        assert!(line.contains("ctx 25%"), "{line}");
    }

    #[test]
    fn ctx_field_renders_raw_tokens_when_max_unknown() {
        let mut state = AppState::new(AgentId::new());
        state.focused_ctx_tokens = 12_345;
        let line = status_line(&state);
        assert!(
            line.contains("ctx 12.3k"),
            "raw-tokens fallback must compact-format: {line}"
        );
        assert!(!line.contains("ctx 12.3k%"), "{line}");
    }

    #[test]
    fn ctx_field_caps_at_100_percent_when_estimate_exceeds_max() {
        let mut state = AppState::new(AgentId::new());
        state.focused_model_max_context = Some(1000);
        state.focused_ctx_tokens = 5_000; // would be 500%
        let line = status_line(&state);
        assert!(
            line.contains("ctx 100%"),
            "ctx%% must cap at 100, not show 500%%: {line}"
        );
    }

    #[test]
    fn ctx_field_small_count_renders_without_suffix() {
        let mut state = AppState::new(AgentId::new());
        state.focused_ctx_tokens = 750;
        // Drop the `tokens` field so the only `tok`/`k`-bearing text is the
        // ctx field itself -- the hint and tokens field both contain `k`.
        state.status_line_config = cfg(&["mode", "ctx", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ctx 750"), "{line}");
        assert!(
            !line.contains("ctx 750k"),
            "small counts must not get a `k` suffix: {line}"
        );
    }

    #[test]
    fn override_reorders_and_hides_fields() {
        // A custom order that drops `activity` and `hint`, putting `tokens`
        // first.
        let mut state = AppState::new(AgentId::new());
        state.focused_agent_usage = conway::Usage {
            input_tokens: 10,
            ..Default::default()
        };
        state.status_line_config = cfg(&["tokens", "mode", "ctx"]);
        let line = status_line(&state);
        let tok = line.find("10 tok").unwrap();
        let mode = line.find("ready").unwrap();
        let ctx = line.find("ctx").unwrap();
        assert!(tok < mode, "{line}");
        assert!(mode < ctx, "{line}");
        assert!(!line.contains("idle"), "activity dropped: {line}");
        assert!(!line.contains("Ctrl-E"), "hint dropped: {line}");
    }

    #[test]
    fn unknown_field_names_are_dropped_silently() {
        // Unknown names never panic; the known ones still render.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["mode", "bogus", "nonsense", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(!line.contains("bogus"));
    }

    #[test]
    fn empty_fields_falls_back_to_default_order() {
        // An empty `fields` list falls back to the Lean order rather
        // than rendering a blank line.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&[]);
        let line = status_line(&state);
        assert!(line.contains("ready"));
        assert!(line.contains("idle"));
        assert!(line.contains("Ctrl-E"));
    }

    /// V6: the footer names KEYS, not commands. It used to enumerate
    /// `/thinking` and `/timestamps` alongside `/help`; the user's note was
    /// that a footer should not be a command list. `/help` survives as the
    /// single pointer -- it is the keybinding overlay, so it is where the
    /// rest of this actually lives.
    #[test]
    fn hint_field_names_keys_not_a_command_list() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);

        assert!(line.contains("Enter submit"), "{line}");
        assert!(line.contains("Ctrl-E expand"), "{line}");
        assert!(line.contains("/help"), "{line}");
        assert!(line.contains("/agents"), "{line}");

        // The enumeration is gone. These remain reachable via /help and the
        // settings menu, but the footer no longer lists them.
        assert!(
            !line.contains("/thinking"),
            "the footer must not enumerate display toggles: {line}"
        );
        assert!(
            !line.contains("/timestamps"),
            "the footer must not enumerate display toggles: {line}"
        );
    }

    #[test]
    fn git_and_cwd_fields_render_when_set() {
        let mut state = AppState::new(AgentId::new());
        state.git_branch = Some("feature-branch".to_string());
        state.cwd_display = Some("/Users/dan/conway".to_string());
        state.status_line_config = cfg(&["mode", "git", "cwd", "activity"]);
        let line = status_line(&state);
        assert!(line.contains("feature-branch"), "{line}");
        assert!(line.contains("/Users/dan/conway"), "{line}");
    }
    /// V6: the spinner no longer pulses. T2 cycled a palette index on every
    /// tick so the glyph and activity word changed color; that read as
    /// strobing. The frame still advances -- motion is the liveness cue --
    /// but the style is now constant.
    ///
    /// Asserted at the RENDER level rather than on state, because "does not
    /// pulse" is a claim about what the user sees: a state-only test would
    /// still pass if some other tick-driven style crept back in.
    #[test]
    fn consecutive_ticks_advance_the_frame_without_changing_the_style() {
        let mut state = AppState::new(AgentId::new());
        state.activity = Activity::Thinking;
        let theme = Theme::default();

        let mut styles = Vec::new();
        let mut glyphs = Vec::new();
        for _ in 0..4 {
            let line = status_line_spans(&state, &theme, WIDE);
            // The spinner span is the one carrying a braille frame.
            let span = line
                .spans
                .iter()
                .find(|s| SPINNER_FRAMES.iter().any(|f| s.content.starts_with(f)))
                .expect("a spinner span must render while animating");
            styles.push(span.style);
            glyphs.push(span.content.to_string());
            state.tick_animation();
        }

        assert!(
            styles.windows(2).all(|w| w[0] == w[1]),
            "the spinner style must not change between ticks (no pulse): {styles:?}"
        );
        assert!(
            glyphs.windows(2).any(|w| w[0] != w[1]),
            "the braille frame must still advance -- motion is the liveness cue: {glyphs:?}"
        );
    }

    /// V2: auto-allow must never be ambiguous. An operator who has
    /// forgotten they are in it, and believes they are still being asked,
    /// is the failure this mode most needs to avoid -- so the label is
    /// emphatic and always present.
    #[test]
    fn the_status_line_names_a_non_default_permission_mode() {
        let mut state = AppState::new(AgentId::new());

        // Prompt is the default and is deliberately NOT named: labelling
        // the ordinary case every frame trains the eye to skip the field.
        assert!(
            !status_line(&state).contains("prompt"),
            "the default mode is not named: {}",
            status_line(&state)
        );

        state.permission_mode = PermissionMode::AutoAllow;
        assert!(
            status_line(&state).contains("AUTO-ALLOW"),
            "auto-allow must be unmistakable: {}",
            status_line(&state)
        );

        state.permission_mode = PermissionMode::Plan;
        assert!(
            status_line(&state).contains("plan"),
            "plan mode must be visible: {}",
            status_line(&state)
        );
    }

    /// V7: AUTO-ALLOW is the one status the palette gives real color to
    /// (`theme.fatal_error`, red + bold) -- it is a genuine safety signal,
    /// not decoration. `plan` merely restricts what runs, so it keeps the
    /// plain `theme.emphasized` (bold, no color) treatment.
    #[test]
    fn auto_allow_renders_with_fatal_error_style_plan_does_not() {
        let theme = Theme::default();
        let mut state = AppState::new(AgentId::new());

        state.permission_mode = PermissionMode::AutoAllow;
        let spans = status_line_spans(&state, &theme, WIDE).spans;
        let auto_allow_span = spans
            .iter()
            .find(|s| s.content.as_ref() == "AUTO-ALLOW")
            .expect("AUTO-ALLOW span must be present");
        assert_eq!(
            auto_allow_span.style, theme.fatal_error,
            "AUTO-ALLOW must render with the palette's highest-alert accent"
        );

        state.permission_mode = PermissionMode::Plan;
        let spans = status_line_spans(&state, &theme, WIDE).spans;
        let plan_span = spans
            .iter()
            .find(|s| s.content.as_ref() == "plan")
            .expect("plan span must be present");
        assert_eq!(
            plan_span.style, theme.emphasized,
            "plan mode is a restriction, not a danger -- it keeps the plain bold treatment"
        );
        assert_ne!(
            plan_span.style, theme.fatal_error,
            "plan must not share AUTO-ALLOW's alert color"
        );
    }

    // ---- This item: `session`/`lineage` fields (correcting T6's
    // requirement miss -- see `view/header.rs`'s module doc). ----

    fn push_node(
        state: &mut AppState,
        id: AgentId,
        parent: AgentId,
        agent_def: Option<&str>,
        kind: Option<conway::SubagentMode>,
        inherited_upto: Option<conway::LogSeq>,
    ) {
        use crate::tui::state::{NodeStatus, TreeNode};
        state.tree.nodes.push(TreeNode {
            agent_id: id,
            parent: Some(parent),
            agent_def: agent_def.map(str::to_string),
            status: NodeStatus::Running,
            kind,
            inherited_upto,
            ephemeral: false,
        });
    }

    #[test]
    fn session_field_always_renders_the_roots_short_id() {
        let root = AgentId::new();
        let state = AppState::new(root);
        let line = status_line(&state);
        assert!(
            line.contains(&format!("session {}", agents::short_agent_id(root))),
            "{line}"
        );
    }

    /// Once `session_head_seq` is
    /// known, the `session` field widens to `session <id>@<seq>` -- the
    /// exact `<session-id>[@<seq>]` notation `session_ref.rs`'s own
    /// `--fork-from` flag already established, and the number
    /// `/conway.history.rewind <seq>` (`conway-plugin-history`) takes.
    #[test]
    fn session_field_appends_the_head_seq_once_known() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.session_head_seq = Some(conway::LogSeq(42));
        let line = status_line(&state);
        assert!(
            line.contains(&format!("session {}@42", agents::short_agent_id(root))),
            "{line}"
        );
    }

    /// Before the first authoritative fetch has ever completed (a fresh
    /// `AppState`, `session_head_seq` still `None`), the field degrades to
    /// exactly its pre-this-item bare form -- never `@0` or any other
    /// invented placeholder.
    #[test]
    fn session_field_omits_seq_while_head_is_unknown() {
        let root = AgentId::new();
        let state = AppState::new(root);
        assert_eq!(state.session_head_seq, None);
        let line = status_line(&state);
        assert!(!line.contains('@'), "{line}");
    }

    #[test]
    fn lineage_field_is_empty_while_root_focused() {
        let state = AppState::new(AgentId::new());
        let line = status_line(&state);
        assert!(
            !line.contains("agent "),
            "root-focused must not show a redundant `agent <id>` field: {line}"
        );
    }

    #[test]
    fn lineage_field_shows_the_focused_agent_off_root() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let child = AgentId::new();
        state.focus_agent(child);

        let line = status_line(&state);
        assert!(
            line.contains(&format!("agent {}", agents::short_agent_id(child))),
            "{line}"
        );
    }

    /// The item's most important lineage test, ported unchanged from
    /// `view/header.rs` (pre-move): a fork child and a spawn child are
    /// distinguished, and the spawn child's lineage never shows the
    /// parent's actual content -- only recipe metadata.
    #[test]
    fn fork_and_spawn_children_are_distinguished_and_spawn_shows_no_parent_content() {
        use conway::{LogSeq, SubagentMode};

        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.transcript.push(crate::tui::state::Entry::Assistant {
            text: "PARENT-SECRET-6f2c".to_string(),
            model: None,
            summary: None,
            ts: None,
        });

        let fork_child = AgentId::new();
        push_node(
            &mut state,
            fork_child,
            root,
            None,
            Some(SubagentMode::Fork),
            Some(LogSeq(5)),
        );
        let spawn_child = AgentId::new();
        push_node(
            &mut state,
            spawn_child,
            root,
            Some("reviewer"),
            Some(SubagentMode::Spawn),
            None,
        );

        state.focus_agent(fork_child);
        let fork_text = status_line(&state);
        assert!(fork_text.contains("fork @seq 5"), "{fork_text}");
        assert!(!fork_text.contains("PARENT-SECRET"), "{fork_text}");

        state.focus_agent(spawn_child);
        let spawn_text = status_line(&state);
        assert!(spawn_text.contains("@reviewer"), "{spawn_text}");
        assert!(!spawn_text.contains("PARENT-SECRET"), "{spawn_text}");
    }

    /// A deep ancestry chain degrades to a shorter COMPLETE form rather than
    /// being clipped mid-word, and never panics -- pinning `lineage` alone
    /// in the field list isolates the width budget to just this field, the
    /// same way `header.rs`'s pre-move test used the header's own
    /// single-field row as its whole budget.
    #[test]
    fn deep_ancestry_chain_degrades_to_the_compact_ellipsis_form() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.status_line_config = cfg(&["lineage"]);
        let mut cursor = root;
        for i in 0..40 {
            let next = AgentId::new();
            push_node(
                &mut state,
                next,
                cursor,
                Some(format!("agent{i}").as_str()),
                Some(conway::SubagentMode::Spawn),
                None,
            );
            cursor = next;
        }
        state.focus_agent(cursor);

        let text = status_line_spans(&state, &Theme::default(), WIDE);
        let text = flatten(&text);
        assert!(
            text.contains("…(39)"),
            "a 40-hop chain must collapse to the compact ellipsis form even \
             at a generous width: {text}"
        );
        assert!(text.contains("@agent39"), "{text}");
        assert!(!text.contains("@agent0"), "{text}");

        // Narrower widths degrade further, all the way to the bare field --
        // never a fragment, never a panic.
        for width in [60u16, 30, 15, 0] {
            let narrow = flatten(&status_line_spans(&state, &Theme::default(), width));
            assert!(
                !narrow.trim().is_empty() || width == 0,
                "a deep chain must still render something at width {width}"
            );
        }

        // Also exercise it end to end through the real render pass (would
        // panic on any width-arithmetic underflow).
        let backend = ratatui::backend::TestBackend::new(20, 3);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, 20, 1), &state, &Theme::default()))
            .expect("draw must not panic on a deep chain at a narrow width");
    }

    /// The narrowest candidate (`Bare`, e.g. `agent <id>`) is always tried
    /// last and is what a too-narrow terminal falls back to -- it is a
    /// complete field on its own, not a fragment of a longer one.
    #[test]
    fn narrow_width_falls_back_to_the_bare_agent_field_not_a_fragment() {
        use conway::SubagentMode;

        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.status_line_config = cfg(&["lineage"]);
        let child = AgentId::new();
        push_node(
            &mut state,
            child,
            root,
            Some("a-fairly-long-agent-definition-name"),
            Some(SubagentMode::Spawn),
            None,
        );
        state.focus_agent(child);

        let text = flatten(&status_line_spans(&state, &Theme::default(), 18));
        assert!(
            text.contains(&format!("agent {}", agents::short_agent_id(child))),
            "even the narrowest form must still name the focused agent: {text}"
        );
        assert!(
            !text.contains("via"),
            "a too-narrow width must fall back to the plain `agent <id>` \
             field, not a truncated lineage form: {text}"
        );
    }

    /// A node with `kind: None` between the root and the focused agent must
    /// render sensibly -- its own short id -- rather than being mislabeled
    /// as a fork or a spawn.
    #[test]
    fn a_kindless_ancestor_renders_its_short_id_not_a_mislabeled_recipe() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        let untracked_kind = AgentId::new();
        push_node(&mut state, untracked_kind, root, None, None, None);
        let focused = AgentId::new();
        push_node(
            &mut state,
            focused,
            untracked_kind,
            None,
            Some(conway::SubagentMode::Spawn),
            None,
        );
        state.focus_agent(focused);

        let text = status_line(&state);
        assert!(
            text.contains(&agents::short_agent_id(untracked_kind)),
            "{text}"
        );
        assert!(!text.contains("fork"), "{text}");
    }

    // ---- Width-aware status line assembly (review finding) ----
    //
    // The prior narrow-width coverage all pinned `fields = ["lineage"]`
    // alone, so none of it ever exercised the DEFAULT field order under
    // real width pressure -- this is exactly the gap the review flagged:
    // every field but `lineage` rendered unconditionally, so the terminal
    // silently clipped whatever fell off the right edge once `session`/
    // `lineage` pushed the default line's full length past ~86 chars.

    /// `hint` must retain content at 80 columns (the reviewer's own
    /// benchmark width) under the DEFAULT field order and default state --
    /// this is the direct regression test for the finding.
    #[test]
    fn hint_retains_content_at_80_columns_under_the_default_field_order() {
        let state = AppState::new(AgentId::new());
        let line = flatten(&status_line_spans(&state, &Theme::default(), 80));
        assert!(
            line.chars().count() <= 80,
            "the assembled line must fit the requested width, not just be \
             clipped by the terminal: {line:?} ({} chars)",
            line.chars().count()
        );
        assert!(
            line.contains("/help"),
            "hint must still point at /help at 80 columns: {line:?}"
        );
        assert!(
            line.contains("/agents"),
            "hint must still name the /agents toggle at 80 columns: {line:?}"
        );
    }

    /// The full width matrix from the review (40/80/100/200) under the
    /// default field order and default (idle, root-focused, no model)
    /// state: at every width, the assembled line fits, and `mode` (`ready`,
    /// the only field guaranteed never to be dropped) is always present.
    #[test]
    fn default_field_order_fits_and_keeps_mode_at_every_reviewed_width() {
        let state = AppState::new(AgentId::new());
        for width in [40u16, 80, 100, 200] {
            let line = flatten(&status_line_spans(&state, &Theme::default(), width));
            assert!(
                line.chars().count() <= width as usize,
                "width {width}: line must fit, not silently clip: {line:?} \
                 ({} chars)",
                line.chars().count()
            );
            assert!(
                line.contains("ready"),
                "width {width}: `mode` must never be the field that gives \
                 way: {line:?}"
            );
        }
        // At the full 200-column width nothing needs to degrade at all --
        // parity with the pre-finding line (session/lineage/mode/ctx/
        // tokens/activity/hint all present).
        let wide = flatten(&status_line_spans(&state, &Theme::default(), 200));
        assert!(wide.contains("session "), "{wide}");
        assert!(wide.contains("ctx"), "{wide}");
        assert!(wide.contains("0 tok"), "{wide}");
        assert!(wide.contains("idle"), "{wide}");
        assert!(wide.contains("Enter submit"), "{wide}");
    }

    /// The hard safety guarantee: `AUTO-ALLOW` must survive at EVERY width
    /// where anything at all survives -- it is the one field
    /// (`drop_priority`'s highest rank) that is never fully dropped, and
    /// its own ladder's one degrade step keeps the label over the plain
    /// `ready`/`awaiting permission` UI word specifically so the label is
    /// what's left once space runs out.
    ///
    /// **Revised by the adversarial review (352d2f4), finding 3.** Below
    /// the floor rung's own width (the `AUTO-ALLOW` label plus its
    /// leading+trailing pad = 12 columns), the label itself cannot fit
    /// whole. The original version of this test asserted the literal
    /// substring `"AUTO-ALLOW"` at every width down to 5, which passed
    /// only because it ran against [`flatten`]'s PRE-render span content
    /// (finding 4) -- the real render (no `.wrap()` on the `Paragraph`)
    /// silently clipped the label mid-word instead (verified empirically:
    /// `" AUTO-ALLO"` at width 10, `" AUTO-AL"` at width 8, `" AUTO"` at
    /// width 5). The fix makes [`status_line_spans`] itself clamp to an
    /// explicit, character-boundary-safe `…` truncation before ever
    /// reaching a `Paragraph` (see [`clamp_to_width`]), so `flatten` and
    /// the real render can no longer disagree -- this test now asserts the
    /// HONEST claim: full text OR an explicitly `…`-marked truncation of
    /// it, never a silent bare fragment.
    #[test]
    fn auto_allow_survives_at_every_width_where_anything_survives() {
        let mut state = AppState::new(AgentId::new());
        state.permission_mode = PermissionMode::AutoAllow;

        for width in [0u16, 1, 5, 10, 12, 15, 20, 40, 80, 200] {
            let line = flatten(&status_line_spans(&state, &Theme::default(), width));
            if line.trim().is_empty() {
                continue;
            }
            let full = line.contains("AUTO-ALLOW");
            let marked_truncation = line.ends_with('…');
            assert!(
                full || marked_truncation,
                "width {width}: AUTO-ALLOW must render in full, or be \
                 explicitly truncated with a trailing `…` -- never a bare, \
                 unmarked fragment: {line:?}"
            );
        }
        // And concretely: at 12 columns (the exact width of the bare
        // `AUTO-ALLOW` rung plus its padding) it is the ONLY thing on the
        // line -- proof the degrade ladder actually reached its floor
        // rather than merely still fitting by coincidence at a wider test
        // width.
        let floor = flatten(&status_line_spans(&state, &Theme::default(), 12));
        assert_eq!(
            floor.trim(),
            "AUTO-ALLOW",
            "at the floor width, AUTO-ALLOW must be the ENTIRE line, \
             everything else already given way: {floor:?}"
        );
        // One column short of the floor: the trailing pad space is what
        // drops, not the label -- the label itself stays whole.
        let just_under = flatten(&status_line_spans(&state, &Theme::default(), 11));
        assert_eq!(just_under.trim(), "AUTO-ALLOW", "{just_under:?}");
    }

    /// Renders `draw` through a REAL `Terminal<TestBackend>` and reads the
    /// buffer's cells back -- the buffer-asserting test binding decision
    /// requires for exactly this class of
    /// safety-critical claim (adversarial review finding 4). Every prior
    /// width assertion in this module operated on [`flatten`]'s pre-render
    /// span content, which can (and did) disagree with what a `Paragraph`
    /// with no `.wrap()` actually puts on screen.
    fn render_row(state: &AppState, theme: &Theme, width: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(width.max(1), 1);
        let mut terminal = Terminal::new(backend).expect("TestBackend construction cannot fail");
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, width, 1), state, theme))
            .expect("draw must not panic");
        let buffer = terminal.backend().buffer().clone();
        (0..width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect()
    }

    /// **Finding 4, the concrete regression guard.** Against the pre-fix
    /// code, reading the REAL rendered buffer back at widths 5/8/10 shows
    /// `" AUT"`/`" AUTO-AL"`/`" AUTO-ALLO"` -- a silent mid-word clip with
    /// no sign anything was cut, exactly what the module's own "never a
    /// silent clip" doc promises will not happen. The fix
    /// ([`clamp_to_width`]) makes that truncation explicit: it always ends
    /// in `…` at these widths instead.
    #[test]
    fn auto_allow_buffer_reads_back_a_deliberate_ellipsis_not_a_silent_mid_word_clip() {
        let mut state = AppState::new(AgentId::new());
        state.permission_mode = PermissionMode::AutoAllow;
        let theme = Theme::default();

        for width in [5u16, 8, 10] {
            let rendered = render_row(&state, &theme, width);
            assert!(
                rendered.ends_with('…'),
                "width {width}: an over-length mode floor must be \
                 truncated with an explicit `…` marker, not silently \
                 mid-word clipped by the terminal: {rendered:?}"
            );
        }

        // Width 11: one column short of the floor's own width (12) -- the
        // trailing pad space drops cleanly and the label itself renders
        // whole, un-clipped.
        let at_11 = render_row(&state, &theme, 11);
        assert_eq!(at_11.trim(), "AUTO-ALLOW", "{at_11:?}");

        // Width 12+: the floor rung (label + both pad spaces) fits exactly
        // -- the safety label renders whole, no truncation needed.
        let at_12 = render_row(&state, &theme, 12);
        assert_eq!(at_12.trim(), "AUTO-ALLOW", "{at_12:?}");
    }

    /// **Finding 2, the concrete regression guard.** `ladder_width` used to
    /// measure `.content.chars().count()` -- one CJK character counts as
    /// one char but renders as TWO terminal columns, so the arithmetic
    /// could be wrong by up to 2x. `lineage` embeds `agent_def` names
    /// (`"@{def}"`, `view/agents.rs`'s `recipe_parts`) verbatim -- arbitrary
    /// user-chosen text with no ASCII restriction -- and `lineage` sits
    /// directly before `mode` in the default give-up order, so an
    /// undercounted `lineage` rung used to leave the assembly thinking it
    /// had more room than it did, and the overflow landed on -- and
    /// mid-word-clipped -- the `AUTO-ALLOW` safety indicator instead of
    /// `lineage` degrading (or dropping) first as `drop_priority` intends.
    #[test]
    fn cjk_lineage_content_is_measured_in_columns_not_chars_and_never_clips_mode() {
        let root = AgentId::new();
        let mut state = AppState::new(root);
        state.permission_mode = PermissionMode::AutoAllow;
        let child = AgentId::new();
        // 10 CJK characters -- 10 chars, but 20 terminal columns; a
        // char-counting `ladder_width` undercounts this rung's true width
        // by half.
        push_node(
            &mut state,
            child,
            root,
            Some("称呼名字十字路口方向标识"),
            Some(conway::SubagentMode::Spawn),
            None,
        );
        state.focus_agent(child);
        // `mode` immediately follows `lineage`, and nothing follows `mode`
        // -- isolates exactly the reviewer's claim: with `lineage` sitting
        // directly before `mode` and nothing after it to absorb an
        // undercounted tail, a mismeasured `lineage` rung's overflow lands
        // squarely on the safety indicator. Empirically confirmed against
        // the pre-fix code: widths 62-72 mid-word-clip `AUTO-ALLOW` itself
        // (e.g. width 67 renders `"...ready · AUTO"`, missing `-ALLOW`
        // entirely, with no sign anything was cut) because `ladder_width`
        // undercounts the 12 double-width CJK characters in `lineage`'s
        // `Full` rung by 12 columns, so the fit-check believes `lineage`
        // can stay at `Full` when the real render cannot fit it.
        state.status_line_config = cfg(&["lineage", "mode"]);

        for width in 62u16..=72 {
            let rendered = render_row(&state, &Theme::default(), width);
            let full = rendered.contains("AUTO-ALLOW");
            let marked_truncation = rendered.trim_end().ends_with('…');
            assert!(
                full || marked_truncation,
                "width {width}: a mis-measured CJK `lineage` rung must not \
                 silently mid-word-clip the AUTO-ALLOW indicator -- it \
                 must render whole, or be explicitly `…`-truncated: \
                 {rendered:?}"
            );
        }
    }

    /// A non-default field order/config still keeps `mode` last to give up
    /// -- the width-aware assembly is not special-cased to the Lean
    /// default order.
    #[test]
    fn mode_survives_narrow_width_even_with_a_custom_field_order() {
        let mut state = AppState::new(AgentId::new());
        state.permission_mode = PermissionMode::Plan;
        state.status_line_config = cfg(&[
            "session", "hint", "activity", "tokens", "ctx", "model", "mode",
        ]);
        state.activity = Activity::Thinking;
        state.turn_running_tokens = 999;

        let narrow = flatten(&status_line_spans(&state, &Theme::default(), 15));
        assert!(
            narrow.contains("plan"),
            "the non-default permission-mode label must survive at 15 \
             columns even with a custom field order: {narrow:?}"
        );
    }

    /// The width-aware assembly never panics, at any width including
    /// 0, for the default field order.
    #[test]
    fn status_line_spans_never_panics_at_any_width() {
        let mut state = AppState::new(AgentId::new());
        state.permission_mode = PermissionMode::AutoAllow;
        state.activity = Activity::Thinking;
        for width in 0u16..=10 {
            let _ = status_line_spans(&state, &Theme::default(), width);
        }
        for width in [50u16, 106, 200, u16::MAX] {
            let _ = status_line_spans(&state, &Theme::default(), width);
        }
    }

    // ---- Adversarial review (352d2f4) of the width-degradation ladder:
    // FINDING 1 (critical) -- `AUTO-ALLOW` can be silently disabled by
    // config. `resolve_fields` accepted a configured `fields` list verbatim
    // even when it omitted `mode` entirely -- the empty-list fallback only
    // fires when the list is empty AFTER filtering unknown names, not when
    // a KNOWN field is simply absent. `mode_ladder`'s width-survival
    // guarantee is worthless if `mode` never enters the resolved list at
    // all. Fix: while a non-default `PermissionMode` is active, `mode` is
    // forced into the resolved list even when the configured `fields`
    // omits it -- see `resolve_fields`'s own doc for why this is Option 3
    // (appear only when it carries information) rather than Option 1
    // (always forced) or Option 2 (validate/refuse at config load). ----

    #[test]
    fn auto_allow_survives_a_fields_list_that_omits_mode_entirely() {
        // This is the direct regression test for the finding: a pinned
        // `fields` list that simply never mentions `mode` must not be able
        // to turn off the one genuine safety signal on the line.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["session", "hint"]);
        state.permission_mode = PermissionMode::AutoAllow;
        let line = status_line(&state);
        assert!(
            line.contains("AUTO-ALLOW"),
            "a `fields` list that omits `mode` must not silently disable \
             the AUTO-ALLOW indicator: {line}"
        );
    }

    #[test]
    fn plan_mode_also_survives_a_fields_list_that_omits_mode() {
        // Not just AutoAllow: any non-default permission mode is
        // information the operator needs, so the fix is not special-cased
        // to the single most alarming variant.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["session", "hint"]);
        state.permission_mode = PermissionMode::Plan;
        let line = status_line(&state);
        assert!(line.contains("plan"), "{line}");
    }

    #[test]
    fn default_prompt_mode_stays_out_when_a_fields_list_omits_mode() {
        // The forced-in behavior is conditional on carrying information
        // (Option 3, not Option 1): while `Prompt` (the default) is
        // active, an older/hand-pinned `fields` list that genuinely omits
        // `mode` keeps rendering exactly as configured -- no `ready`/
        // `awaiting permission` text appears out of nowhere.
        let mut state = AppState::new(AgentId::new());
        state.status_line_config = cfg(&["session", "hint"]);
        let line = status_line(&state);
        assert!(
            !line.contains("ready"),
            "the default Prompt mode must not be forced in when `fields` \
             genuinely omits `mode`: {line}"
        );
    }

    /// The env-var override path (`CONWAY_TUI__STATUS_LINE__FIELDS`) feeds
    /// `StatusLineConfig` through the exact same comma-split array-leaf-key
    /// mechanism the file-based `settings.json` `fields` key uses
    /// (`conway::config::merge`'s `ARRAY_LEAF_KEYS`) -- this proves the env
    /// path can ALSO produce a `fields` list missing `mode`, not just a
    /// hand-pinned `settings.json`, and that the fix at the render layer
    /// (not the config layer) covers both sources uniformly.
    #[test]
    fn env_var_fields_override_can_omit_mode_and_the_fix_still_covers_it() {
        let xdg_dir = tempfile::tempdir().expect("tempdir");
        let cwd_dir = tempfile::tempdir().expect("tempdir");
        let mut env = std::collections::HashMap::new();
        // Redirect the user-scoped config path into an empty tempdir so
        // this test cannot pick up a real `~/.conway/settings.json` on the
        // machine it runs on.
        env.insert(
            "XDG_CONFIG_HOME".to_string(),
            xdg_dir.path().to_string_lossy().to_string(),
        );
        env.insert(
            "CONWAY_TUI__STATUS_LINE__FIELDS".to_string(),
            "session,hint".to_string(),
        );
        let outcome = conway::config::load(conway::config::LoadOptions {
            cwd: cwd_dir.path().to_path_buf(),
            explicit_path: None,
            env,
            cli_overrides: conway::config::CliOverrides::default(),
            model_metadata_refresh: false,
        })
        .expect("load must succeed");

        assert_eq!(
            outcome.config.tui.status_line.fields,
            vec!["session".to_string(), "hint".to_string()],
            "the env override must actually produce a `fields` list \
             omitting `mode` -- proving this is a reachable real config, \
             not just a hypothetical one"
        );

        let mut state = AppState::new(AgentId::new());
        state.status_line_config = outcome.config.tui.status_line;
        state.permission_mode = PermissionMode::AutoAllow;
        let line = status_line(&state);
        assert!(
            line.contains("AUTO-ALLOW"),
            "AUTO-ALLOW must survive even when the env-sourced `fields` \
             list omits `mode` entirely: {line}"
        );
    }
}

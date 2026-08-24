//! Shared rendering helpers for the `sessions`/`routes` subcommands
//!: table layout, ASCII tree layout, and small id/timestamp
//! formatters, kept in one place so the two subcommand families render
//! consistently rather than each inventing its own table/tree shape.

use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};

/// A cell longer than this is truncated with a trailing `…` (character
/// count, not byte count, so multi-byte UTF-8 is never split mid-character).
const MAX_CELL_CHARS: usize = 48;

/// Renders a left-aligned table: a two-space gutter between columns, each
/// column sized to the widest cell in it (header included), overlong cells
/// truncated to `MAX_CELL_CHARS`. Always renders the header row, even
/// with zero data rows -- this is what makes `sessions list`'s "empty store
/// prints header only" contract hold without a special case at the call
/// site.
pub fn table(headers: &[&str], rows: Vec<Vec<String>>) -> String {
    let ncols = headers.len();
    let header_cells: Vec<String> = headers.iter().map(|h| truncate_cell(h)).collect();
    let rows: Vec<Vec<String>> = rows
        .into_iter()
        .map(|row| row.iter().map(|c| truncate_cell(c)).collect())
        .collect();

    let mut widths: Vec<usize> = header_cells.iter().map(|c| c.chars().count()).collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate().take(ncols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let mut out = String::new();
    push_row(&mut out, &header_cells, &widths);
    for row in &rows {
        push_row(&mut out, row, &widths);
    }
    out
}

fn truncate_cell(s: &str) -> String {
    if s.chars().count() > MAX_CELL_CHARS {
        let head: String = s.chars().take(MAX_CELL_CHARS.saturating_sub(1)).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

/// Writes one row: every column but the last is left-padded to its column
/// width plus a two-space gutter; the last column is written unpadded (no
/// trailing whitespace) and terminates the line.
fn push_row(out: &mut String, cells: &[String], widths: &[usize]) {
    let last = cells.len().saturating_sub(1);
    for (i, cell) in cells.iter().enumerate() {
        if i == last {
            let _ = writeln!(out, "{cell}");
        } else {
            let width = widths.get(i).copied().unwrap_or(0);
            let _ = write!(out, "{cell:<width$}  ");
        }
    }
}

/// Renders `root` and its descendants (as produced by `children_fn`) as an
/// ASCII tree: `├─ ` for a non-final child at a given depth, `└─ ` for the
/// final one, and a `│  `/`   ` prefix carried down to that child's own
/// subtree depending on whether an ancestor at that depth was final.
pub fn tree_lines<T>(
    root: &T,
    children_fn: impl Fn(&T) -> Vec<T>,
    label_fn: impl Fn(&T) -> String,
) -> Vec<String> {
    let mut out = vec![label_fn(root)];
    render_children(&children_fn(root), "", &children_fn, &label_fn, &mut out);
    out
}

fn render_children<T>(
    nodes: &[T],
    prefix: &str,
    children_fn: &impl Fn(&T) -> Vec<T>,
    label_fn: &impl Fn(&T) -> String,
    out: &mut Vec<String>,
) {
    let last_index = nodes.len().checked_sub(1);
    for (i, node) in nodes.iter().enumerate() {
        let is_last = Some(i) == last_index;
        let branch = if is_last { "└─ " } else { "├─ " };
        out.push(format!("{prefix}{branch}{}", label_fn(node)));
        let child_prefix = format!("{prefix}{}", if is_last { "   " } else { "│  " });
        render_children(
            &children_fn(node),
            &child_prefix,
            children_fn,
            label_fn,
            out,
        );
    }
}

/// RFC 3339, seconds precision, `Z`-suffixed UTC.
pub fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// The first 8 characters of an id's `Display` form -- a fixed-width
/// truncation, **not** a unique short form. A ULID's leading characters
/// encode the high bits of its 48-bit millisecond timestamp, so two ids
/// minted within roughly the same second (two sessions created back to
/// back, for instance) share these 8 characters exactly. This function
/// performs no uniqueness computation against any set of other ids -- a
/// caller that needs "the shortest prefix that still tells this row apart
/// from its neighbours" wants something else (the TUI agent panel's
/// `panel_agent_id`, `crates/conway-cli/src/tui/view/agents.rs`, is that
/// something else, extending the prefix until it is unique among the rows
/// on screen; see TREE-ID `01M0TNCAP1HH4YNC5K9753YG26`'s ruling that a
/// short id is a UI affordance and a full id is a durable reference).
///
/// Board item `01M0V03FQGJ8C375QJDD75YH41`: this used to be `sessions
/// list`'s ID column too, which made two sessions created close together
/// render identical, indistinguishable rows. `sessions list`/`sessions
/// tree` now print the full id in every position an operator might need to
/// address a specific row (the ID column, and `tree`'s per-node label);
/// this helper survives only for `sessions list`'s ORIGIN cell, which
/// names a single already-known parent as annotation -- the same "one
/// thing, not a choice among several" case `panel_agent_id`'s own doc
/// carves out for `short_agent_id`'s hop labels and status line. Do not
/// reach for this where a reader might need to tell two rows apart.
pub fn id_short(id: impl std::fmt::Display) -> String {
    id.to_string().chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_with_no_rows_prints_header_only() {
        let out = table(&["ID", "STATUS"], Vec::new());
        assert_eq!(out, "ID  STATUS\n");
    }

    #[test]
    fn table_pads_columns_to_widest_cell() {
        let out = table(
            &["ID", "STATUS"],
            vec![
                vec!["abcdef12".to_string(), "active".to_string()],
                vec!["z".to_string(), "done".to_string()],
            ],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "ID        STATUS");
        assert_eq!(lines[1], "abcdef12  active");
        assert_eq!(lines[2], "z         done");
    }

    #[test]
    fn table_truncates_overlong_cells() {
        let long = "x".repeat(60);
        let out = table(&["COL"], vec![vec![long]]);
        let data_line = out.lines().nth(1).unwrap();
        assert_eq!(data_line.chars().count(), MAX_CELL_CHARS);
        assert!(data_line.ends_with('…'));
    }

    #[test]
    fn tree_lines_uses_ascii_branch_glyphs() {
        // A tiny 3-node tree: root -> [a, b], `a` has one child `c`.
        #[derive(Clone)]
        struct Node(&'static str, Vec<&'static str>);

        fn children_fn(n: &Node) -> Vec<Node> {
            n.1.iter()
                .map(|&name| Node(name, if name == "a" { vec!["c"] } else { vec![] }))
                .collect()
        }

        let root = Node("root", vec!["a", "b"]);
        let lines = tree_lines(&root, children_fn, |n| n.0.to_string());
        assert_eq!(
            lines,
            vec![
                "root".to_string(),
                "├─ a".to_string(),
                "│  └─ c".to_string(),
                "└─ b".to_string(),
            ]
        );
    }

    // Rewritten for board item `01M0V03FQGJ8C375QJDD75YH41`: the assertion
    // is unchanged (the function still takes a fixed first 8 characters --
    // that mechanical behaviour was never the defect), but the doc comment
    // above `id_short` changed from implying a stable, human-pasteable
    // *unique* short form to stating plainly that no uniqueness is
    // computed. Kept rather than deleted so the "first 8 characters,
    // nothing more" contract stays pinned by a test, and paired with
    // `id_short_does_not_distinguish_two_ids_from_the_same_second` below so
    // the doc's warning is demonstrated, not just asserted in prose.
    #[test]
    fn id_short_takes_first_eight_chars() {
        assert_eq!(id_short("01J9ZZZZZZZZZZZZZZZZZZZZZZ"), "01J9ZZZZ");
    }

    #[test]
    fn id_short_does_not_distinguish_two_ids_from_the_same_second() {
        // Two distinct ULIDs sharing a timestamp prefix -- exactly the
        // shape two sessions created back to back produce. `id_short`
        // collapses them to the same 8 characters; this is the failure
        // mode its doc now warns callers away from, not a bug in this
        // function itself (it never claimed uniqueness -- the defect was
        // callers assuming it anyway).
        let a = "01J9ZZZZAAAAAAAAAAAAAAAAAA";
        let b = "01J9ZZZZBBBBBBBBBBBBBBBBBB";
        assert_eq!(id_short(a), id_short(b));
    }
}

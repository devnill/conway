//! A minimal, dependency-free unified diff over line-based text --
//! `/conway.checkpoint.diff`'s own preview text.
//!
//! Deliberately NOT a diff crate: this crate's own "no new dependency
//! unless a diff or three-way merge library is genuinely necessary"
//! constraint expects the answer to be no here, and a plain LCS-based line
//! diff is short enough that hand-writing and reading it costs less than
//! auditing a new dependency for something whose only job is a human-facing
//! preview.
//!
//! **A real diff-quality algorithm (Myers' O(ND)) was considered and
//! rejected** in favor of a straightforward O(n*m) LCS table specifically
//! because it is short enough to verify correct by inspection. Its
//! quadratic cost is capped explicitly ([`MAX_DIFF_CELLS`]): past that
//! bound this falls back to a coarse, honestly labeled summary rather than
//! a slow or memory-heavy exact diff. Nothing about `conway.checkpoint`'s
//! own correctness (which bytes a rollback actually writes,
//! `crate::store`) depends on this module at all -- it exists purely for
//! the preview text a human reads before deciding whether to run
//! `rollback`.

/// Above this many `(old_lines+1)*(new_lines+1)` table cells,
/// [`unified_diff`] gives up on a fine-grained diff and returns a coarse
/// summary instead -- bounded compute and memory regardless of how large a
/// snapshot the per-file bound let through.
pub const MAX_DIFF_CELLS: u64 = 4_000_000;

/// How many unchanged lines surround each changed run in the rendered
/// output -- the same "3 lines of context" convention `diff -u`'s own
/// default uses.
const CONTEXT: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpKind {
    Equal,
    Delete,
    Insert,
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split('\n').collect()
    }
}

/// LCS table, suffix-indexed: `table[i][j]` is the LCS length of
/// `a[i..]`/`b[j..]`.
fn lcs_table(a: &[&str], b: &[&str]) -> Vec<Vec<u32>> {
    let (n, m) = (a.len(), b.len());
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if a[i] == b[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    table
}

/// Walks the LCS table forward into a flat op sequence. Ties (an unchanged
/// line reachable via more than one path) break toward `Delete` before
/// `Insert` -- an arbitrary but STABLE choice. This module makes no claim
/// to the minimal/canonical diff a human would hand-pick, only a CORRECT
/// one: concatenating the `old`-side lines (`Equal`+`Delete`) reconstructs
/// `a` exactly, and the `new`-side lines (`Equal`+`Insert`) reconstruct `b`
/// exactly -- pinned by this module's own round-trip test.
fn diff_ops<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<(OpKind, &'a str)> {
    let table = lcs_table(a, b);
    let (mut i, mut j) = (0usize, 0usize);
    let mut ops = Vec::with_capacity(a.len() + b.len());
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            ops.push((OpKind::Equal, a[i]));
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            ops.push((OpKind::Delete, a[i]));
            i += 1;
        } else {
            ops.push((OpKind::Insert, b[j]));
            j += 1;
        }
    }
    while i < a.len() {
        ops.push((OpKind::Delete, a[i]));
        i += 1;
    }
    while j < b.len() {
        ops.push((OpKind::Insert, b[j]));
        j += 1;
    }
    ops
}

/// Renders a unified diff between `old_text` and `new_text`, labeled with
/// `old_label`/`new_label` (conventionally the same path twice, for the
/// `---`/`+++` headers). Empty when the two are byte-identical.
pub fn unified_diff(old_label: &str, new_label: &str, old_text: &str, new_text: &str) -> String {
    if old_text == new_text {
        return String::new();
    }
    let old_lines = split_lines(old_text);
    let new_lines = split_lines(new_text);
    let cells = (old_lines.len() as u64 + 1) * (new_lines.len() as u64 + 1);
    if cells > MAX_DIFF_CELLS {
        return format!(
            "--- {old_label}\n+++ {new_label}\n@@ diff omitted: {} old line(s), {} new \
             line(s) -- exceeds this preview's {MAX_DIFF_CELLS}-cell bound @@\n",
            old_lines.len(),
            new_lines.len()
        );
    }
    let ops = diff_ops(&old_lines, &new_lines);

    // Prefix counts: `old_before[k]`/`new_before[k]` = how many old/new
    // lines `ops[..k]` has already consumed -- lets a hunk boundary compute
    // its own starting line numbers without re-walking from zero.
    let mut old_before = vec![0usize; ops.len() + 1];
    let mut new_before = vec![0usize; ops.len() + 1];
    for (k, (kind, _)) in ops.iter().enumerate() {
        let consumes_old = if *kind != OpKind::Insert { 1 } else { 0 };
        let consumes_new = if *kind != OpKind::Delete { 1 } else { 0 };
        old_before[k + 1] = old_before[k] + consumes_old;
        new_before[k + 1] = new_before[k] + consumes_new;
    }

    let change_indices: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != OpKind::Equal)
        .map(|(k, _)| k)
        .collect();
    if change_indices.is_empty() {
        // `old_text != new_text` yet no line-level change is possible ONLY
        // for a trailing/leading empty-line split -- still honest to report
        // nothing to show at line granularity.
        return String::new();
    }

    // Merge change indices into windows, expanding each by `CONTEXT` on
    // both sides and merging any that then overlap or touch.
    let mut windows: Vec<(usize, usize)> = Vec::new();
    for idx in change_indices {
        let lo = idx.saturating_sub(CONTEXT);
        let hi = (idx + CONTEXT + 1).min(ops.len());
        match windows.last_mut() {
            Some((_, last_hi)) if lo <= *last_hi => {
                *last_hi = (*last_hi).max(hi);
            }
            _ => windows.push((lo, hi)),
        }
    }

    let mut out = format!("--- {old_label}\n+++ {new_label}\n");
    for (lo, hi) in windows {
        let old_start = old_before[lo];
        let new_start = new_before[lo];
        let old_count = old_before[hi] - old_before[lo];
        let new_count = new_before[hi] - new_before[lo];
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start + 1,
            old_count,
            new_start + 1,
            new_count
        ));
        for (kind, line) in &ops[lo..hi] {
            let marker = match kind {
                OpKind::Equal => ' ',
                OpKind::Delete => '-',
                OpKind::Insert => '+',
            };
            out.push_str(&format!("{marker}{line}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_produces_no_diff() {
        assert_eq!(unified_diff("a", "a", "same\ntext", "same\ntext"), "");
    }

    #[test]
    fn a_single_changed_line_shows_a_minus_and_a_plus() {
        let out = unified_diff("f", "f", "one\ntwo\nthree", "one\nTWO\nthree");
        assert!(out.contains("-two"), "{out}");
        assert!(out.contains("+TWO"), "{out}");
        assert!(out.contains(" one"), "{out}");
        assert!(out.contains(" three"), "{out}");
    }

    #[test]
    fn a_pure_insertion_into_an_empty_old_file_shows_every_new_line_as_added() {
        let out = unified_diff("f", "f", "", "hello\nworld");
        assert!(out.contains("+hello"), "{out}");
        assert!(out.contains("+world"), "{out}");
        assert!(
            !out.contains("\n-"),
            "an empty old side has no content line to delete: {out}"
        );
    }

    #[test]
    fn a_pure_deletion_to_an_empty_new_file_shows_every_old_line_as_removed() {
        let out = unified_diff("f", "f", "hello\nworld", "");
        assert!(out.contains("-hello"), "{out}");
        assert!(out.contains("-world"), "{out}");
    }

    /// The op sequence must reconstruct both inputs exactly -- the
    /// correctness property this module actually depends on (module doc).
    #[test]
    fn ops_reconstruct_both_inputs_exactly() {
        let a: Vec<&str> = "the quick brown fox\njumps over\nthe lazy dog"
            .split('\n')
            .collect();
        let b: Vec<&str> = "the quick fox\njumps way over\nthe lazy dog\nagain"
            .split('\n')
            .collect();
        let ops = diff_ops(&a, &b);
        let reconstructed_a: Vec<&str> = ops
            .iter()
            .filter(|(kind, _)| *kind != OpKind::Insert)
            .map(|(_, line)| *line)
            .collect();
        let reconstructed_b: Vec<&str> = ops
            .iter()
            .filter(|(kind, _)| *kind != OpKind::Delete)
            .map(|(_, line)| *line)
            .collect();
        assert_eq!(reconstructed_a, a);
        assert_eq!(reconstructed_b, b);
    }

    #[test]
    fn a_diff_beyond_the_cell_bound_falls_back_to_a_coarse_honest_summary() {
        // 3000 * 3000 lines >> MAX_DIFF_CELLS, and deliberately entirely
        // distinct on each side so a real LCS pass would do real work if it
        // ran at all.
        let old_text: String = (0..3000).map(|n| format!("old-{n}\n")).collect();
        let new_text: String = (0..3000).map(|n| format!("new-{n}\n")).collect();
        let out = unified_diff("f", "f", &old_text, &new_text);
        assert!(out.contains("diff omitted"), "{out}");
        let old_line_count = split_lines(&old_text).len();
        assert!(out.contains(&format!("{old_line_count} old line")), "{out}");
    }
}

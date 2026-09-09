//! A minimal, dependency-free unified-diff engine, shared by four call
//! sites that all need the same thing: the permission prompt's pending
//! `edit`/`write` preview (`tui/view/mod.rs`), the settled transcript
//! entry's stored diff (`tui/state/transcript.rs`), the TUI's `/diff`
//! command (`tui/commands.rs`), and `conway sessions show --diff`
//! (`commands/sessions.rs`).
//!
//! ## Why this is a SECOND file, not a dependency on
//! `conway-plugin-checkpoint::diff`
//!
//! Board item 01M1YVEJB6GAPST5YZET4KZZE2's own brief asks explicitly: read
//! `crates/conway-plugin-checkpoint/src/diff.rs` first, and prefer reusing
//! or mirroring it over hand-rolling a second, different algorithm. It was
//! read. It cannot be reused as a dependency: that crate declares `mod
//! diff;` (private, not `pub mod`), and re-exports nothing from it in its
//! own `lib.rs` (`pub use store::{...}` only) -- there is no public item to
//! depend on without editing that crate's own file, which is both outside
//! this item's owned-file set and a first-party PLUGIN crate this item has
//! no reason to widen the public surface of just to lend a private helper.
//! Depending on the whole `conway-plugin-checkpoint` crate (which also
//! pulls in `blake3`/its own `store` module, neither relevant here) to
//! reach ~150 lines of pure string math would be a heavier, stranger
//! coupling than mirroring those lines.
//!
//! So: this module is a close MIRROR of
//! `conway-plugin-checkpoint/src/diff.rs`'s own [`unified_diff`] -- the
//! same algorithm (a straightforward O(n*m) LCS table, short enough to
//! verify by inspection, never Myers/patience), the same
//! [`MAX_DIFF_CELLS`] bound with the same coarse-honest-summary fallback
//! past it, the same 3-line-of-context convention. It is NOT a second,
//! independently-designed diff -- it is the same one, copied because there
//! is no lower-friction way to share code across this workspace boundary
//! today. A future item could make `conway-plugin-checkpoint::diff` `pub`
//! and have this module re-export it instead; not done here to keep this
//! item's file-ownership fence intact.
//!
//! [`simulate_edit`] additionally mirrors `conway-tools`'s
//! `EditTool::invoke` (`crates/conway-tools/src/fs/edit.rs`) own literal,
//! byte-exact substring-replacement semantics -- never regex, never
//! whitespace-normalized -- so a simulated result never disagrees with
//! what the real tool would actually do to the file.

use std::collections::BTreeMap;

/// Above this many `(old_lines+1)*(new_lines+1)` table cells, [`unified_diff`]
/// gives up on a fine-grained diff and returns a coarse summary instead --
/// bounded compute and memory regardless of how large a file slips past
/// whatever size limits sit upstream of this module. Mirrors
/// `conway-plugin-checkpoint::diff::MAX_DIFF_CELLS` exactly (see this
/// module's own doc for why it is copied, not imported).
pub const MAX_DIFF_CELLS: u64 = 4_000_000;

/// How many unchanged lines surround each changed run in the rendered
/// output -- the same "3 lines of context" convention `diff -u` uses.
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

/// Walks the LCS table forward into a flat op sequence. Ties break toward
/// `Delete` before `Insert` -- an arbitrary but STABLE choice. Concatenating
/// the old-side ops (`Equal`+`Delete`) reconstructs `a` exactly; the
/// new-side ops (`Equal`+`Insert`) reconstruct `b` exactly.
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
/// `---`/`+++` headers). Empty when the two are byte-identical, or when
/// `old_text != new_text` only by a leading/trailing empty-line split (no
/// line-granularity change to show).
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
        return String::new();
    }

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

/// Mirrors `EditTool::invoke`'s (`crates/conway-tools/src/fs/edit.rs`) own
/// literal substring-replacement semantics exactly: `old_string` must
/// appear at least once, and more than once requires `replace_all`.
/// Returns `None` in every case the real tool would also refuse
/// (`old_string == new_string`, zero occurrences, or an ambiguous multi-
/// occurrence match without `replace_all`) -- a computed diff is never
/// shown, or folded into a running reconstruction, for a call the real
/// tool would have rejected. `Some((new_content, replacements))` otherwise.
pub fn simulate_edit(
    content: &str,
    old_string: &str,
    new_string: &str,
    replace_all: bool,
) -> Option<(String, usize)> {
    if old_string == new_string {
        return None;
    }
    let count = content.matches(old_string).count();
    if count == 0 {
        return None;
    }
    if count > 1 && !replace_all {
        return None;
    }
    if replace_all {
        Some((content.replace(old_string, new_string), count))
    } else {
        Some((content.replacen(old_string, new_string, 1), 1))
    }
}

/// One successful `edit`/`write` call, narrowed to exactly what
/// [`cumulative_diffs`] needs to fold it into a running per-path
/// reconstruction -- built from a tool call's own JSON `arguments`
/// (`serde_json::Value`), the SAME shape whether it came from a live
/// `Event::ToolCallProposed` (TUI) or a replayed `LogRecord::Assistant`'s
/// `ContentBlock::ToolUse` (headless `sessions show --diff`).
#[derive(Clone, Debug, PartialEq)]
pub struct FileTouch {
    pub path: String,
    pub kind: TouchKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TouchKind {
    Edit {
        old_string: String,
        new_string: String,
        replace_all: bool,
    },
    Write {
        content: String,
    },
}

/// Extracts a [`FileTouch`] from one successful `edit`/`write` call's
/// `tool` name + JSON `arguments` -- `None` for any other tool, or for
/// `arguments` missing a field the named tool requires (never a panic on
/// untrusted/malformed JSON, e.g. a record from a future schema version).
pub fn file_touch_from_args(tool: &str, args: &serde_json::Value) -> Option<FileTouch> {
    let path = args.get("path")?.as_str()?.to_string();
    match tool {
        "write" => Some(FileTouch {
            path,
            kind: TouchKind::Write {
                content: args.get("content")?.as_str()?.to_string(),
            },
        }),
        "edit" => Some(FileTouch {
            path,
            kind: TouchKind::Edit {
                old_string: args.get("old_string")?.as_str()?.to_string(),
                new_string: args.get("new_string")?.as_str()?.to_string(),
                replace_all: args
                    .get("replace_all")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            },
        }),
        _ => None,
    }
}

/// Folds one [`TouchKind`] onto `before`, the running content immediately
/// preceding it -- the one place both [`cumulative_diffs`] (folding a whole
/// ordered sequence) and the settled-transcript-entry per-call diff
/// (`tui/state/transcript.rs::finish_tool`, folding exactly one touch)
/// compute "what this call would produce", so the two can never quietly
/// diverge on the fold rule. A `write` touch replaces `before` outright (it
/// always carries the full new content). An `edit` touch that
/// [`simulate_edit`] refuses (disagrees with what the real, already-
/// successful call must have done -- e.g. `before` drifted from what the
/// live tool actually saw) returns `before` unchanged rather than guessing.
pub fn apply_touch(before: &str, kind: &TouchKind) -> String {
    match kind {
        TouchKind::Write { content } => content.clone(),
        TouchKind::Edit {
            old_string,
            new_string,
            replace_all,
        } => match simulate_edit(before, old_string, new_string, *replace_all) {
            Some((new_content, _)) => new_content,
            None => before.to_string(),
        },
    }
}

/// The cumulative diff `/diff` and `sessions show --diff` both show: every
/// path touched by `touches` (in order), diffed against the bytes it had
/// the FIRST time this ordered sequence touches it.
///
/// **The one disk read this function performs, and its honest limit.** The
/// first time a given path appears in `touches`, its CURRENT on-disk
/// content (read once, right here) is taken as the baseline -- correct for
/// a live session's `/diff` (nothing has touched the file yet at that
/// point) and for `sessions show --diff` run shortly after a session ends
/// against files nothing else has since modified. It is NOT correct for an
/// old session inspected long after its files diverged further, or for a
/// path some other process already reverted -- conway persists no full
/// file snapshots to reconstruct a truer baseline from, so this is the best
/// available answer from the call log alone (see `docs/sessions.md`).
/// Every subsequent touch to the same path is folded in-memory via
/// [`simulate_edit`]/direct replacement, with NO further disk access --
/// this is what keeps the result deterministic and repeatable within one
/// invocation.
///
/// A `write` touch replaces the running content outright (it always
/// carries the full new content). An `edit` touch that [`simulate_edit`]
/// refuses (disagrees with what the real, already-successful call must
/// have done -- e.g. the tracked content drifted from what the live tool
/// actually saw) leaves that path's running content unchanged rather than
/// guessing.
///
/// Returns `(path, unified_diff)` pairs for paths with a non-empty diff, in
/// the order each path was first touched.
pub fn cumulative_diffs(touches: &[FileTouch]) -> Vec<(String, String)> {
    let mut baseline: BTreeMap<String, String> = BTreeMap::new();
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();

    for touch in touches {
        if !current.contains_key(&touch.path) {
            let disk = std::fs::read_to_string(&touch.path).unwrap_or_default();
            baseline.insert(touch.path.clone(), disk.clone());
            current.insert(touch.path.clone(), disk);
            order.push(touch.path.clone());
        }
        let before = current.get(&touch.path).cloned().unwrap_or_default();
        let after = apply_touch(&before, &touch.kind);
        current.insert(touch.path.clone(), after);
    }

    order
        .into_iter()
        .filter_map(|path| {
            let base = baseline.get(&path).cloned().unwrap_or_default();
            let cur = current.get(&path).cloned().unwrap_or_default();
            let d = unified_diff(&path, &path, &base, &cur);
            if d.is_empty() {
                None
            } else {
                Some((path, d))
            }
        })
        .collect()
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
    fn a_two_line_changed_run_shows_both_minus_lines_and_both_plus_lines() {
        let out = unified_diff("f", "f", "a\nb\nc\nd", "a\nB\nC\nd");
        assert!(out.contains("-b"), "{out}");
        assert!(out.contains("-c"), "{out}");
        assert!(out.contains("+B"), "{out}");
        assert!(out.contains("+C"), "{out}");
    }

    #[test]
    fn a_diff_beyond_the_cell_bound_falls_back_to_a_coarse_honest_summary() {
        let old_text: String = (0..3000).map(|n| format!("old-{n}\n")).collect();
        let new_text: String = (0..3000).map(|n| format!("new-{n}\n")).collect();
        let out = unified_diff("f", "f", &old_text, &new_text);
        assert!(out.contains("diff omitted"), "{out}");
    }

    #[test]
    fn simulate_edit_matches_the_real_tools_single_replacement_semantics() {
        let (out, n) = simulate_edit("one two three", "two", "TWO", false).expect("one match");
        assert_eq!(out, "one TWO three");
        assert_eq!(n, 1);
    }

    #[test]
    fn simulate_edit_refuses_an_ambiguous_match_without_replace_all() {
        assert_eq!(simulate_edit("a a a", "a", "b", false), None);
    }

    #[test]
    fn simulate_edit_replace_all_replaces_every_occurrence() {
        let (out, n) = simulate_edit("a a a", "a", "b", true).expect("replace_all");
        assert_eq!(out, "b b b");
        assert_eq!(n, 3);
    }

    #[test]
    fn simulate_edit_refuses_identical_old_and_new() {
        assert_eq!(simulate_edit("x", "x", "x", false), None);
    }

    #[test]
    fn file_touch_from_args_extracts_edit_fields() {
        let touch = file_touch_from_args(
            "edit",
            &serde_json::json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
        )
        .expect("edit touch");
        assert_eq!(touch.path, "f.txt");
        assert_eq!(
            touch.kind,
            TouchKind::Edit {
                old_string: "a".to_string(),
                new_string: "b".to_string(),
                replace_all: false,
            }
        );
    }

    #[test]
    fn file_touch_from_args_extracts_write_fields() {
        let touch = file_touch_from_args(
            "write",
            &serde_json::json!({"path": "f.txt", "content": "hello"}),
        )
        .expect("write touch");
        assert_eq!(touch.path, "f.txt");
        assert_eq!(
            touch.kind,
            TouchKind::Write {
                content: "hello".to_string(),
            }
        );
    }

    #[test]
    fn file_touch_from_args_ignores_other_tools() {
        assert_eq!(
            file_touch_from_args("bash", &serde_json::json!({"command": "ls"})),
            None
        );
    }

    #[test]
    fn cumulative_diffs_folds_two_edits_to_one_path_into_one_diff_against_first_touch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f.txt");
        std::fs::write(&path, "one\ntwo\nthree\n").expect("write");
        let path_str = path.to_string_lossy().to_string();

        let touches = vec![
            FileTouch {
                path: path_str.clone(),
                kind: TouchKind::Edit {
                    old_string: "one".to_string(),
                    new_string: "ONE".to_string(),
                    replace_all: false,
                },
            },
            FileTouch {
                path: path_str.clone(),
                kind: TouchKind::Edit {
                    old_string: "three".to_string(),
                    new_string: "THREE".to_string(),
                    replace_all: false,
                },
            },
        ];

        let diffs = cumulative_diffs(&touches);
        assert_eq!(diffs.len(), 1, "one path touched: {diffs:?}");
        let (got_path, diff_text) = &diffs[0];
        assert_eq!(got_path, &path_str);
        assert!(diff_text.contains("-one"), "{diff_text}");
        assert!(diff_text.contains("+ONE"), "{diff_text}");
        assert!(diff_text.contains("-three"), "{diff_text}");
        assert!(diff_text.contains("+THREE"), "{diff_text}");
    }

    #[test]
    fn cumulative_diffs_covers_multiple_paths_independently() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path_a = dir.path().join("a.txt");
        let path_b = dir.path().join("b.txt");
        std::fs::write(&path_a, "alpha\n").expect("write a");
        std::fs::write(&path_b, "beta\n").expect("write b");
        let a_str = path_a.to_string_lossy().to_string();
        let b_str = path_b.to_string_lossy().to_string();

        let touches = vec![
            FileTouch {
                path: a_str.clone(),
                kind: TouchKind::Edit {
                    old_string: "alpha".to_string(),
                    new_string: "ALPHA".to_string(),
                    replace_all: false,
                },
            },
            FileTouch {
                path: b_str.clone(),
                kind: TouchKind::Write {
                    content: "BETA\n".to_string(),
                },
            },
        ];

        let diffs = cumulative_diffs(&touches);
        assert_eq!(diffs.len(), 2, "{diffs:?}");
        let paths: Vec<&String> = diffs.iter().map(|(p, _)| p).collect();
        assert!(paths.contains(&&a_str));
        assert!(paths.contains(&&b_str));
    }

    #[test]
    fn cumulative_diffs_is_empty_when_no_touches() {
        assert_eq!(cumulative_diffs(&[]), Vec::new());
    }
}

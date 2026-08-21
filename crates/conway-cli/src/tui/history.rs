//! T8: persistence for the input line's history FIFO
//! ([`AppState::history`](crate::tui::state::AppState::history)) -- loaded
//! once at `App::new`, appended to on every submit. Lives alongside the
//! global config (`~/.conway/history`, or `$CONWAY_CONFIG_DIR/history`
//! when set -- `conway::config::discovery::history_file_path`), NOT under
//! the project's own `.conway/` directory: history follows the user, not
//! the checkout.
//!
//! Config and the history file are both untrusted input: [`load`]
//! degrades to an empty history on a missing, unreadable, or corrupt file
//! -- never a panic, never a startup failure -- and skips individual
//! malformed lines rather than discarding the whole file over one bad
//! entry. [`save`] follows the same tmp-write-then-rename shape
//! `conway-session`'s `SessionIndex::persist_full` uses (write a `.tmp`
//! sibling, `rename` it over the real path) so a crash mid-write can never
//! leave a half-written, corrupt history file in place -- the file on disk
//! is always either the complete old version or the complete new one.
//! `App::submit` (the only caller) treats a failed [`save`] as best-effort:
//! a lost history write must never fail the submit it was recording.
//!
//! One JSON-string-encoded entry per line, not a bare newline-delimited
//! line per entry -- a submitted line can itself contain embedded `\n`
//! (T8's multi-line input, Alt/Shift-Enter), which a bare-newline format
//! could not round-trip. `serde_json` is already a dependency (no new
//! dependency needed for the escaping).

use std::collections::VecDeque;
use std::path::Path;

/// Reads the history file at `path` into a `VecDeque` in on-disk (oldest
/// first) order. Never errors: a missing/unreadable file yields an empty
/// history, and each line is decoded independently -- a corrupt line is
/// skipped, not fatal to the rest of the file -- the file is untrusted input.
pub fn load(path: &Path) -> VecDeque<String> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return VecDeque::new();
    };
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            serde_json::from_str::<String>(line).ok()
        })
        .collect()
}

/// Writes `history` to `path`, one JSON-string-encoded entry per line, via a
/// `.tmp` sibling + atomic `rename` (mirrors
/// `conway-session::index::SessionIndex::persist_full`'s write-then-rename
/// shape). Creates the parent directory if it does not exist yet. Returns an
/// `io::Result` so the caller can decide how to treat a failure -- the file is
/// untrusted input: this function itself never panics, and the caller
/// (`App::submit`) never lets a failure here fail the submit it was recording.
pub fn save(path: &Path, history: &VecDeque<String>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut buf = String::new();
    for entry in history {
        // `String`/`&str` serialization to JSON cannot fail; the
        // `unwrap_or_default` is belt-and-braces against untrusted input,
        // not a real fallback path.
        buf.push_str(&serde_json::to_string(entry).unwrap_or_default());
        buf.push('\n');
    }
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, buf)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "conway-history-test-{}-{}-{name}",
            std::process::id(),
            unique_suffix()
        ))
    }

    fn unique_suffix() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        COUNTER.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn round_trips_entries_including_embedded_newlines() {
        let path = temp_path("round-trip");
        let mut history = VecDeque::new();
        history.push_back("hello".to_string());
        history.push_back("multi\nline\nprompt".to_string());
        history.push_back("unicode: héllo 🎉".to_string());

        save(&path, &history).expect("save must succeed against a writable temp path");
        let loaded = load(&path);

        assert_eq!(loaded, history);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_of_a_missing_file_is_an_empty_history_not_an_error() {
        let path = temp_path("missing");
        assert!(!path.exists());
        assert_eq!(load(&path), VecDeque::new());
    }

    #[test]
    fn load_of_a_corrupt_file_skips_bad_lines_and_keeps_good_ones() {
        let path = temp_path("corrupt");
        std::fs::write(&path, "not json\n\"good one\"\n{broken\n\"good two\"\n")
            .expect("write must succeed");

        let loaded = load(&path);

        assert_eq!(
            loaded,
            VecDeque::from(vec!["good one".to_string(), "good two".to_string()])
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn save_creates_the_parent_directory_if_absent() {
        let dir = temp_path("nested-dir");
        let path = dir.join("history");
        assert!(!dir.exists());

        let mut history = VecDeque::new();
        history.push_back("x".to_string());
        save(&path, &history).expect("save must create the missing parent dir");

        assert_eq!(load(&path), history);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_never_leaves_a_tmp_file_behind_on_success() {
        let path = temp_path("no-tmp-residue");
        let mut history = VecDeque::new();
        history.push_back("one".to_string());
        save(&path, &history).expect("save must succeed");

        assert!(!path.with_extension("tmp").exists());
        let _ = std::fs::remove_file(&path);
    }
}

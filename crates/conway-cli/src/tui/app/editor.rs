//! `Action::OpenExternalEditor` (board item `01M1YVJ4RA5V7FF95MFRQMTQW3`,
//! default `Ctrl-G`): suspends the TUI, edits the current input in
//! `$VISUAL`/`$EDITOR`/`vi`, and resumes.
//!
//! `input::handle_key` stays pure with respect to I/O (its own module doc)
//! -- this is the ONE piece of `tui/app/**` the feature genuinely needs: a
//! live terminal to suspend/resume around a child process, which only the
//! app loop (`run.rs`'s `Action::OpenExternalEditor` arm, the sole
//! production caller) has access to.
//!
//! **Reuses `tui/mod.rs`'s own enter/leave pairing, not new terminal
//! control.** `tui::run` enters raw mode + the alternate screen +
//! bracketed paste on startup (`EnableBracketedPaste`,
//! `EnterAlternateScreen`, `enable_raw_mode`) and its own `restore_terminal`
//! undoes exactly those three, in reverse order, best-effort, on every exit
//! path (including a panic, via the installed panic hook). Suspending
//! around `$EDITOR` needs the identical shape, just temporarily: this
//! module leaves in `restore_terminal`'s own order (disable bracketed
//! paste, disable raw mode, leave the alternate screen) and resumes in
//! `run`'s own order (enable raw mode, enter the alternate screen, enable
//! bracketed paste) -- the same three primitives, the same pairing, the
//! same "each step is independently best-effort" posture, just callable
//! from here too. `tui/mod.rs`'s own enter/leave functions are private to
//! that module, so the sequence is mirrored here rather than called; if
//! those are ever made visible to this module, prefer calling them over
//! keeping a second copy of the primitive order.
//!
//! **`editor_command` is a caller-resolved parameter, not read from
//! `std::env` internally.** [`resolve_editor_command`] does the
//! `$VISUAL`/`$EDITOR`/`vi` resolution; `run.rs`'s `Action::
//! OpenExternalEditor` arm is the one production call site for both. This
//! crate's own tests never mutate `std::env::set_var`/`remove_var` in
//! process (a documented pattern elsewhere in this crate -- see
//! `app/plugin_toggle.rs`'s own module doc, "the identical reason":
//! `std::env`'s process-global mutation races against cargo's own
//! parallel-by-default test threads), so [`edit_prompt_externally`] taking
//! the resolved command as a plain argument is what keeps its own tests
//! (below) free of that hazard entirely, exercising a real child process
//! without ever touching the real environment.
//!
//! **Every failure path leaves the input UNCHANGED and shows a notice --
//! never a silent clear:**
//! - The temp file can't be created: [`EditorOutcome::Failed`], input
//!   untouched.
//! - The editor program can't even be spawned (not found, not executable):
//!   [`EditorOutcome::Failed`], input untouched.
//! - The editor exits non-zero: [`EditorOutcome::Failed`], input untouched
//!   -- the file may hold a half-edit the operator abandoned (`:cq` in
//!   vim, a non-zero exit from a wrapper script), so it is never trusted.
//! - The temp file is gone when we go to read it back (deleted out from
//!   under us -- `$TMPDIR` cleaner, the editor's own "save as" moving it):
//!   [`EditorOutcome::Failed`], input untouched.
//! - The result, after stripping the ONE trailing newline an editor
//!   conventionally appends, is empty (the operator deleted everything and
//!   saved): [`EditorOutcome::Unchanged`] -- NOT [`EditorOutcome::Failed`]
//!   (nothing went wrong; an empty save is a legitimate way to say "never
//!   mind"), and NOT a clear either -- the input stays exactly what it was
//!   before `Ctrl-G`.
//! - Anything else (editor exits 0, file readable, non-empty result):
//!   [`EditorOutcome::Replace`] -- `AppState::input` becomes the file's
//!   content verbatim (minus that one trailing newline).

use std::process::Command;

use ratatui::backend::Backend;
use ratatui::crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

/// What editing the input line externally produced -- see this module's
/// own doc for exactly which failure lands on [`EditorOutcome::Failed`]
/// versus [`EditorOutcome::Unchanged`].
pub(crate) enum EditorOutcome {
    /// The editor exited 0, the temp file was still there, and its content
    /// (minus one trailing newline) was non-empty -- `AppState::input`
    /// becomes this verbatim.
    Replace(String),
    /// Nothing to apply -- the editor exited 0 but the saved result was
    /// empty. `AppState::input` is left exactly as it was.
    Unchanged,
    /// The editor could not be run to a trustworthy completion -- `notice`
    /// is shown to the operator (a transcript `Entry::Notice`) and
    /// `AppState::input` is left exactly as it was.
    Failed { notice: String },
}

/// `$VISUAL`, falling back to `$EDITOR`, falling back to `vi` -- the one
/// place this resolution happens, so [`edit_prompt_externally`] itself
/// never reads `std::env` (see this module's own doc for why that split
/// matters for testability).
pub(crate) fn resolve_editor_command() -> String {
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string())
}

/// Writes `current` to a fresh temp file, suspends the terminal (see this
/// module's own doc for the exact enter/leave pairing reused), runs
/// `editor_command` against that file, resumes the terminal, and returns
/// what happened. `terminal.clear()` is called on resume so the NEXT
/// `view::draw` repaints the whole screen -- the editor drew its own
/// content over the same alternate-screen buffer while it ran, and
/// ratatui's own diffing cache has no way to know that happened; without
/// this the screen can stay showing the editor's exit state (or garbage)
/// until something else forces a full redraw.
///
/// `editor_command` (typically [`resolve_editor_command`]'s own result) is
/// split on whitespace so a value carrying its own flags (`"code --wait"`,
/// `"emacs -nw"`) runs as written, with the temp file path appended as the
/// final argument -- the common shape every `$EDITOR`-invoking tool (git,
/// most shells' `fc`, ...) already assumes.
pub(crate) fn edit_prompt_externally<B: Backend>(
    terminal: &mut Terminal<B>,
    current: &str,
    editor_command: &str,
) -> EditorOutcome {
    let path = std::env::temp_dir().join(format!(
        "conway-prompt-{}-{}.txt",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ));
    if let Err(e) = std::fs::write(&path, current) {
        return EditorOutcome::Failed {
            notice: format!("could not create a temp file for the editor: {e}"),
        };
    }

    let mut parts = editor_command.split_whitespace();
    let program = parts.next().unwrap_or("vi");
    let mut command = Command::new(program);
    command.args(parts);
    command.arg(&path);

    // Suspend: same three primitives, same order, `tui::mod::
    // restore_terminal` already uses on every OTHER exit path -- see this
    // module's own doc.
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    let _ = disable_raw_mode();
    let _ = execute!(std::io::stdout(), LeaveAlternateScreen);

    let status = command.status();

    // Resume: same three primitives, same order, `tui::run` already uses
    // to enter in the first place.
    let _ = enable_raw_mode();
    let _ = execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste
    );
    let _ = terminal.clear();

    let status = match status {
        Ok(status) => status,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return EditorOutcome::Failed {
                notice: format!("could not run editor {editor_command:?}: {e}"),
            };
        }
    };

    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return EditorOutcome::Failed {
            notice: format!(
                "editor {editor_command:?} exited with {status} -- the prompt was left unchanged"
            ),
        };
    }

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(e) => {
            return EditorOutcome::Failed {
                notice: format!(
                    "the editor exited cleanly, but its temp file is gone ({e}) -- the prompt \
                     was left unchanged"
                ),
            };
        }
    };
    let _ = std::fs::remove_file(&path);

    // Strip exactly the ONE trailing newline an editor conventionally
    // appends (and its `\r` if the file is CRLF) -- never every trailing
    // blank line, which the operator may have typed deliberately.
    let trimmed = contents
        .strip_suffix('\n')
        .map(|s| s.strip_suffix('\r').unwrap_or(s))
        .unwrap_or(&contents);

    if trimmed.is_empty() {
        EditorOutcome::Unchanged
    } else {
        EditorOutcome::Replace(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn test_terminal() -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(20, 5)).expect("TestBackend construction cannot fail")
    }

    /// A test-only "editor": a tiny shell script this test writes and
    /// marks executable, so `edit_prompt_externally` spawns a REAL child
    /// process (proving the spawn/wait/read round-trip works end to end)
    /// without depending on any editor actually being installed on the
    /// machine running the test, and without ever touching
    /// `std::env::set_var` (this module's own doc, "a caller-resolved
    /// parameter").
    fn write_test_script(name: &str, body: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "conway-editor-test-{}-{}-{name}.sh",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, body).expect("write must succeed against a writable temp path");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path)
                .expect("metadata must succeed")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod must succeed");
        }
        path
    }

    /// Acceptance check (c): with the editor command set to a script that
    /// APPENDS text to the temp file, the input changes to include that
    /// appended text -- proves the whole round trip (temp file written,
    /// editor spawned against it, editor's own edit read back, applied to
    /// the input) actually works, not just that SOME action fires.
    #[test]
    #[cfg(unix)]
    fn editor_that_appends_text_changes_the_input() {
        let script =
            write_test_script("appends", "#!/bin/sh\necho ' appended' >> \"$1\"\nexit 0\n");
        let mut terminal = test_terminal();

        let outcome =
            edit_prompt_externally(&mut terminal, "hello", script.to_str().expect("utf8 path"));

        let _ = std::fs::remove_file(&script);

        match outcome {
            EditorOutcome::Replace(text) => {
                assert_eq!(
                    text, "hello appended",
                    "the editor's own edit must round-trip"
                );
            }
            EditorOutcome::Unchanged => panic!("expected Replace, got Unchanged"),
            EditorOutcome::Failed { notice } => panic!("expected Replace, got Failed: {notice}"),
        }
    }

    /// A non-zero editor exit leaves the input UNCHANGED (`Failed`, never
    /// `Replace`) even though the script also mutated the temp file --
    /// catches an implementation that trusts the file's content over the
    /// editor's own exit status.
    #[test]
    #[cfg(unix)]
    fn editor_exiting_non_zero_leaves_input_unchanged() {
        let script = write_test_script(
            "fails",
            "#!/bin/sh\necho 'should never be applied' >> \"$1\"\nexit 1\n",
        );
        let mut terminal = test_terminal();

        let outcome =
            edit_prompt_externally(&mut terminal, "hello", script.to_str().expect("utf8 path"));

        let _ = std::fs::remove_file(&script);

        match outcome {
            EditorOutcome::Failed { .. } => {}
            EditorOutcome::Replace(text) => {
                panic!("a non-zero exit must not replace the input, got {text:?}")
            }
            EditorOutcome::Unchanged => panic!("expected Failed, got Unchanged"),
        }
    }

    /// A script that deletes the temp file before exiting 0 -- the "missing
    /// temp file" failure path -- must also leave the input unchanged, not
    /// panic and not silently clear it.
    #[test]
    #[cfg(unix)]
    fn missing_temp_file_after_a_clean_exit_leaves_input_unchanged() {
        let script = write_test_script("deletes", "#!/bin/sh\nrm -f \"$1\"\nexit 0\n");
        let mut terminal = test_terminal();

        let outcome =
            edit_prompt_externally(&mut terminal, "hello", script.to_str().expect("utf8 path"));

        let _ = std::fs::remove_file(&script);

        match outcome {
            EditorOutcome::Failed { .. } => {}
            EditorOutcome::Replace(text) => {
                panic!("a missing temp file must not replace the input, got {text:?}")
            }
            EditorOutcome::Unchanged => {
                panic!("a missing temp file is a Failed notice, not a silent Unchanged")
            }
        }
    }

    /// An editor that empties the file (and exits 0) is a deliberate
    /// "never mind," not a failure and not a clear -- `Unchanged`.
    #[test]
    #[cfg(unix)]
    fn editor_that_empties_the_file_reports_unchanged_not_failed() {
        let script = write_test_script("empties", "#!/bin/sh\n> \"$1\"\nexit 0\n");
        let mut terminal = test_terminal();

        let outcome =
            edit_prompt_externally(&mut terminal, "hello", script.to_str().expect("utf8 path"));

        let _ = std::fs::remove_file(&script);

        match outcome {
            EditorOutcome::Unchanged => {}
            EditorOutcome::Replace(text) => panic!("expected Unchanged, got Replace({text:?})"),
            EditorOutcome::Failed { notice } => panic!("expected Unchanged, got Failed: {notice}"),
        }
    }

    /// An empty input, edited by a script that does not touch the file at
    /// all, must also report `Unchanged` -- there was nothing to begin
    /// with and nothing after either.
    #[test]
    #[cfg(unix)]
    fn empty_input_left_untouched_by_the_editor_reports_unchanged() {
        let script = write_test_script("noop", "#!/bin/sh\nexit 0\n");
        let mut terminal = test_terminal();

        let outcome =
            edit_prompt_externally(&mut terminal, "", script.to_str().expect("utf8 path"));

        let _ = std::fs::remove_file(&script);

        assert!(matches!(outcome, EditorOutcome::Unchanged));
    }

    /// An editor program that does not exist at all (never spawns) is the
    /// "editor not found" failure path.
    #[test]
    fn nonexistent_editor_program_fails_cleanly() {
        let mut terminal = test_terminal();

        let outcome = edit_prompt_externally(
            &mut terminal,
            "hello",
            "/no/such/conway-test-editor-binary-does-not-exist",
        );

        match outcome {
            EditorOutcome::Failed { .. } => {}
            EditorOutcome::Replace(text) => {
                panic!("a nonexistent editor must not replace the input, got {text:?}")
            }
            EditorOutcome::Unchanged => panic!("expected Failed, got Unchanged"),
        }
    }

    /// A trailing newline the editor conventionally appends is stripped --
    /// exactly one, not every trailing blank line.
    #[test]
    #[cfg(unix)]
    fn exactly_one_trailing_newline_is_stripped() {
        let script = write_test_script(
            "writes-two-trailing-newlines",
            "#!/bin/sh\nprintf 'line one\\n\\n' > \"$1\"\nexit 0\n",
        );
        let mut terminal = test_terminal();

        let outcome =
            edit_prompt_externally(&mut terminal, "x", script.to_str().expect("utf8 path"));

        let _ = std::fs::remove_file(&script);

        match outcome {
            EditorOutcome::Replace(text) => assert_eq!(
                text, "line one\n",
                "only the FINAL trailing newline is stripped, not every one"
            ),
            EditorOutcome::Unchanged => panic!("expected Replace, got Unchanged"),
            EditorOutcome::Failed { notice } => panic!("expected Replace, got Failed: {notice}"),
        }
    }
}

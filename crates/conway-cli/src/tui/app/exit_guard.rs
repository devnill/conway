//! Board item `01M1YVH1X49WYSQ9C2Z4D6B4XM` ("B3d"): intercepts a bare
//! submitted word that experienced users of OTHER chat/editor tools
//! habitually type to leave -- `exit`, `quit`, `q`, `:q`, `:wq` -- so it is
//! not silently sent to the model as an ordinary prompt and answered with a
//! cheerful, useless reply. The operator's own real sessions are the trigger:
//! they typed `exit` as a prompt twice, got a polite non-answer both times,
//! and never found `/quit`.
//!
//! **Exact words only, no natural-language intent detection.** This is a
//! fixed, tiny list of words a DIFFERENT tool's muscle memory produces, not
//! a guess at what the operator "really meant" -- "please exit" or "I want
//! to quit the project" are ordinary prompts and reach the model unchanged,
//! exactly as every other ordinary prompt does.
//!
//! **A second identical submission sends it.** The hint this module asks
//! `App::submit` to show never refuses the word outright -- only once. If
//! the operator meant it as a literal prompt (or just wants to see what
//! happens), pressing Enter again on the exact same line sends it through
//! normally, no special syntax needed. `AppState::pending_exit_word` is the
//! one bit of state this needs (this module itself is pure and holds none):
//! `check` takes the PREVIOUS value as a plain `Option<&str>` rather than
//! reading `AppState` itself, so it is trivially unit-testable with no
//! `AppState` fixture at all, and `App::submit`'s own read/write of that
//! field stays a one-line call at each end (see this module's doc on why
//! that matters for the upcoming B3e merge).
//!
//! **Why a free function, not a method on `App`.** Unlike this module's
//! sibling `viewport.rs` (terminal-size math `App` alone has the inputs
//! for), this check needs nothing from `App` or a live terminal -- it is a
//! pure function of two strings. `App::submit` is the one production
//! caller, calling it with `self.state.pending_exit_word.as_deref()` and
//! writing its own field back from the result, so this module carries no
//! `AppState` dependency at all and the diff `App::submit` needs is a
//! single early call plus a two-armed match -- deliberately small so the
//! in-flight B3e busy-input work (`app/shutdown.rs`, `input.rs`,
//! `keybindings.rs` on the other lane) merges past it with at most a
//! one-line conflict.

/// The exact (case-insensitive, trimmed) words intercepted -- nothing else.
const INTERCEPTED_WORDS: &[&str] = &["exit", "quit", "q", ":q", ":wq"];

/// The one-line hint `App::submit` shows in the transcript in place of
/// sending the word.
pub const HINT: &str =
    "to leave, use `/quit` (or Ctrl-D on an empty line) -- press Enter again to send the word to \
     the model";

/// What `App::submit` should do with a just-submitted line, given
/// `previous` -- the trimmed, lowercased word from the immediately
/// preceding submission IF that one was itself intercepted (`None`
/// otherwise, including "this is the first submission of the session").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Not one of the intercepted words (or a repeat of a DIFFERENT one):
    /// proceed exactly as every ordinary submission always has.
    Pass,
    /// The first time this exact word was seen in a row: show [`HINT`]
    /// instead of sending it, and remember the word for next time.
    Hint,
    /// The SAME word, submitted again immediately after its own [`Decision::Hint`]:
    /// send it through as an ordinary prompt this time.
    Send,
}

/// Normalizes (trim, lowercase) `text` and returns it only if it is exactly
/// one of [`INTERCEPTED_WORDS`] -- `None` for every other line, including
/// one that merely CONTAINS an intercepted word (`"please exit"` is an
/// ordinary prompt, never intercepted -- module doc's "exact words only").
fn intercepted_word(text: &str) -> Option<String> {
    let trimmed = text.trim();
    INTERCEPTED_WORDS
        .iter()
        .find(|w| w.eq_ignore_ascii_case(trimmed))
        .map(|_| trimmed.to_lowercase())
}

/// Decides what `App::submit` should do with `text`, given `previous`
/// (`AppState::pending_exit_word`, read BEFORE this call updates it --
/// `App::submit` is the one place that both calls this and owns writing
/// the field back, since a pure function has nowhere else to put the
/// "remember this for next time" side effect).
pub fn check(text: &str, previous: Option<&str>) -> Decision {
    match intercepted_word(text) {
        None => Decision::Pass,
        Some(word) => {
            if previous == Some(word.as_str()) {
                Decision::Send
            } else {
                Decision::Hint
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_prompt_passes_through_untouched() {
        assert_eq!(check("hello there", None), Decision::Pass);
        assert_eq!(check("", None), Decision::Pass);
    }

    #[test]
    fn a_prompt_merely_containing_an_intercepted_word_is_not_intercepted() {
        // Module doc's "exact words only" -- substring/phrase matches are
        // ordinary prompts, not the bare word itself.
        assert_eq!(check("please exit", None), Decision::Pass);
        assert_eq!(check("I want to quit my job", None), Decision::Pass);
        assert_eq!(check("exit code 1", None), Decision::Pass);
    }

    #[test]
    fn each_intercepted_word_hints_the_first_time() {
        for word in INTERCEPTED_WORDS {
            assert_eq!(check(word, None), Decision::Hint, "word {word:?}");
        }
    }

    #[test]
    fn matching_is_case_insensitive_and_trims_whitespace() {
        assert_eq!(check("EXIT", None), Decision::Hint);
        assert_eq!(check("  exit  ", None), Decision::Hint);
        assert_eq!(check("Quit", None), Decision::Hint);
    }

    #[test]
    fn an_identical_second_submission_sends_it() {
        let first = check("exit", None);
        assert_eq!(first, Decision::Hint);
        // `App::submit` would have stored "exit" as `pending_exit_word` on
        // seeing `Decision::Hint` -- simulated here by passing it straight
        // through as `previous`.
        assert_eq!(check("exit", Some("exit")), Decision::Send);
    }

    #[test]
    fn the_second_submission_must_match_case_insensitively_too() {
        assert_eq!(check("EXIT", Some("exit")), Decision::Send);
    }

    #[test]
    fn a_different_word_in_between_resets_the_hint() {
        // Operator typed "exit" (hint shown, pending = "exit"), then typed
        // an unrelated prompt (`App::submit` clears `pending_exit_word` on
        // any `Decision::Pass` or `Decision::Send`), then typed "exit"
        // again -- this must hint again, not send, since the two "exit"s
        // were not consecutive.
        assert_eq!(check("exit", None), Decision::Hint);
        assert_eq!(check("what is 2+2", None), Decision::Pass);
        assert_eq!(check("exit", None), Decision::Hint);
    }

    #[test]
    fn a_different_intercepted_word_right_after_hints_again_rather_than_sending() {
        // "exit" hinted, then "quit" submitted -- not a repeat of the SAME
        // word, so it hints too rather than sending.
        assert_eq!(check("exit", None), Decision::Hint);
        assert_eq!(check("quit", Some("exit")), Decision::Hint);
    }

    #[test]
    fn no_other_prompt_text_is_ever_intercepted() {
        // A representative sample of ordinary prompts, including ones that
        // share a prefix/suffix with an intercepted word.
        for prompt in [
            "write me a function",
            "q and a session",
            "wq is not a word",
            "quitting time soon?",
        ] {
            assert_eq!(check(prompt, None), Decision::Pass, "prompt {prompt:?}");
        }
    }
}

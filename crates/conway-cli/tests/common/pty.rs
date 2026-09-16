//! A small PTY harness for driving the compiled `conway` binary the way an
//! interactive operator actually sees it: stdin/stdout attached to a REAL
//! pseudo-terminal, with an explicit window size, rather than the piped
//! `Stdio` [`super::command`] gives every other suite in this directory.
//! That distinction is load-bearing, not cosmetic -- `main.rs`'s own
//! `IsTerminal` check is exactly why the interactive guided-setup flow,
//! `/model`, `/role`, and permission-mode cycling were untestable against
//! the real binary before this: a piped stdin/stdout is never a terminal,
//! so every compiled-binary test in this directory always took the
//! non-interactive branch, by construction.
//!
//! # Why `portable-pty`, not `expectrl`
//!
//! Both crates are Unix-adequate for this repo's actual footprint (CI is
//! `ubuntu-latest`, the operator is on macOS -- `.github/workflows/ci.yml`
//! carries no `windows-latest` job, by deliberate, documented choice), so
//! neither one's Windows story is a real differentiator here. What decided
//! it: `portable-pty` is the wezterm project's own pty-allocation crate,
//! versioned and released alongside wezterm itself and still actively
//! maintained; `expectrl`'s own release cadence is comparatively stale.
//! `portable-pty` also gives an EXPLICIT `PtySize { rows, cols, .. }` at
//! allocation time -- exactly the primitive this module's own history (see
//! below) needs, rather than an ambient default a caller has to remember to
//! override. Neither crate ships a "wait for text" primitive suited to a
//! full-screen `ratatui` app (both are built around a line-oriented
//! `expect`-a-shell-prompt idiom); [`PtySession::wait_for_any`] is this
//! module's own, deliberately simple answer -- see its own doc for exactly
//! what it does and does not model, and why a real VT100 terminal emulator
//! (a plausible, more precise alternative) was rejected as a SECOND new
//! dependency this item's one-dependency budget does not allow.
//!
//! # Two failures this harness exists to not repeat
//!
//! Both cost a retry when this was done by hand with `tmux` before this
//! item existed:
//!
//! - **A pane dies the instant its command exits.** Reading scrollback
//!   AFTER a process exits can already be too late. [`PtySession::spawn`]
//!   never reads on demand: a background thread drains the pty's master
//!   side continuously, into [`PtySession`]'s own buffer, for as long as
//!   anything arrives -- by the time a test asks, whatever the child ever
//!   printed is already captured, whether or not the child (or the pty
//!   itself) is still alive.
//! - **An unsized window wraps at 80 columns.** [`PtySession::spawn`]
//!   takes `cols`/`rows` as required, non-defaulted parameters for exactly
//!   this reason: a caller that does not think about the window size is
//!   forced to pick one, rather than silently inheriting whatever the pty
//!   allocator's own default happens to be.
//!
//! # What "capture the rendered screen" means here
//!
//! [`PtySession::screen`] is every printable character this pty has ever
//! emitted, with ANSI/VT control sequences stripped, concatenated in
//! emission order. It is **not** a live, cursor-addressed grid (that would
//! need real terminal emulation -- the VT100-emulator alternative this
//! module's own doc above already explains rejecting). Two consequences,
//! both deliberate given what this suite's own tests actually assert (see
//! `CONTRIBUTING.md`'s TUI-testing section: substring/line claims, never a
//! whole-screen snapshot):
//!
//! - Text stays visible in [`PtySession::screen`] forever once printed,
//!   even after a later redraw would visually replace it on a real
//!   terminal. Fine for "did X ever get printed", and actively useful for
//!   [`PtySession::wait_for_any`]'s own ordering guarantee (`since`, below)
//!   -- it is exactly wrong for "is X on screen RIGHT NOW", which nothing
//!   in this suite needs to ask.
//! - Escape sequences are dropped with no boundary character inserted in
//!   their place, so two on-screen regions separated only by a cursor-move
//!   escape are concatenated directly. Deliberate: `ratatui` interleaves
//!   per-character SGR (colour/style) escapes far more often than it uses
//!   cursor-position escapes, and inserting a boundary at every stripped
//!   escape would fragment ordinary styled words with spurious spaces --
//!   worse, for THIS suite's assertions, than the rarer risk of two
//!   adjacent rows gluing together.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, MasterPty, PtySize};
pub use portable_pty::{CommandBuilder, ExitStatus};

/// How often [`PtySession::wait_for_any`] re-checks the accumulated output
/// while polling. Not a synchronization primitive on its own -- the
/// `timeout` a caller passes is what makes a stuck wait fail loudly rather
/// than hang; this is only how fine-grained that failure's own latency is.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A running `conway` process attached to a real pty, with its combined
/// stdout+stderr output continuously drained into memory by a background
/// thread. See this module's own doc for why continuous draining (never
/// on-demand reads) is the point.
pub struct PtySession {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Arc<Mutex<Vec<u8>>>,
    // Kept alive for the session's whole lifetime purely so its own `Drop`
    // (whatever that closes) never runs early -- `try_clone_reader`/
    // `take_writer` already gave this session its own independent handles,
    // so nothing here is read from or written to directly.
    _master: Box<dyn MasterPty + Send>,
}

impl PtySession {
    /// Spawns `cmd` attached to a fresh pty sized `cols`x`rows` -- ALWAYS
    /// explicit, never the allocator's own default (this module's own doc:
    /// an unsized window silently wraps at 80 columns and breaks any
    /// assertion expecting a longer rendered line intact on one row).
    pub fn spawn(cmd: CommandBuilder, cols: u16, rows: u16) -> PtySession {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("allocate a pty (openpty)");

        let child = pair
            .slave
            .spawn_command(cmd)
            .expect("spawn conway attached to the pty's slave side");
        // Close THIS process's own copy of the slave fd now that the child
        // has its own (inherited across the spawn) -- otherwise a read on
        // `master` below can block forever after the child exits, since a
        // slave fd still open in this process would keep the far end of
        // the pty alive from the kernel's point of view regardless of what
        // the child itself does.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .expect("clone a reader for the pty's master side");
        let writer = pair
            .master
            .take_writer()
            .expect("take a writer for the pty's master side");

        let output = Arc::new(Mutex::new(Vec::new()));
        let output_for_reader = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        output_for_reader
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .extend_from_slice(&buf[..n]);
                    }
                    // A pty master read after the far (slave/child) side
                    // has gone away commonly surfaces as an OS error (EIO
                    // on Linux) rather than a clean `Ok(0)` -- treated
                    // identically here, never a panic: this thread's only
                    // job is "keep draining for as long as there is
                    // anything to drain", and either signal means there
                    // is not.
                    Err(_) => break,
                }
            }
        });

        PtySession {
            child,
            writer,
            output,
            _master: pair.master,
        }
    }

    /// Writes `text` to the pty's input side verbatim -- e.g. `"/model\r"`
    /// for a slash command followed by Enter (raw-mode terminal input
    /// sends CR, not LF, for the Enter key; [`PtySession::send_enter`]
    /// spells that out for a caller that does not want to remember it).
    pub fn send(&mut self, text: &str) {
        self.writer
            .write_all(text.as_bytes())
            .expect("write to the pty");
        self.writer.flush().expect("flush the pty writer");
    }

    /// The Enter key, alone, in raw terminal input: `CR` (`\r`), not `\n`.
    pub fn send_enter(&mut self) {
        self.send("\r");
    }

    /// Shift-Tab, alone: the `CSI Z` encoding (`ESC [ Z`) most terminals
    /// send for this chord, and the one `crossterm` decodes as
    /// `KeyCode::BackTab` -- `tui/input.rs`'s own
    /// `shift_tab_in_normal_mode_cycles_the_permission_mode` test names
    /// this as "crossterm's decode on most terminals".
    pub fn send_shift_tab(&mut self) {
        self.send("\x1b[Z");
    }

    /// Polls for the child's exit, up to `timeout` -- the one piece of
    /// process lifecycle none of [`Self::spawn`]/[`Self::send`]/
    /// [`Self::wait_for_any`]/[`Self::screen`] otherwise expose, needed by
    /// a non-interactive run this suite expects to actually EXIT (rather
    /// than sit at a live TUI): reads the rendered guidance alone would
    /// leave "refuses" unproven without also confirming the process left
    /// non-zero, not just a printed line. Panics, naming `timeout`, if the
    /// child is still running once it elapses -- the same "loud, never a
    /// silent hang" contract [`Self::wait_for_any`] has.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("poll child exit status") {
                return status;
            }
            if start.elapsed() > timeout {
                panic!(
                    "timed out after {timeout:?} waiting for the child to exit; captured screen \
                     so far:\n{}",
                    self.screen()
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Blocks until ONE of `patterns` has appeared in the pty's
    /// ANSI-stripped output so far, searching from (approximately) byte
    /// offset `since` in that stripped text -- so a caller can chain calls
    /// to prove ORDERING (`b = wait_for_since(pattern_b, a, ..)` can only
    /// succeed on text that arrived at or after `a`'s own match), which is
    /// how this suite proves e.g. "verifying" is printed before "landed"
    /// without needing a live screen grid at all (this module's own doc).
    /// `since` is clamped to the nearest earlier UTF-8 character boundary,
    /// never panics on a mid-character offset.
    ///
    /// Panics -- loudly, naming `patterns` and the full captured screen --
    /// rather than ever returning after `timeout` silently unmet (board
    /// note: "no fixed sleep as a synchronisation primitive"; this IS the
    /// primitive a fixed sleep would otherwise stand in for, badly).
    ///
    /// Returns `(index into patterns of the match, byte offset in the
    /// stripped text right after the match)` -- the second half is exactly
    /// what a follow-up `wait_for_since` call wants for `since`.
    pub fn wait_for_any(
        &self,
        patterns: &[&str],
        since: usize,
        timeout: Duration,
    ) -> (usize, usize) {
        assert!(
            !patterns.is_empty(),
            "wait_for_any needs at least one pattern"
        );
        let start = Instant::now();
        loop {
            let text = self.screen();
            let from = floor_char_boundary(&text, since.min(text.len()));
            if let Some((pattern_index, end)) = earliest_match(&text[from..], patterns) {
                return (pattern_index, from + end);
            }
            if start.elapsed() > timeout {
                panic!(
                    "timed out after {timeout:?} waiting for one of {patterns:?} (searching \
                     from offset {since}); captured screen so far:\n{text}"
                );
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// [`Self::wait_for_any`] for exactly one pattern, searched from the
    /// start of the captured output. Returns the byte offset right after
    /// the match (feed straight into a later [`Self::wait_for_since`] to
    /// prove ordering).
    pub fn wait_for(&self, pattern: &str, timeout: Duration) -> usize {
        self.wait_for_any(&[pattern], 0, timeout).1
    }

    /// [`Self::wait_for_any`] for exactly one pattern, searched from
    /// `since` onward.
    pub fn wait_for_since(&self, pattern: &str, since: usize, timeout: Duration) -> usize {
        self.wait_for_any(&[pattern], since, timeout).1
    }

    /// The rendered screen, best-effort -- see this module's own top doc
    /// ("What 'capture the rendered screen' means here") for exactly what
    /// this is and is not.
    pub fn screen(&self) -> String {
        let buf = self.output.lock().unwrap_or_else(|e| e.into_inner());
        strip_ansi(&buf)
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Kill even on panic: a failed assertion elsewhere in a test must
        // never leave an orphaned `conway` process holding the pty (and,
        // for an interactive session, a live mock backend's listener) open
        // after the test itself has stopped running. `Drop` still runs
        // during an unwind under this workspace's default `panic = unwind`
        // test profile, which is what makes this reachable at all.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Strips ANSI/VT escape sequences from `bytes`, keeping every other byte
/// verbatim (including multi-byte UTF-8 sequences, which this never
/// inspects -- decoding to `str` happens exactly once, at the end, via
/// `from_utf8_lossy`, over the already-filtered byte vector).
///
/// Recognizes exactly the shapes `crossterm`'s own ANSI backend emits
/// (cursor movement/visibility, SGR colour/attributes, screen/line
/// clearing, alternate-screen enter/leave, bracketed-paste toggling): a
/// bare `ESC` (`0x1b`) starts either a CSI sequence (`ESC '['`, then any
/// number of parameter/intermediate bytes, ending at the first byte in
/// `0x40..=0x7e`), an OSC sequence (`ESC ']'`, terminated by `BEL`/`0x07`
/// or the two-byte String Terminator `ESC '\'`), or -- the fallback for
/// every other `ESC`-prefixed sequence this crate does not otherwise name
/// (`ESC 7`, `ESC 8`, `ESC c`, ...) -- a single following byte. A `CR`
/// (`\r`) is dropped too (raw terminal output pairs every `\n` with one);
/// a bare `\n` is kept.
fn strip_ansi(bytes: &[u8]) -> String {
    let mut kept: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            i += 1;
            match bytes.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    // Consume the final byte too, if present (a sequence
                    // truncated mid-flight -- a live read racing a still-
                    // arriving escape -- just runs off the end here; the
                    // next poll iteration sees the completed sequence).
                    if i < bytes.len() {
                        i += 1;
                    }
                }
                Some(b']') => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => {
                    i += 1;
                }
                None => {}
            }
            continue;
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        kept.push(b);
        i += 1;
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// The largest index `<= at` that is a valid `char` boundary in `text` --
/// so [`PtySession::wait_for_any`] can slice `text[from..]` without ever
/// panicking on an offset carried over from a PREVIOUS (shorter) capture
/// that happened to land mid-character in the CURRENT one.
fn floor_char_boundary(text: &str, at: usize) -> usize {
    let mut at = at.min(text.len());
    while at > 0 && !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The earliest (by start position) of `patterns` found in `haystack`, as
/// `(index into patterns, byte offset right after the match)`.
fn earliest_match(haystack: &str, patterns: &[&str]) -> Option<(usize, usize)> {
    patterns
        .iter()
        .enumerate()
        .filter_map(|(i, pattern)| {
            haystack
                .find(pattern)
                .map(|start| (i, start, start + pattern.len()))
        })
        .min_by_key(|(_, start, _)| *start)
        .map(|(i, _, end)| (i, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_drops_cursor_and_sgr_sequences_but_keeps_plain_text() {
        // `ESC[2J` (clear screen) + `ESC[1;1H` (cursor home) + a red-styled
        // word (`ESC[31m` ... `ESC[0m`) + a trailing CR: exactly the shape
        // ratatui's crossterm backend emits for one frame.
        let raw = b"\x1b[2J\x1b[1;1Hhello \x1b[31mworld\x1b[0m\r\n";
        assert_eq!(strip_ansi(raw), "hello world\n");
    }

    #[test]
    fn strip_ansi_tolerates_a_sequence_truncated_mid_flight() {
        // The tail end of a read landing exactly inside an escape
        // sequence -- must not panic, and must not leak the partial
        // sequence's bytes into the kept text.
        let raw = b"ok\x1b[31";
        assert_eq!(strip_ansi(raw), "ok");
    }

    #[test]
    fn earliest_match_prefers_the_pattern_that_occurs_first() {
        let haystack = "conway found a local model server";
        assert_eq!(
            earliest_match(haystack, &["local model server", "conway found"]),
            Some((1, "conway found".len()))
        );
    }

    #[test]
    fn floor_char_boundary_never_panics_on_a_multi_byte_offset() {
        let text = "héllo"; // 'é' is 2 bytes, so byte offset 2 lands mid-char
        let at = floor_char_boundary(text, 2);
        assert!(text.is_char_boundary(at));
        assert!(at <= 2);
    }
}

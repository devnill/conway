//! The vim prompt-editing layer (board item `01M1YVJNS575YN5DCQG9BKZR4E`):
//! an opt-in modal editor over the SAME `(text, cursor)` pair
//! `crate::tui::input` already owns for the plain (emacs-shaped) input
//! line. `tui.editor_mode = vim` (`crate::tui::config::EditorMode`) turns it
//! on; `emacs` (the default) never constructs or calls into anything in
//! this module at all, so the existing keymap is byte-for-byte unchanged
//! when it is off.
//!
//! ## A pure state machine, not a second copy of `AppState`
//!
//! [`handle_key`] takes `&mut VimState`, `&mut String`, `&mut usize` --
//! nothing else. It never reads `AppState`, never returns an `Action`, and
//! never knows a transcript/agent-panel/permission-prompt exists. That is
//! what makes it unit-testable with a bare `String`/`usize` and no
//! terminal, app loop, or `SessionHandle` (see this module's own tests,
//! which do exactly that) -- and it is why `crate::tui::input::
//! handle_vim_key` (the one caller) temporarily takes `AppState::input`/
//! `cursor` out, hands them to this module, and writes them back, rather
//! than passing `&mut AppState` through.
//!
//! ## Precedence: where this layer sits, and what it never touches
//!
//! [`wants_key`] is the ONE gate `input::handle_normal_key` consults, and
//! ONLY after `resolve_keymap_action` has already had first refusal. That
//! ordering matters for the operator ruling (`01M3TJCRRQCDDKFGMTWPCTQZ85`):
//! vim mode is a MODAL LAYER, not a rebinding of `crate::tui::keybindings::
//! ACTIONS` -- every Ctrl-bound action (`Ctrl-A`/`Ctrl-E`/`Ctrl-U`/`Ctrl-K`,
//! `Ctrl-P`/`Ctrl-N` history, `Ctrl-O` tool-output, `Ctrl-G` the external
//! editor, `Tab` completion, `F2` send-now) resolves exactly as it does in
//! `emacs` mode, in BOTH vim submodes, because `resolve_keymap_action` sees
//! those keys FIRST and this module never does. [`wants_key`] claims only:
//!
//! - `Esc` (always -- mode transitions and cancelling a partial command).
//! - A plain `Char` key with no `Ctrl`/`Alt` held (ordinary letters/digits
//!   -- `Shift` alone, for an uppercase letter like `G`/`D`/`C`/`X`, is
//!   fine).
//! - `Ctrl-R` (redo) -- the ONE new Ctrl chord this feature adds, since
//!   nothing in [`crate::tui::keybindings::ACTIONS`] binds it and the
//!   existing fixed chain would otherwise silently swallow it as an
//!   "unbound Ctrl chord" (`input.rs`'s own `KeyCode::Char(_) if ctrl !=
//!   alt` arm).
//! - `Enter` and `Backspace`, but ONLY while [`VimState::mode`] is
//!   [`VimMode::Search`] -- the one place this module intercepts either
//!   key, so a search query can be confirmed/edited. Everywhere else,
//!   `Enter` keeps its EXACT existing meaning untouched (submit in both
//!   vim submodes, newline on `Alt`/`Shift-Enter` -- "Enter in NORMAL
//!   submits" is therefore not new code at all, just the pre-existing
//!   arm now also reachable from `Normal`), and `Backspace` keeps editing
//!   the input the ordinary way.
//!
//! `Left`/`Right`/`Up`/`Down`/`Home`/`End`/`Delete`/`PageUp`/`PageDown` are
//! NEVER claimed -- they fall through to the exact same fixed chain emacs
//! mode uses, in both editor modes, unchanged. `h`/`j`/`k`/`l` are this
//! module's own letter-based motions instead (see "Supported subset"
//! below); the arrow keys are deliberately a separate, untouched
//! vocabulary.
//!
//! Esc's existing, non-input meanings elsewhere in the TUI (closing the
//! `/agents` panel, the `/help`/`/settings`/`/plugin` overlays, the
//! permission prompt's deny-with-feedback, every modal's own cancel) are
//! ALL resolved before `input::handle_normal_key`'s own `Esc` arm is ever
//! reached (see that function's own doc) or, for the two cases that DO
//! live inside it (closing the agent panel, returning focus to the root),
//! checked ahead of this module's own `Esc` handling -- `handle_vim_key`
//! is only consulted once the key has already run that whole
//! chain and found nothing to do with it, so vim's own `Esc` behavior
//! (leave INSERT, or cancel a partial NORMAL command) only ever applies in
//! exactly the state plain `emacs` mode already treated `Esc` as a true
//! no-op. One disclosed interaction: `agents_panel.cycle_visibility`
//! (default `v`) still wins over Visual mode's own `v` while the `/agents`
//! panel is open -- the same "same key, different surface, only one can be
//! live" shape `Ctrl-O`/`Ctrl-E` already have elsewhere in this crate (see
//! `crate::tui::keybindings`'s own module doc).
//!
//! ## Supported subset (state it, do not imply more)
//!
//! - Modes: INSERT, NORMAL (`Esc` -> NORMAL; `i a I A o O` -> INSERT).
//! - Motions, with counts: `h j k l w b e 0 $ ^ gg G f F t T`.
//! - Operators `d c y` composed with any motion above, plus the linewise
//!   doubled forms `dd cc yy` and the to-end-of-line forms `D C`.
//! - `x X p P` (the unnamed register only -- no named registers, no
//!   macros), `u`/`Ctrl-R` (undo/redo), `.` (repeat the last
//!   text-changing command, replayed against the CURRENT cursor, not a
//!   recorded range).
//! - Visual `v`/`V` (characterwise/linewise) with `d y c`.
//! - `/pattern` + `Enter`: a literal (non-regex) forward search WITHIN the
//!   input text, wrapping; `Esc` cancels.
//! - Text objects, ONLY: `iw aw i" a" i( a(` -- reachable only right after
//!   an operator (`ciw`, `daw`, `yi"`, ...), never as bare motions and
//!   never inside Visual mode.
//!
//! NOT built, on purpose: no `:` command line, no registers beyond the
//! one unnamed register, no macros, no other text objects (`ib`, `it`,
//! paragraph/sentence objects, ...), no `;`/`,` find-repeat. Counts apply
//! to motions and `f`/`F`/`t`/`T`; `G`/`gg`'s own count is an absolute
//! target line, never a multiplier, and a count typed before a text
//! object is accepted but ignored (vim's own "count applies to some text
//! objects" nuance is out of scope here).
//!
//! ## Dot-repeat: replaying the literal keys
//!
//! `VimState::last_change` is not a structured description of the last
//! edit -- it is the literal sequence of [`ratatui::crossterm::event::
//! KeyEvent`]s that produced it, from the first key of the command (an
//! operator, `x`/`X`/`p`/`P`, or an INSERT-entry key) through to the key
//! that settled it back into NORMAL (for an INSERT-entry command, that
//! includes every key typed during the session and the terminating
//! `Esc`). `.` simply feeds that same sequence back through [`VimState::
//! dispatch`] at the CURRENT cursor position -- since every motion/text
//! object is resolved fresh from `(text, cursor)` each time, replaying the
//! literal keys reproduces vim's own "repeat relative to where you are
//! now" semantics for free, with no separate closure/parameter capture
//! needed. [`handle_key`] (the public entry point) is what maintains
//! `VimState::cmd_buffer`/`VimState::last_change` across calls;
//! `dispatch` itself has no idea recording is happening, which is what
//! keeps a `.`-triggered replay from re-recording itself into a new
//! `last_change` of just `['.']` (see `repeat_last_change`'s own doc).
//!
//! A disclosed limitation: only `Char` keys and the INSERT-exit `Esc` are
//! ever routed through this module while composing an INSERT session (see
//! "Precedence" above -- `Backspace`/arrows/`Home`/`End` keep editing via
//! the ordinary fixed chain even in vim mode). `.` after an insert session
//! that used one of those to self-correct replays only the net characters
//! this module itself saw, not the correction -- a best-effort repeat of
//! what was TYPED, not a perfect transcript of every keystroke.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The ceiling every vim count -- a bare count (`p`'s own `20000000p`) or
/// either half of an operator+motion pair (`9999d9999w`) -- is clamped to
/// the instant its digits are parsed (see [`VimState::accumulate_count_digit`]).
/// Nothing a person can read on screen is meaningfully bigger than this,
/// and without a ceiling a count this large either repeats a register text
/// enough times to allocate gigabytes on the UI thread (`p`) or loops a
/// motion enough times to never return promptly (`w`/`b`/`e`), even before
/// considering that accumulating digits with no limit at all risks the
/// backing `u32` overflowing outright. Documented in `docs/interactive.md`'s
/// vim section as this exact number.
const MAX_COUNT: u32 = 9999;

/// The input widget's current vim submode. `Default` is [`VimMode::Insert`]
/// -- turning `tui.editor_mode` on mid-session (or starting a session with
/// it already on) begins exactly where `emacs` mode always has, ordinary
/// typing, rather than dropping the operator into a NORMAL mode they never
/// asked to be in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum VimMode {
    #[default]
    Insert,
    Normal,
    /// `v` (characterwise, `linewise: false`) / `V` (linewise, `true`).
    /// `anchor` is the char index where the selection started; the live
    /// end is always `AppState::cursor` (the `cursor` parameter every
    /// method in this module threads through), never stored twice.
    Visual {
        anchor: usize,
        linewise: bool,
    },
    /// `/` was pressed: `query` is the pattern typed so far, shown live in
    /// the input tray (`view/input_box.rs`). `Enter` confirms (jumps the
    /// cursor to the next match, wrapping); `Esc` cancels back to NORMAL
    /// with the cursor untouched.
    Search {
        query: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Delete,
    Change,
    Yank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindKind {
    F,
    CapF,
    T,
    CapT,
}

impl FindKind {
    fn from_char(c: char) -> Self {
        match c {
            'f' => FindKind::F,
            'F' => FindKind::CapF,
            't' => FindKind::T,
            _ => FindKind::CapT,
        }
    }

    /// `(forward, till)` -- `till` means the motion lands one char short of
    /// the match (`t`/`T`), rather than ON it (`f`/`F`).
    fn params(self) -> (bool, bool) {
        match self {
            FindKind::F => (true, false),
            FindKind::CapF => (false, false),
            FindKind::T => (true, true),
            FindKind::CapT => (false, true),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextObjectScope {
    Inner,
    Around,
}

/// A second key this module is mid-way through collecting: the first key
/// of a two-(or more-)key NORMAL-mode command already consumed (`g`, an
/// operator followed by `i`/`a`, or `f`/`F`/`t`/`T`), waiting for the key
/// that completes it. Cleared (back to `None`) the instant that next key
/// arrives, successfully resolved or not -- an unrecognized completion
/// cancels the whole pending command rather than leaving it half-built.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum Awaiting {
    #[default]
    None,
    /// First `g` seen; `Option<u32>` is whatever count had been typed
    /// before it (an ABSOLUTE target line for `gg`, never a multiplier --
    /// see this module's own doc). A non-`g` completion cancels any
    /// pending operator too, exactly like an unrecognized motion does.
    G(Option<u32>),
    /// `f`/`F`/`t`/`T` seen; `u32` is the already-resolved total count
    /// (operator count * motion count, folded together at the moment the
    /// find key was pressed -- see [`VimState::take_total_count`]).
    Find(FindKind, u32),
    /// An operator's own `i`/`a` seen -- only ever entered while
    /// [`VimState::pending_operator`] is `Some` (text objects are not
    /// reachable as bare motions, nor from Visual mode -- see this
    /// module's own doc).
    TextObject(TextObjectScope),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpanKind {
    /// Vim's "exclusive" motions: the range is `[min, max)`.
    CharExclusive,
    /// Vim's "inclusive" motions: the range is `[min, max + 1)` -- the
    /// char AT the larger end is included.
    CharInclusive,
    /// A linewise motion (`gg`/`G`/`j`/`k`, operator-only) -- resolved by
    /// LINE INDEX, not a flat char range; see [`VimState::apply_resolved`].
    Lines,
}

#[derive(Debug, Clone, Default)]
struct Register {
    text: String,
    linewise: bool,
}

#[derive(Debug, Clone)]
struct UndoEntry {
    text: String,
    cursor: usize,
}

/// The vim engine's own session-local state -- everything that must be
/// CLASSIFIED (never left to drift) in `AppState::reset_for_new_session`'s
/// exhaustive destructure. All of it is transient: a fresh session gets a
/// fresh [`VimState::default`] (RESET, not carried) -- only `tui.
/// editor_mode` itself (`AppState::editor_mode`, CARRIED) decides whether
/// this module is consulted at all, the same CARRY/RESET split
/// `AppState::busy_input` (config-seeded, session-adjustable) and
/// `AppState::pending_steers` (per-session bookkeeping) already draw
/// between each other.
#[derive(Debug, Clone, Default)]
pub struct VimState {
    pub mode: VimMode,
    /// Digits accumulated for the CURRENT, not-yet-resolved count (reset to
    /// `None` the instant any command consumes it). `None` means "no count
    /// typed," distinct from `Some(0)`, which this module never produces
    /// (a bare `0` with nothing accumulated yet is the `^0`-start-of-line
    /// motion, never a count digit -- see [`VimState::normal_key`]).
    count: Option<u32>,
    /// An operator (`d`/`c`/`y`) already typed, waiting for the motion/
    /// text-object/doubled-operator key that completes it. The `Option<u32>`
    /// is the count that had been typed BEFORE the operator itself (e.g.
    /// the leading `2` in `2dw`), kept separate from [`Self::count`] (which
    /// restarts accumulating for the MOTION the instant the operator is
    /// seen) so the two can multiply together at completion
    /// ([`Self::take_total_count`]).
    pending_operator: Option<(Operator, Option<u32>)>,
    awaiting: Awaiting,
    register: Register,
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    /// The keys making up whatever NORMAL-mode command is currently being
    /// typed (plus every key of the INSERT session it may have opened) --
    /// see [`handle_key`]'s own doc, "Dot-repeat."
    cmd_buffer: Vec<KeyEvent>,
    /// Whether the command `cmd_buffer` is currently recording actually
    /// changes `text` (set by `apply_charwise`/`apply_linewise_lines`'s
    /// `Delete`/`Change` arms, `open_line`, `put`, and INSERT-entry --
    /// never by `Yank` or a bare motion), so a yank-only or motion-only
    /// command is never mistaken for something `.` should repeat.
    recording_mutating: bool,
    last_change: Vec<KeyEvent>,
    /// The cursor position `/` was pressed at -- [`Self::confirm_search`]
    /// searches forward starting just past this, wrapping back to it.
    search_origin: usize,
}

impl VimState {
    /// The input tray's own mode label (`view/input_box.rs`) -- `/help`'s
    /// text, never a bare steering id or a ULID, matches this module's own
    /// "state the concept" doc posture.
    pub fn mode_label(&self) -> &'static str {
        match &self.mode {
            VimMode::Insert => "INSERT",
            VimMode::Normal => "NORMAL",
            VimMode::Visual { linewise: true, .. } => "VISUAL LINE",
            VimMode::Visual {
                linewise: false, ..
            } => "VISUAL",
            VimMode::Search { .. } => "SEARCH",
        }
    }

    fn is_idle(&self) -> bool {
        self.pending_operator.is_none()
            && matches!(self.awaiting, Awaiting::None)
            && self.count.is_none()
    }

    /// The ONE funnel `input::handle_normal_key` calls on every path that
    /// does NOT hand `key` to this engine -- `resolve_keymap_action`
    /// claiming it first (history recall, kill/yank, Tab completion,
    /// `@`-mention accept, ...), or it falling through `wants_key`'s own
    /// refusal (arrows, `Backspace`/`Enter` outside a search) to the fixed
    /// chain below. Any of those can rewrite `state.input`/`state.cursor`
    /// out from under an in-flight NORMAL-mode command -- a pending
    /// operator (`d` of `dw`), an awaited completion key (`f`/`g`/a
    /// text-object prefix), an accumulating count, or an open Visual
    /// selection (its `anchor` is a char index into text that may no
    /// longer mean the same thing) -- so this drops all of them back to a
    /// clean NORMAL state rather than letting the NEXT vim key complete a
    /// command against text it was never typed against (regression: `d`,
    /// `Ctrl-P` to recall history, `w` used to delete into the just-recalled
    /// text).
    ///
    /// Deliberately narrow: INSERT and SEARCH are left completely alone.
    /// Both already have their own documented "a key that bypasses this
    /// engine keeps editing/composing the ordinary way" behavior (see this
    /// module's own doc, "Precedence") -- that is not a stale command, it
    /// is the session still in progress, so cancelling it here would turn
    /// every self-correcting `Backspace` in an INSERT session into an
    /// unwanted exit back to NORMAL. The undo/redo stacks and the unnamed
    /// register are untouched too -- they are committed history, not an
    /// in-flight command.
    pub fn cancel_pending(&mut self) {
        self.pending_operator = None;
        self.awaiting = Awaiting::None;
        self.count = None;
        if matches!(self.mode, VimMode::Visual { .. }) {
            self.mode = VimMode::Normal;
        }
    }

    fn take_count(&mut self) -> u32 {
        self.count.take().unwrap_or(1).clamp(1, MAX_COUNT)
    }

    /// The operator's own leading count (if any) times whatever count was
    /// typed since -- `2d3w` deletes six words. See [`Self::pending_
    /// operator`]'s own doc. The product is clamped to [`MAX_COUNT`] too
    /// (not just each factor on its own) -- `9999d9999w` would otherwise
    /// still ask for roughly a hundred million word-motions even though
    /// neither digit run alone exceeds the ceiling.
    fn take_total_count(&mut self) -> u32 {
        let op_count = self
            .pending_operator
            .and_then(|(_, c)| c)
            .unwrap_or(1)
            .clamp(1, MAX_COUNT);
        op_count.saturating_mul(self.take_count()).min(MAX_COUNT)
    }

    /// The ONE place a typed digit is folded into an accumulating count --
    /// shared by NORMAL and VISUAL mode's otherwise-identical digit arms --
    /// so [`MAX_COUNT`] is enforced exactly once, before the value is ever
    /// stored. Without this, a count like `20000000p` (finding: an
    /// unbounded `count` repeated a register text that many times,
    /// allocating gigabytes on the UI thread) or `4000000000w` (a `u32`
    /// this close to overflow) would accumulate past any sane ceiling, or
    /// past `u32::MAX` itself, before anything downstream ever gets a
    /// chance to clamp it. Clamping on EVERY digit (not just once at the
    /// end) also means the typed value never even transiently exceeds
    /// [`MAX_COUNT`], so no downstream arithmetic needs to worry about it.
    fn accumulate_count_digit(count: &mut Option<u32>, d: u32) {
        let next = count.unwrap_or(0).saturating_mul(10).saturating_add(d);
        *count = Some(next.min(MAX_COUNT));
    }

    fn push_undo(&mut self, text: &str, cursor: usize) {
        self.undo_stack.push(UndoEntry {
            text: text.to_string(),
            cursor,
        });
        self.redo_stack.clear();
    }

    fn undo(&mut self, text: &mut String, cursor: &mut usize) {
        if let Some(entry) = self.undo_stack.pop() {
            self.redo_stack.push(UndoEntry {
                text: text.clone(),
                cursor: *cursor,
            });
            *text = entry.text;
            *cursor = clamp_normal(text, entry.cursor);
        }
    }

    fn redo(&mut self, text: &mut String, cursor: &mut usize) {
        if let Some(entry) = self.redo_stack.pop() {
            self.undo_stack.push(UndoEntry {
                text: text.clone(),
                cursor: *cursor,
            });
            *text = entry.text;
            *cursor = clamp_normal(text, entry.cursor);
        }
    }

    /// `.`: replays [`Self::last_change`] -- the literal keys of the last
    /// text-changing command -- through [`Self::dispatch`] directly
    /// (NEVER through the public [`handle_key`] wrapper), so the replay
    /// does not re-enter the recording bookkeeping and overwrite `last_
    /// change` with just `['.']`. `dispatch` resolves every motion/text
    /// object fresh against the CURRENT `(text, cursor)`, so the replay
    /// naturally lands wherever vim itself would put it, with no stored
    /// range to re-target.
    fn repeat_last_change(&mut self, text: &mut String, cursor: &mut usize) {
        if self.last_change.is_empty() {
            return;
        }
        let keys = self.last_change.clone();
        for key in keys {
            self.dispatch(text, cursor, key);
        }
        // A replay is not itself a new change to remember -- without this,
        // the NEXT `.` would repeat only the literal `['.']` keystroke
        // that triggered this one (a no-op loop), since `dispatch`'s own
        // mutating arms set this flag as a side effect of replaying them.
        self.recording_mutating = false;
    }

    fn confirm_search(&mut self, text: &str, cursor: &mut usize) {
        let query = match std::mem::replace(&mut self.mode, VimMode::Normal) {
            VimMode::Search { query } => query,
            other => {
                self.mode = other;
                return;
            }
        };
        if query.is_empty() {
            return;
        }
        let chars: Vec<char> = text.chars().collect();
        let pattern: Vec<char> = query.chars().collect();
        let (n, m) = (chars.len(), pattern.len());
        if m == 0 || m > n {
            return;
        }
        let start_from = (self.search_origin + 1).min(n);
        let find_from = |range: std::ops::Range<usize>| -> Option<usize> {
            range
                .filter(|&i| i + m <= n)
                .find(|&i| chars[i..i + m] == pattern[..])
        };
        let found = find_from(start_from..n).or_else(|| find_from(0..start_from));
        if let Some(idx) = found {
            *cursor = clamp_normal(text, idx);
        }
    }

    fn search_push(&mut self, c: char) {
        if let VimMode::Search { query } = &mut self.mode {
            query.push(c);
        }
    }

    fn search_backspace(&mut self) {
        if let VimMode::Search { query } = &mut self.mode {
            query.pop();
        }
    }

    fn handle_escape(&mut self, text: &str, cursor: &mut usize) {
        match &self.mode {
            VimMode::Insert => {
                // One column back WITHIN THE CURRENT LINE, never crossing
                // into the previous line -- vim's own rule (staying at
                // column 0 of the SAME line when INSERT was entered right
                // after a newline, not jumping onto the line before it,
                // the way a naive `cursor - 1` on the flat char index
                // would).
                let (ls, _) = line_bounds(text, *cursor);
                let stepped_back = if *cursor > ls { *cursor - 1 } else { ls };
                *cursor = clamp_normal(text, stepped_back);
                self.mode = VimMode::Normal;
            }
            VimMode::Visual { .. } | VimMode::Search { .. } => {
                self.mode = VimMode::Normal;
            }
            VimMode::Normal => {}
        }
        self.pending_operator = None;
        self.awaiting = Awaiting::None;
        self.count = None;
    }

    fn insert_char(&mut self, text: &mut String, cursor: &mut usize, c: char) {
        insert_str_at(text, *cursor, &c.to_string());
        *cursor += 1;
        self.recording_mutating = true;
    }

    fn enter_insert(&mut self, text: &str, cursor: &mut usize, at: usize) {
        self.push_undo(text, *cursor);
        *cursor = at;
        self.mode = VimMode::Insert;
        self.recording_mutating = true;
    }

    fn open_line(&mut self, text: &mut String, cursor: &mut usize, below: bool) {
        self.push_undo(text, *cursor);
        let (ls, le) = line_bounds(text, *cursor);
        let at = if below { le } else { ls };
        insert_str_at(text, at, "\n");
        *cursor = if below { le + 1 } else { ls };
        self.mode = VimMode::Insert;
        self.recording_mutating = true;
    }

    /// Resolves a motion's result against whatever operator may be
    /// pending -- the ONE funnel every motion/`gg`/`G`/find/text-object
    /// completion goes through, so "apply the operator" vs. "just move
    /// the cursor" is decided in exactly one place.
    fn apply_resolved(
        &mut self,
        text: &mut String,
        cursor: &mut usize,
        target: usize,
        kind: SpanKind,
    ) {
        match self.pending_operator.take() {
            Some((op, _)) => match kind {
                SpanKind::Lines => {
                    let cur_line = line_index(text, *cursor);
                    let target_line = line_index(text, target);
                    let (first, last) = if target_line >= cur_line {
                        (cur_line, target_line)
                    } else {
                        (target_line, cur_line)
                    };
                    self.apply_linewise_lines(text, cursor, first, last, op);
                }
                _ => {
                    let (start, end) = motion_range(*cursor, target, kind);
                    self.apply_charwise(text, cursor, start, end, op);
                }
            },
            None => {
                *cursor = clamp_normal(text, target);
            }
        }
    }

    fn apply_charwise(
        &mut self,
        text: &mut String,
        cursor: &mut usize,
        start: usize,
        end: usize,
        op: Operator,
    ) {
        let (start, end) = (start.min(end), start.max(end));
        if start == end {
            self.recording_mutating = false;
            return;
        }
        match op {
            Operator::Yank => {
                self.register = Register {
                    text: substr(text, start, end),
                    linewise: false,
                };
                *cursor = clamp_normal(text, start);
                self.recording_mutating = false;
            }
            Operator::Delete => {
                self.push_undo(text, *cursor);
                self.register = Register {
                    text: substr(text, start, end),
                    linewise: false,
                };
                replace_range(text, start, end, "");
                *cursor = clamp_normal(text, start);
                self.recording_mutating = true;
            }
            Operator::Change => {
                self.push_undo(text, *cursor);
                self.register = Register {
                    text: substr(text, start, end),
                    linewise: false,
                };
                replace_range(text, start, end, "");
                *cursor = start.min(char_len(text));
                self.mode = VimMode::Insert;
                self.recording_mutating = true;
            }
        }
    }

    /// `dd`/`cc`/`yy`: `count` lines starting at the cursor's own line.
    fn apply_linewise(&mut self, text: &mut String, cursor: &mut usize, count: u32, op: Operator) {
        let lines_total = text.matches('\n').count() + 1;
        let cur_line = line_index(text, *cursor);
        let last = (cur_line + count.max(1) as usize - 1).min(lines_total - 1);
        self.apply_linewise_lines(text, cursor, cur_line, last, op);
    }

    /// Every linewise operator application, by 0-based `[first, last]`
    /// line indices (inclusive). `Delete`/`Yank` remove/copy the WHOLE
    /// lines, newlines and all (collapsing them out of existence, like
    /// vim's `dd`); `Change` removes only their CONTENT, leaving exactly
    /// one empty line behind at the same position, ready to type into
    /// (vim's `cc`) -- the two differ only in how many boundary newlines
    /// come along with the deletion, both derived from the SAME `[first_
    /// start, last_end)` span.
    fn apply_linewise_lines(
        &mut self,
        text: &mut String,
        cursor: &mut usize,
        first: usize,
        last: usize,
        op: Operator,
    ) {
        let lines: Vec<&str> = text.split('\n').collect();
        let first_start: usize = lines[..first].iter().map(|l| l.chars().count() + 1).sum();
        let last_end: usize = first_start
            + lines[first..=last]
                .iter()
                .map(|l| l.chars().count())
                .sum::<usize>()
            + (last - first);
        let total_len = char_len(text);
        let has_trailing_after = last_end < total_len;

        match op {
            Operator::Yank => {
                let yank_end = if has_trailing_after {
                    last_end + 1
                } else {
                    last_end
                };
                self.register = Register {
                    text: substr(text, first_start, yank_end),
                    linewise: true,
                };
                *cursor = clamp_normal(text, first_start);
                self.recording_mutating = false;
            }
            Operator::Change => {
                self.push_undo(text, *cursor);
                self.register = Register {
                    text: substr(text, first_start, last_end),
                    linewise: true,
                };
                replace_range(text, first_start, last_end, "");
                *cursor = first_start;
                self.mode = VimMode::Insert;
                self.recording_mutating = true;
            }
            Operator::Delete => {
                self.push_undo(text, *cursor);
                let (del_start, del_end) = if has_trailing_after {
                    (first_start, last_end + 1)
                } else if first_start > 0 {
                    (first_start - 1, last_end)
                } else {
                    (first_start, last_end)
                };
                self.register = Register {
                    text: substr(text, del_start, del_end),
                    linewise: true,
                };
                replace_range(text, del_start, del_end, "");
                *cursor = clamp_normal(text, del_start.min(char_len(text)));
                self.recording_mutating = true;
            }
        }
    }

    fn delete_forward(&mut self, text: &mut String, cursor: &mut usize, n: u32) {
        let (_, le) = line_bounds(text, *cursor);
        let end = (*cursor + n as usize).min(le);
        if end <= *cursor {
            self.recording_mutating = false;
            return;
        }
        self.apply_charwise(text, cursor, *cursor, end, Operator::Delete);
    }

    fn delete_backward(&mut self, text: &mut String, cursor: &mut usize, n: u32) {
        let (ls, _) = line_bounds(text, *cursor);
        let start = cursor.saturating_sub(n as usize).max(ls);
        if start >= *cursor {
            self.recording_mutating = false;
            return;
        }
        self.apply_charwise(text, cursor, start, *cursor, Operator::Delete);
    }

    fn put(&mut self, text: &mut String, cursor: &mut usize, after: bool, count: u32) {
        if self.register.text.is_empty() {
            self.recording_mutating = false;
            return;
        }
        self.push_undo(text, *cursor);
        let reg = self.register.clone();
        if reg.linewise {
            let body = reg.text.trim_end_matches('\n');
            let repeated = std::iter::repeat_n(body, count.max(1) as usize)
                .collect::<Vec<_>>()
                .join("\n");
            let len = char_len(text);
            let (ls, le) = line_bounds(text, *cursor);
            let (insert_at, to_insert, dest_is_after_insert) = if after {
                if le < len {
                    (le + 1, format!("{repeated}\n"), false)
                } else {
                    (len, format!("\n{repeated}"), true)
                }
            } else {
                (ls, format!("{repeated}\n"), false)
            };
            insert_str_at(text, insert_at, &to_insert);
            let dest_start = if dest_is_after_insert {
                insert_at + 1
            } else {
                insert_at
            };
            *cursor = first_nonblank(text, dest_start.min(char_len(text).saturating_sub(1)));
        } else {
            let repeated = reg.text.repeat(count.max(1) as usize);
            let insert_at = if after {
                (*cursor + 1).min(char_len(text))
            } else {
                *cursor
            };
            insert_str_at(text, insert_at, &repeated);
            let inserted_len = char_len(&repeated);
            *cursor = clamp_normal(text, insert_at + inserted_len.saturating_sub(1));
        }
        self.recording_mutating = true;
    }

    fn visual_key(
        &mut self,
        text: &mut String,
        cursor: &mut usize,
        c: char,
        anchor: usize,
        linewise: bool,
    ) {
        if c.is_ascii_digit() && !(c == '0' && self.count.is_none()) {
            let d = c.to_digit(10).unwrap();
            Self::accumulate_count_digit(&mut self.count, d);
            return;
        }
        match c {
            'v' => {
                self.mode = if linewise {
                    VimMode::Visual {
                        anchor,
                        linewise: false,
                    }
                } else {
                    VimMode::Normal
                };
                self.count = None;
            }
            'V' => {
                self.mode = if linewise {
                    VimMode::Normal
                } else {
                    VimMode::Visual {
                        anchor,
                        linewise: true,
                    }
                };
                self.count = None;
            }
            'd' | 'y' | 'c' => {
                self.count = None;
                let op = match c {
                    'd' => Operator::Delete,
                    'y' => Operator::Yank,
                    _ => Operator::Change,
                };
                self.mode = VimMode::Normal;
                if linewise {
                    let a = line_index(text, anchor);
                    let b = line_index(text, *cursor);
                    let (first, last) = if a <= b { (a, b) } else { (b, a) };
                    self.apply_linewise_lines(text, cursor, first, last, op);
                } else {
                    let lo = anchor.min(*cursor);
                    let hi = anchor.max(*cursor);
                    let end = (hi + 1).min(char_len(text));
                    self.apply_charwise(text, cursor, lo, end, op);
                }
            }
            other => {
                let n = self.take_count();
                if let Some((target, _)) = resolve_motion(text, *cursor, other, n) {
                    *cursor = clamp_normal(text, target);
                }
            }
        }
    }

    fn normal_key(&mut self, text: &mut String, cursor: &mut usize, c: char) {
        // Step 1: resolve whatever two-(or more-)key command is already
        // mid-flight.
        if !matches!(self.awaiting, Awaiting::None) {
            let awaiting = std::mem::replace(&mut self.awaiting, Awaiting::None);
            match awaiting {
                Awaiting::G(saved_count) => {
                    if c == 'g' {
                        let target = goto_line_target(text, saved_count, true);
                        self.apply_resolved(text, cursor, target, SpanKind::Lines);
                    } else {
                        self.pending_operator = None;
                    }
                }
                Awaiting::Find(kind, count) => {
                    let (forward, till) = kind.params();
                    match find_in_line(text, *cursor, c, forward, till, count) {
                        Some(target) => {
                            self.apply_resolved(text, cursor, target, SpanKind::CharInclusive)
                        }
                        None => self.pending_operator = None,
                    }
                }
                Awaiting::TextObject(scope) => {
                    if let Some((op, _)) = self.pending_operator.take() {
                        let around = matches!(scope, TextObjectScope::Around);
                        let range = match c {
                            'w' => word_object(text, *cursor, around),
                            '"' => quote_object(text, *cursor, '"', around),
                            '(' => paren_object(text, *cursor, '(', ')', around),
                            _ => None,
                        };
                        match range {
                            Some((s, e)) => self.apply_charwise(text, cursor, s, e, op),
                            None => self.recording_mutating = false,
                        }
                    }
                }
                Awaiting::None => unreachable!("guarded above"),
            }
            return;
        }

        // Step 2: count digits.
        if c.is_ascii_digit() && !(c == '0' && self.count.is_none()) {
            let d = c.to_digit(10).unwrap();
            Self::accumulate_count_digit(&mut self.count, d);
            return;
        }

        // Step 3: `g`/`G`/`f`/`F`/`t`/`T` -- identical whether or not an
        // operator is pending; completion always funnels through
        // `apply_resolved`, which branches on `pending_operator` itself.
        match c {
            'g' => {
                let saved = self.count.take();
                self.awaiting = Awaiting::G(saved);
                return;
            }
            'G' => {
                let target_line = self.count.take();
                let target = goto_line_target(text, target_line, false);
                self.apply_resolved(text, cursor, target, SpanKind::Lines);
                return;
            }
            'f' | 'F' | 't' | 'T' => {
                let kind = FindKind::from_char(c);
                let count = self.take_total_count();
                self.awaiting = Awaiting::Find(kind, count);
                return;
            }
            _ => {}
        }

        // Step 4: an operator is pending -- doubled operator, a text-object
        // prefix, or a motion that completes it.
        if let Some((op, _)) = self.pending_operator {
            match c {
                'd' if op == Operator::Delete => {
                    let n = self.take_total_count();
                    self.pending_operator = None;
                    self.apply_linewise(text, cursor, n, op);
                }
                'c' if op == Operator::Change => {
                    let n = self.take_total_count();
                    self.pending_operator = None;
                    self.apply_linewise(text, cursor, n, op);
                }
                'y' if op == Operator::Yank => {
                    let n = self.take_total_count();
                    self.pending_operator = None;
                    self.apply_linewise(text, cursor, n, op);
                }
                'i' => self.awaiting = Awaiting::TextObject(TextObjectScope::Inner),
                'a' => self.awaiting = Awaiting::TextObject(TextObjectScope::Around),
                // `cw`: vim's own special case, carved out ahead of the
                // generic motion arm below -- see `change_w_span`'s own
                // doc. (There is no `W`/WORD motion in this module's
                // vocabulary at all -- see the module doc's "Supported
                // subset" -- so `cW` is not a reachable command here.)
                // `cw`: vim's own special case, carved out ahead of the
                // generic motion arm below -- see `change_w_span`'s own
                // doc. (There is no `W`/WORD motion in this module's
                // vocabulary at all -- see the module doc's "Supported
                // subset" -- so `cW` is not a reachable command here.)
                'w' if op == Operator::Change => {
                    let n = self.take_total_count();
                    self.pending_operator = None;
                    let (target, kind) = change_w_span(text, *cursor, n);
                    let (start, end) = motion_range(*cursor, target, kind);
                    self.apply_charwise(text, cursor, start, end, op);
                }
                other => {
                    let n = self.take_total_count();
                    match resolve_motion(text, *cursor, other, n) {
                        Some((target, kind)) => self.apply_resolved(text, cursor, target, kind),
                        None => {
                            self.pending_operator = None;
                            self.recording_mutating = false;
                        }
                    }
                }
            }
            return;
        }

        // Step 5: bare commands -- no operator pending.
        match c {
            'i' => self.enter_insert(text, cursor, *cursor),
            'a' => {
                let at = (*cursor + 1).min(char_len(text));
                self.enter_insert(text, cursor, at);
            }
            'I' => {
                let (ls, _) = line_bounds(text, *cursor);
                self.enter_insert(text, cursor, ls);
            }
            'A' => {
                let (_, le) = line_bounds(text, *cursor);
                self.enter_insert(text, cursor, le);
            }
            'o' => self.open_line(text, cursor, true),
            'O' => self.open_line(text, cursor, false),
            'v' => {
                self.mode = VimMode::Visual {
                    anchor: *cursor,
                    linewise: false,
                };
                self.count = None;
            }
            'V' => {
                self.mode = VimMode::Visual {
                    anchor: *cursor,
                    linewise: true,
                };
                self.count = None;
            }
            '/' => {
                self.search_origin = *cursor;
                self.mode = VimMode::Search {
                    query: String::new(),
                };
                self.count = None;
            }
            'u' => {
                self.count = None;
                self.undo(text, cursor);
            }
            '.' => {
                self.count = None;
                self.repeat_last_change(text, cursor);
            }
            'd' | 'c' | 'y' => {
                let op = match c {
                    'd' => Operator::Delete,
                    'c' => Operator::Change,
                    _ => Operator::Yank,
                };
                self.pending_operator = Some((op, self.count.take()));
            }
            'x' => {
                let n = self.take_count();
                self.delete_forward(text, cursor, n);
            }
            'X' => {
                let n = self.take_count();
                self.delete_backward(text, cursor, n);
            }
            'D' => {
                self.count = None;
                let start = *cursor;
                let (_, le) = line_bounds(text, start);
                self.apply_charwise(text, cursor, start, le, Operator::Delete);
            }
            'C' => {
                self.count = None;
                let start = *cursor;
                let (_, le) = line_bounds(text, start);
                self.apply_charwise(text, cursor, start, le, Operator::Change);
            }
            'p' => {
                let n = self.take_count();
                self.put(text, cursor, true, n);
            }
            'P' => {
                let n = self.take_count();
                self.put(text, cursor, false, n);
            }
            other => {
                let n = self.take_count();
                self.apply_resolved_bare(text, cursor, other, n);
            }
        }
    }

    /// A bare (no operator) motion -- [`Self::apply_resolved`] would do
    /// the identical thing (`pending_operator` is always `None` on this
    /// path), but spelled out separately so an unrecognized key is
    /// silently swallowed rather than inserted as text, matching vim's own
    /// "an unknown NORMAL-mode command is not a character" rule.
    fn apply_resolved_bare(&mut self, text: &str, cursor: &mut usize, c: char, count: u32) {
        if let Some((target, _)) = resolve_motion(text, *cursor, c, count) {
            *cursor = clamp_normal(text, target);
        }
    }

    fn dispatch(&mut self, text: &mut String, cursor: &mut usize, key: KeyEvent) {
        if key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R'))
        {
            if matches!(self.mode, VimMode::Normal) {
                self.redo(text, cursor);
            }
            return;
        }
        if matches!(key.code, KeyCode::Esc) {
            self.handle_escape(text, cursor);
            return;
        }
        if matches!(self.mode, VimMode::Search { .. }) {
            match key.code {
                KeyCode::Enter => self.confirm_search(text, cursor),
                KeyCode::Backspace => self.search_backspace(),
                // Board item `01M1YVJNS575YN5DCQG9BKZR4E` review round: an
                // unbound Ctrl/Alt chord must be ignored here exactly like
                // the general (non-search) dispatch path already does --
                // without this guard an unbound chord such as `Ctrl-X`
                // typed while composing a search appended a literal "x" to
                // the query, the one place in this module that still
                // violated `docs/interactive.md`'s "a Ctrl or Alt chord
                // that is not bound to anything is ignored rather than
                // typed" rule.
                KeyCode::Char(c) if is_unmodified_char(key) => self.search_push(c),
                _ => {}
            }
            return;
        }
        let KeyCode::Char(c) = key.code else {
            return;
        };
        match self.mode.clone() {
            VimMode::Insert => self.insert_char(text, cursor, c),
            VimMode::Normal => self.normal_key(text, cursor, c),
            VimMode::Visual { anchor, linewise } => {
                self.visual_key(text, cursor, c, anchor, linewise)
            }
            VimMode::Search { .. } => unreachable!("handled above"),
        }
    }
}

/// Whether `key` is one this module wants to see at all -- the ONE gate
/// `input::handle_normal_key` consults (after `resolve_keymap_action`'s own
/// first refusal); see this module's own "Precedence" doc.
pub fn wants_key(state: &VimState, key: KeyEvent) -> bool {
    if matches!(key.code, KeyCode::Esc) {
        return true;
    }
    if key.modifiers == KeyModifiers::CONTROL
        && matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R'))
    {
        return true;
    }
    if matches!(state.mode, VimMode::Search { .. }) {
        // Board item `01M1YVJNS575YN5DCQG9BKZR4E` review round: `Char`
        // here must pass through the SAME Ctrl/Alt gate as the general
        // branch below -- an unbound chord is ignored, never typed, in
        // every vim submode, search included (`dispatch`'s own Search
        // branch has the matching half of this fix).
        return matches!(key.code, KeyCode::Enter | KeyCode::Backspace) || is_unmodified_char(key);
    }
    is_unmodified_char(key)
}

/// A plain letter/digit with neither Ctrl nor Alt held -- `Shift` alone
/// (an ordinary capital letter) is fine. The one check [`wants_key`] and
/// [`VimState::dispatch`]'s Search branch both apply, so "an unbound
/// Ctrl/Alt chord is ignored, not typed" (`docs/interactive.md`) holds the
/// same way in every vim submode.
fn is_unmodified_char(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && !(key.modifiers.contains(KeyModifiers::CONTROL)
            || key.modifiers.contains(KeyModifiers::ALT))
}

/// The public entry point: handles `key` against `(text, cursor)`,
/// maintaining `VimState::cmd_buffer`/`VimState::last_change` across
/// calls for `.` -- see this module's own "Dot-repeat" doc. Only ever
/// called once [`wants_key`] has already said yes.
pub fn handle_key(state: &mut VimState, text: &mut String, cursor: &mut usize, key: KeyEvent) {
    let fresh = matches!(state.mode, VimMode::Normal) && state.is_idle();
    if fresh {
        state.cmd_buffer.clear();
        state.recording_mutating = false;
    }
    state.cmd_buffer.push(key);

    state.dispatch(text, cursor, key);

    let settled = matches!(state.mode, VimMode::Normal) && state.is_idle();
    if settled {
        if state.recording_mutating && !state.cmd_buffer.is_empty() {
            state.last_change = std::mem::take(&mut state.cmd_buffer);
        } else {
            state.cmd_buffer.clear();
        }
        state.recording_mutating = false;
    }
}

// ---------------------------------------------------------------------
// Pure (text, cursor) helpers -- no `VimState`, no `AppState`.
// ---------------------------------------------------------------------

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// The byte offset of the `idx`-th char in `text` (always a char
/// boundary), or `text.len()` if `idx` is at or past the end -- mirrors
/// `input.rs::byte_index` exactly (this module keeps its own copy rather
/// than sharing one, since it must stay free of any dependency on
/// `AppState`/that module's own private helpers).
fn byte_at(text: &str, idx: usize) -> usize {
    text.char_indices()
        .nth(idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn substr(text: &str, start: usize, end: usize) -> String {
    text[byte_at(text, start)..byte_at(text, end)].to_string()
}

fn replace_range(text: &mut String, start: usize, end: usize, with: &str) {
    let (s, e) = (byte_at(text, start), byte_at(text, end));
    text.replace_range(s..e, with);
}

fn insert_str_at(text: &mut String, at: usize, s: &str) {
    let b = byte_at(text, at);
    text.insert_str(b, s);
}

/// `(line, col)`, both char indices -- mirrors `AppState::cursor_line_col`.
fn line_col(text: &str, cursor: usize) -> (usize, usize) {
    let mut line = 0;
    let mut col = 0;
    for c in text.chars().take(cursor) {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

fn line_index(text: &str, cursor: usize) -> usize {
    line_col(text, cursor).0
}

/// The current line's `[start, end)` content bounds (char indices,
/// excluding the `\n`) -- mirrors `input.rs::current_line_bounds`.
fn line_bounds(text: &str, cursor: usize) -> (usize, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    let cursor = cursor.min(char_len(text));
    let (line_idx, _) = line_col(text, cursor);
    let start: usize = lines[..line_idx]
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum();
    let end = start + lines[line_idx].chars().count();
    (start, end)
}

/// The NORMAL/Visual-mode cursor clamp: never past the last char of its
/// own line (an empty line is its own exception -- there is nothing to
/// clamp to). INSERT mode never calls this -- it alone may rest one past
/// the last char, where typing appends.
fn clamp_normal(text: &str, cursor: usize) -> usize {
    let c = cursor.min(char_len(text));
    let (ls, le) = line_bounds(text, c);
    if le > ls {
        c.min(le - 1)
    } else {
        c
    }
}

fn first_nonblank(text: &str, cursor: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let (ls, le) = line_bounds(text, cursor);
    let mut i = ls;
    while i < le && chars[i].is_whitespace() {
        i += 1;
    }
    if i < le {
        i
    } else {
        ls
    }
}

fn line_vertical(text: &str, cursor: usize, delta: isize) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    let (line_idx, col) = line_col(text, cursor);
    let target_line = (line_idx as isize + delta).clamp(0, lines.len() as isize - 1) as usize;
    let target_len = lines[target_line].chars().count();
    let new_col = col.min(target_len);
    let start: usize = lines[..target_line]
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum();
    start + new_col
}

/// `0` = whitespace, `1` = "keyword" (alphanumeric/`_`), `2` = punctuation
/// -- vim's own three-way word classification.
fn char_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

fn word_forward(text: &str, pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut pos = pos.min(n);
    if pos >= n {
        return n;
    }
    let start_class = char_class(chars[pos]);
    if start_class != 0 {
        while pos < n && char_class(chars[pos]) == start_class {
            pos += 1;
        }
    }
    while pos < n && char_class(chars[pos]) == 0 {
        pos += 1;
    }
    pos
}

fn word_backward(text: &str, pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    if pos == 0 {
        return 0;
    }
    let mut pos = pos - 1;
    while pos > 0 && char_class(chars[pos]) == 0 {
        pos -= 1;
    }
    if char_class(chars[pos]) == 0 {
        return 0;
    }
    let class = char_class(chars[pos]);
    while pos > 0 && char_class(chars[pos - 1]) == class {
        pos -= 1;
    }
    pos
}

fn word_end(text: &str, pos: usize) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return 0;
    }
    let mut pos = (pos + 1).min(n);
    while pos < n && char_class(chars[pos]) == 0 {
        pos += 1;
    }
    if pos >= n {
        return n - 1;
    }
    let class = char_class(chars[pos]);
    while pos + 1 < n && char_class(chars[pos + 1]) == class {
        pos += 1;
    }
    pos
}

/// Applies `step` up to `count` times, stopping the instant it stops
/// moving the position -- `w`/`b`/`e` (and [`change_w_span`]'s own `cw`
/// special case) all repeat a per-call, full-text char scan, so a count
/// larger than the draft actually has words must not keep calling it once
/// there is nowhere left to go. [`MAX_COUNT`] already bounds how large
/// `count` itself can be, but a short draft can still exhaust every word
/// in it long before `count` runs out (`9999w` on a ten-word line) -- this
/// is what makes that land promptly on the last word rather than looping
/// the full, now-pointless remainder.
fn repeat_until_stuck(mut pos: usize, count: u32, mut step: impl FnMut(usize) -> usize) -> usize {
    for _ in 0..count {
        let next = step(pos);
        if next == pos {
            break;
        }
        pos = next;
    }
    pos
}

/// `cw`: vim's own special-cased motion, never a plain `dw`-shaped `w`.
/// When the cursor sits on a non-blank character, `cw` changes through the
/// END of the CURRENT word (exactly like `ce`), leaving any trailing
/// whitespace untouched -- vim's own reasoning is that "change this word"
/// should not also eat the gap after it, even though `dw`/`yw` (which
/// really do mean "through the start of the next word") do. On a blank,
/// there is no "current word" whose trailing gap needs protecting, so
/// `cw` behaves exactly like a bare `w` there (vim's own rule too). Both
/// branches honor `count` the same way `resolve_motion`'s own `e`/`w` do
/// (`2cw` changes through the end of the SECOND word), and
/// [`repeat_until_stuck`] keeps either from looping past the end of a
/// short draft. There is no `W`/WORD motion anywhere in this module (see
/// the module doc's "Supported subset"), so this never needs a `cW` case.
fn change_w_span(text: &str, cursor: usize, count: u32) -> (usize, SpanKind) {
    let chars: Vec<char> = text.chars().collect();
    let on_word = chars
        .get(cursor)
        .map(|&c| char_class(c) != 0)
        .unwrap_or(false);
    if on_word {
        (
            repeat_until_stuck(cursor, count, |p| word_end(text, p)),
            SpanKind::CharInclusive,
        )
    } else {
        (
            repeat_until_stuck(cursor, count, |p| word_forward(text, p)),
            SpanKind::CharExclusive,
        )
    }
}

fn resolve_motion(text: &str, cursor: usize, c: char, count: u32) -> Option<(usize, SpanKind)> {
    let len = char_len(text);
    let count = count.max(1);
    match c {
        'h' => Some((
            cursor.saturating_sub(count as usize),
            SpanKind::CharExclusive,
        )),
        'l' => Some(((cursor + count as usize).min(len), SpanKind::CharExclusive)),
        '0' => Some((line_bounds(text, cursor).0, SpanKind::CharExclusive)),
        '^' => Some((first_nonblank(text, cursor), SpanKind::CharExclusive)),
        '$' => {
            let (ls, le) = line_bounds(text, cursor);
            Some((if le > ls { le - 1 } else { ls }, SpanKind::CharInclusive))
        }
        'w' => Some((
            repeat_until_stuck(cursor, count, |p| word_forward(text, p)),
            SpanKind::CharExclusive,
        )),
        'b' => Some((
            repeat_until_stuck(cursor, count, |p| word_backward(text, p)),
            SpanKind::CharExclusive,
        )),
        'e' => Some((
            repeat_until_stuck(cursor, count, |p| word_end(text, p)),
            SpanKind::CharInclusive,
        )),
        'j' => Some((line_vertical(text, cursor, count as isize), SpanKind::Lines)),
        'k' => Some((
            line_vertical(text, cursor, -(count as isize)),
            SpanKind::Lines,
        )),
        _ => None,
    }
}

fn motion_range(cursor: usize, target: usize, kind: SpanKind) -> (usize, usize) {
    match kind {
        SpanKind::CharExclusive => (cursor.min(target), cursor.max(target)),
        SpanKind::CharInclusive => {
            let (a, b) = (cursor.min(target), cursor.max(target));
            (a, b + 1)
        }
        SpanKind::Lines => unreachable!("Lines spans resolve by line index, not a flat char range"),
    }
}

fn find_in_line(
    text: &str,
    cursor: usize,
    target: char,
    forward: bool,
    till: bool,
    count: u32,
) -> Option<usize> {
    let chars: Vec<char> = text.chars().collect();
    let (ls, le) = line_bounds(text, cursor);
    let mut matches = Vec::new();
    if forward {
        for (i, &ch) in chars.iter().enumerate().take(le).skip(cursor + 1) {
            if ch == target {
                matches.push(i);
            }
        }
    } else {
        for i in (ls..cursor).rev() {
            if chars[i] == target {
                matches.push(i);
            }
        }
    }
    let idx = *matches.get((count.max(1) as usize).saturating_sub(1))?;
    Some(if till {
        if forward {
            idx - 1
        } else {
            idx + 1
        }
    } else {
        idx
    })
}

fn goto_line_target(text: &str, target_line_1based: Option<u32>, default_first: bool) -> usize {
    let lines: Vec<&str> = text.split('\n').collect();
    let n = lines.len();
    let line_idx = match target_line_1based {
        Some(n1) => (n1 as usize).saturating_sub(1).min(n - 1),
        None => {
            if default_first {
                0
            } else {
                n - 1
            }
        }
    };
    let start: usize = lines[..line_idx]
        .iter()
        .map(|l| l.chars().count() + 1)
        .sum();
    let mut offset = 0;
    for c in lines[line_idx].chars() {
        if c.is_whitespace() {
            offset += 1;
        } else {
            break;
        }
    }
    if offset >= lines[line_idx].chars().count() {
        offset = 0;
    }
    start + offset
}

/// `iw`/`aw`: the run of characters sharing `pos`'s own word class
/// (keyword, punctuation, or whitespace). `aw` additionally eats trailing
/// whitespace, or -- only when there is none -- leading whitespace.
fn word_object(text: &str, pos: usize, around: bool) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return None;
    }
    let pos = pos.min(n - 1);
    let class = char_class(chars[pos]);
    let mut start = pos;
    while start > 0 && char_class(chars[start - 1]) == class {
        start -= 1;
    }
    let mut end = pos;
    while end + 1 < n && char_class(chars[end + 1]) == class {
        end += 1;
    }
    let mut range_end = end + 1;
    if around {
        let mut trailing = range_end;
        while trailing < n && char_class(chars[trailing]) == 0 {
            trailing += 1;
        }
        if trailing > range_end {
            range_end = trailing;
        } else {
            while start > 0 && char_class(chars[start - 1]) == 0 {
                start -= 1;
            }
        }
    }
    Some((start, range_end))
}

/// `i"`/`a"`: the nearest quote PAIR on the CURRENT LINE enclosing (or, if
/// `pos` sits before any pair, the next) `pos` -- quotes pair up
/// consecutively left to right, vim's own rule.
fn quote_object(text: &str, pos: usize, quote: char, around: bool) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return None;
    }
    let pos = pos.min(n - 1);
    let (ls, le) = line_bounds(text, pos);
    let positions: Vec<usize> = (ls..le).filter(|&i| chars[i] == quote).collect();
    if positions.len() < 2 {
        return None;
    }
    let mut chosen = None;
    let mut i = 0;
    while i + 1 < positions.len() {
        let (open, close) = (positions[i], positions[i + 1]);
        if pos <= close {
            chosen = Some((open, close));
            break;
        }
        i += 2;
    }
    let (open, close) = chosen?;
    if around {
        let mut end = close + 1;
        let mut start = open;
        if end < le && chars[end] == ' ' {
            end += 1;
        } else if start > ls && chars[start - 1] == ' ' {
            start -= 1;
        }
        Some((start, end))
    } else {
        Some((open + 1, close))
    }
}

/// `i(`/`a(`: the nearest enclosing balanced pair around `pos`, searched
/// across the WHOLE text (parens, unlike quotes, may legitimately span a
/// multi-line draft).
fn paren_object(
    text: &str,
    pos: usize,
    open_c: char,
    close_c: char,
    around: bool,
) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return None;
    }
    let pos = pos.min(n - 1);
    let mut depth = 0i32;
    let mut open = None;
    for i in (0..=pos).rev() {
        let ch = chars[i];
        if ch == open_c {
            if depth == 0 {
                open = Some(i);
                break;
            }
            depth -= 1;
        } else if ch == close_c && i != pos {
            depth += 1;
        }
    }
    let open = open?;
    let mut depth = 0i32;
    let mut close = None;
    for (k, &ch) in chars.iter().enumerate().skip(open + 1) {
        if ch == open_c {
            depth += 1;
        } else if ch == close_c {
            if depth == 0 {
                close = Some(k);
                break;
            }
            depth -= 1;
        }
    }
    let close = close?;
    if around {
        Some((open, close + 1))
    } else {
        Some((open + 1, close))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(state: &mut VimState, text: &mut String, cursor: &mut usize, code: KeyCode) {
        press_mod(state, text, cursor, code, KeyModifiers::NONE);
    }

    fn press_mod(
        state: &mut VimState,
        text: &mut String,
        cursor: &mut usize,
        code: KeyCode,
        mods: KeyModifiers,
    ) {
        handle_key(state, text, cursor, KeyEvent::new(code, mods));
    }

    fn press_str(state: &mut VimState, text: &mut String, cursor: &mut usize, s: &str) {
        for c in s.chars() {
            press(state, text, cursor, KeyCode::Char(c));
        }
    }

    fn normal() -> VimState {
        VimState {
            mode: VimMode::Normal,
            ..VimState::default()
        }
    }

    #[test]
    fn ciw_changes_the_word_under_the_cursor() {
        let mut state = normal();
        let mut text = "hello world".to_string();
        let mut cursor = 2;

        press_str(&mut state, &mut text, &mut cursor, "ciw");
        assert_eq!(state.mode, VimMode::Insert);
        assert_eq!(text, " world");
        assert_eq!(cursor, 0);

        press_str(&mut state, &mut text, &mut cursor, "bye");
        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);

        assert_eq!(text, "bye world");
        assert_eq!(cursor, 2);
        assert_eq!(state.mode, VimMode::Normal);
    }

    #[test]
    fn dd_deletes_the_whole_current_line() {
        let mut state = normal();
        let mut text = "first\nsecond\nthird".to_string();
        let mut cursor = 6; // start of "second"

        press_str(&mut state, &mut text, &mut cursor, "dd");

        assert_eq!(text, "first\nthird");
        assert_eq!(cursor, 6);
    }

    #[test]
    fn dd_on_the_last_line_leaves_no_stray_blank_line() {
        let mut state = normal();
        let mut text = "a\nb\nc".to_string();
        let mut cursor = 4; // the 'c'

        press_str(&mut state, &mut text, &mut cursor, "dd");

        assert_eq!(text, "a\nb");
    }

    #[test]
    fn three_w_moves_forward_three_words() {
        let mut state = normal();
        let mut text = "alpha beta gamma delta".to_string();
        let mut cursor = 0;

        press_str(&mut state, &mut text, &mut cursor, "3w");

        assert_eq!(cursor, 17, "cursor must land on the start of \"delta\"");
        assert_eq!(
            text, "alpha beta gamma delta",
            "a bare motion must never mutate the text"
        );
    }

    #[test]
    fn u_and_ctrl_r_undo_and_redo_a_delete() {
        let mut state = normal();
        let mut text = "hello".to_string();
        let mut cursor = 0;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('x'));
        assert_eq!(text, "ello");

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('u'));
        assert_eq!(text, "hello", "u must restore the pre-delete text");

        press_mod(
            &mut state,
            &mut text,
            &mut cursor,
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        );
        assert_eq!(text, "ello", "Ctrl-R must redo the delete u just undid");
    }

    #[test]
    fn dot_repeats_the_last_change_at_the_new_cursor_position() {
        let mut state = normal();
        let mut text = "hello world".to_string();
        let mut cursor = 0;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('x'));
        assert_eq!(text, "ello world");
        assert_eq!(cursor, 0);

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('.'));
        assert_eq!(
            text, "llo world",
            ". must repeat the delete at the CURRENT cursor"
        );
    }

    #[test]
    fn dot_repeats_a_full_change_insert_session() {
        let mut state = normal();
        let mut text = "cat cat".to_string();
        let mut cursor = 0;

        press_str(&mut state, &mut text, &mut cursor, "ciw");
        press_str(&mut state, &mut text, &mut cursor, "dog");
        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);
        assert_eq!(text, "dog cat");

        // Move onto the second "cat" and repeat.
        press_str(&mut state, &mut text, &mut cursor, "w");
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('.'));

        assert_eq!(text, "dog dog");
    }

    #[test]
    fn dt_paren_deletes_up_to_but_not_including_the_target() {
        let mut state = normal();
        let mut text = "abc)def".to_string();
        let mut cursor = 0;

        press_str(&mut state, &mut text, &mut cursor, "dt)");

        assert_eq!(text, ")def");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn ci_quote_changes_inside_the_quoted_text() {
        let mut state = normal();
        let mut text = "say \"hello\" now".to_string();
        let mut cursor = 6; // the 'e' of "hello"

        press_str(&mut state, &mut text, &mut cursor, "ci\"");
        assert_eq!(text, "say \"\" now");
        assert_eq!(cursor, 5);

        press_str(&mut state, &mut text, &mut cursor, "hi");
        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);
        assert_eq!(text, "say \"hi\" now");
    }

    #[test]
    fn ci_paren_changes_inside_the_parens() {
        let mut state = normal();
        let mut text = "call(a, b)".to_string();
        let mut cursor = 6; // inside the parens, right after the comma

        press_str(&mut state, &mut text, &mut cursor, "ci(");
        assert_eq!(text, "call()");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn visual_select_then_delete() {
        let mut state = normal();
        let mut text = "abcdef".to_string();
        let mut cursor = 1;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('v'));
        assert_eq!(
            state.mode,
            VimMode::Visual {
                anchor: 1,
                linewise: false
            }
        );
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('l'));
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('d'));

        assert_eq!(text, "adef");
        assert_eq!(cursor, 1);
        assert_eq!(state.mode, VimMode::Normal);
    }

    #[test]
    fn visual_line_yank_then_put_duplicates_the_line() {
        let mut state = normal();
        let mut text = "one\ntwo\nthree".to_string();
        let mut cursor = 4; // "two"

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('V'));
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('y'));
        assert_eq!(text, "one\ntwo\nthree", "a yank must never mutate the text");

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('p'));
        assert_eq!(text, "one\ntwo\ntwo\nthree");
    }

    #[test]
    fn slash_search_jumps_to_the_next_match_and_wraps() {
        let mut state = normal();
        let mut text = "find foo here".to_string();
        let mut cursor = 0;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('/'));
        assert_eq!(
            state.mode,
            VimMode::Search {
                query: String::new()
            }
        );
        press_str(&mut state, &mut text, &mut cursor, "foo");
        press(&mut state, &mut text, &mut cursor, KeyCode::Enter);

        assert_eq!(state.mode, VimMode::Normal);
        assert_eq!(cursor, 5);
    }

    #[test]
    fn esc_while_composing_a_search_cancels_without_moving() {
        let mut state = normal();
        let mut text = "find foo here".to_string();
        let mut cursor = 0;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('/'));
        press_str(&mut state, &mut text, &mut cursor, "zzz");
        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);

        assert_eq!(state.mode, VimMode::Normal);
        assert_eq!(cursor, 0);
    }

    #[test]
    fn esc_from_insert_steps_the_cursor_back_one_and_clamps() {
        let mut state = VimState::default(); // starts in Insert
        let mut text = "hi".to_string();
        let mut cursor = 2;

        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);

        assert_eq!(state.mode, VimMode::Normal);
        assert_eq!(cursor, 1);
    }

    /// Regression: `Esc` right after a newline (cursor at column 0 of a
    /// line that is NOT the first line) must stay on THAT line's own
    /// column 0, never step back across the `\n` onto the PREVIOUS line's
    /// last character -- a naive `cursor - 1` on the flat char index does
    /// exactly that, which silently redirected a `dd` onto the wrong line
    /// entirely the first time this was wired up.
    #[test]
    fn esc_at_the_start_of_a_non_first_line_stays_on_that_line() {
        let mut state = VimState::default();
        let mut text = "first\nsecond".to_string();
        let mut cursor = 6; // column 0 of "second"

        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);

        assert_eq!(state.mode, VimMode::Normal);
        assert_eq!(
            cursor, 6,
            "must stay on \"second\", not step back onto \"first\""
        );
    }

    #[test]
    fn multi_byte_text_is_never_sliced_on_a_byte_offset() {
        let mut state = normal();
        let mut text = "héllo wörld".to_string();
        let mut cursor = 0;

        // 'w' must land on the start of "wörld" -- char index 6
        // ("héllo " is six CHARS even though "é" is two bytes).
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('w'));
        assert_eq!(cursor, 6);

        // `x` on the 'é' must remove exactly that one character, never
        // panic on a byte boundary.
        let mut cursor2 = 1;
        press(&mut state, &mut text, &mut cursor2, KeyCode::Char('x'));
        assert_eq!(text, "hllo wörld");
        assert_eq!(cursor2, 1);
    }

    #[test]
    fn unrecognized_normal_mode_key_is_swallowed_not_typed() {
        let mut state = normal();
        let mut text = "hi".to_string();
        let mut cursor = 0;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('q'));

        assert_eq!(
            text, "hi",
            "an unbound NORMAL-mode letter must never insert itself as text"
        );
    }

    #[test]
    fn j_and_k_move_between_lines_clamping_the_column_onto_a_shorter_line() {
        let mut state = normal();
        let mut text = "abcdef\nxy\nzzzzzz".to_string();
        let mut cursor = 4; // col 4 ('e') on line 0

        // "xy" only has two columns (0, 1) -- NORMAL mode never rests past
        // the last char, so this clamps onto 'y' (col 1), not a col-2
        // overrun. Disclosed simplification vs. real vim: the column used
        // for the NEXT `j`/`k` is wherever the cursor actually landed here
        // (col 1), not a remembered "originally wanted column 4" -- this
        // module does not implement vim's own sticky-column memory.
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('j'));
        assert_eq!(
            cursor,
            7 + 1,
            "clamped onto the last column of the shorter line"
        );

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('j'));
        assert_eq!(
            cursor,
            10 + 1,
            "carries forward column 1 (not a remembered column 4)"
        );
    }

    #[test]
    fn gg_and_capital_g_jump_to_first_and_last_line() {
        let mut state = normal();
        let mut text = "one\ntwo\nthree".to_string();
        let mut cursor = 6;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('G'));
        assert_eq!(cursor, 8, "first non-blank of the last line (\"three\")");

        press_str(&mut state, &mut text, &mut cursor, "gg");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn wants_key_excludes_arrows_and_ordinary_ctrl_chords() {
        let state = normal();
        assert!(!wants_key(
            &state,
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)
        ));
        assert!(!wants_key(
            &state,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
        ));
        assert!(!wants_key(
            &state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ));
        assert!(!wants_key(
            &state,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL)
        ));
        assert!(wants_key(
            &state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
        ));
        assert!(wants_key(
            &state,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
        ));
        assert!(wants_key(
            &state,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        ));
    }

    // ---- Review round, board item `01M1YVJNS575YN5DCQG9BKZR4E` ----

    /// Finding 1's engine-level half: `cancel_pending` clears every
    /// in-flight NORMAL-mode command (pending operator, awaited completion
    /// key, accumulating count) and drops an open Visual selection back to
    /// NORMAL, but leaves INSERT and SEARCH completely alone -- neither is
    /// an in-flight "command" in the sense this cancels (see the method's
    /// own doc). The integration-level half (the real key router actually
    /// calling this on every non-vim path) is covered in `input`'s own
    /// test module by `ctrl_p_history_recall_cancels_a_pending_vim_operator`.
    #[test]
    fn cancel_pending_clears_in_flight_command_state_but_leaves_insert_and_search_alone() {
        let mut state = normal();
        state.pending_operator = Some((Operator::Delete, None));
        state.count = Some(4);
        state.awaiting = Awaiting::G(None);

        state.cancel_pending();

        assert!(state.pending_operator.is_none());
        assert!(state.count.is_none());
        assert!(matches!(state.awaiting, Awaiting::None));

        state.mode = VimMode::Visual {
            anchor: 0,
            linewise: false,
        };
        state.cancel_pending();
        assert_eq!(
            state.mode,
            VimMode::Normal,
            "an open Visual selection must drop too -- the text under it may have changed"
        );

        state.mode = VimMode::Insert;
        state.cancel_pending();
        assert_eq!(
            state.mode,
            VimMode::Insert,
            "INSERT is an in-progress session, not a stale command -- must never be cancelled"
        );

        state.mode = VimMode::Search {
            query: "abc".to_string(),
        };
        state.cancel_pending();
        assert_eq!(
            state.mode,
            VimMode::Search {
                query: "abc".to_string()
            },
            "a search composed so far must survive too"
        );
    }

    /// Finding 2: a count large enough to allocate gigabytes
    /// (`20000000p`) must clamp to `MAX_COUNT` copies, not the literal
    /// typed number.
    #[test]
    fn a_huge_count_before_put_is_clamped_to_the_ceiling() {
        let mut state = normal();
        let mut text = "x".to_string();
        let mut cursor = 0;

        // Delete the one char so the register holds something to put, and
        // the buffer starts empty.
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('x'));
        assert_eq!(text, "");

        press_str(&mut state, &mut text, &mut cursor, "20000000");
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('p'));

        assert_eq!(
            char_len(&text),
            MAX_COUNT as usize,
            "must clamp to MAX_COUNT copies, never the literal 20000000"
        );
    }

    /// Finding 2/3: a count large enough to be most of the way to
    /// overflowing a `u32` (`4000000000w`) must still land promptly, at
    /// the end of the text -- both because the count itself is clamped to
    /// `MAX_COUNT`, and because the `w` loop stops the instant it stops
    /// moving rather than looping the full (clamped) count regardless.
    #[test]
    fn a_huge_count_before_w_lands_at_the_end_of_text_promptly() {
        let mut state = normal();
        let mut text = "alpha beta gamma".to_string();
        let mut cursor = 0;

        press_str(&mut state, &mut text, &mut cursor, "4000000000w");

        assert_eq!(
            cursor,
            char_len(&text) - 1,
            "must land on the last character, not loop 4 billion times"
        );
    }

    /// Finding 2: digits must clamp to `MAX_COUNT` as they accumulate, so
    /// the backing `u32` can never even transiently approach overflow.
    #[test]
    fn digit_accumulation_never_overflows_and_clamps_to_the_ceiling() {
        let mut state = normal();
        let mut text = String::new();
        let mut cursor = 0;

        // 20 nines -- many times past `u32::MAX` if accumulated unclamped.
        press_str(&mut state, &mut text, &mut cursor, "99999999999999999999");

        assert_eq!(
            state.count,
            Some(MAX_COUNT),
            "digits past the ceiling must clamp on every keystroke, not just at the end"
        );
    }

    /// Finding 4: an unbound Ctrl/Alt chord must be ignored while composing
    /// a `/` search, exactly like everywhere else in this module --
    /// `wants_key`'s own half of the fix (the real key router's gate).
    #[test]
    fn wants_key_declines_an_unbound_ctrl_chord_while_composing_a_search() {
        let mut state = normal();
        state.mode = VimMode::Search {
            query: String::new(),
        };

        assert!(!wants_key(
            &state,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL)
        ));
        assert!(wants_key(
            &state,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
        ));
    }

    /// Finding 4's other half: even if a Ctrl/Alt chord somehow reached
    /// `dispatch` directly while composing a search, it must not become a
    /// literal character in the query.
    #[test]
    fn slash_search_ignores_an_unbound_ctrl_chord_while_composing() {
        let mut state = normal();
        let mut text = "find foo here".to_string();
        let mut cursor = 0;

        press(&mut state, &mut text, &mut cursor, KeyCode::Char('/'));
        press_mod(
            &mut state,
            &mut text,
            &mut cursor,
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
        );

        match &state.mode {
            VimMode::Search { query } => assert_eq!(
                query, "",
                "an unbound Ctrl chord must never be typed into the query"
            ),
            other => panic!("expected to still be composing a search, got {other:?}"),
        }
    }

    /// Finding 5: `cw` with the cursor on a non-blank changes through the
    /// END of the current word (vim's own `ce`-shaped special case),
    /// leaving the trailing whitespace untouched -- never the plain `w`
    /// span a bare motion-completion would use.
    #[test]
    fn cw_changes_through_the_end_of_the_current_word_like_ce() {
        let mut state = normal();
        let mut text = "hello world".to_string();
        let mut cursor = 0;

        press_str(&mut state, &mut text, &mut cursor, "cw");
        assert_eq!(
            text, " world",
            "cw must stop at the end of \"hello\", leaving the space"
        );
        assert_eq!(cursor, 0);
        assert_eq!(state.mode, VimMode::Insert);

        press_str(&mut state, &mut text, &mut cursor, "bye");
        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);

        assert_eq!(text, "bye world");
    }

    /// Finding 5's other half: on a blank, `cw` has no "current word" to
    /// protect the trailing gap of, so it behaves exactly like a bare `w`.
    #[test]
    fn cw_on_a_blank_behaves_like_a_bare_w() {
        let mut state = normal();
        let mut text = "a   b".to_string();
        let mut cursor = 1; // the first of three spaces after "a"

        press_str(&mut state, &mut text, &mut cursor, "cw");

        assert_eq!(text, "ab", "must delete through the start of the next word");
        assert_eq!(cursor, 1);
    }

    /// Finding 5: dot-repeat of `cw` must follow the exact same rule --
    /// this is not a separate code path, `.` simply replays the literal
    /// `c`/`w`/... keys through the same special case (see `dispatch`'s own
    /// "Dot-repeat" doc) -- stated as its own test since the finding named
    /// it explicitly.
    #[test]
    fn dot_repeats_cw_using_the_same_end_of_word_rule() {
        let mut state = normal();
        let mut text = "cat cat".to_string();
        let mut cursor = 0;

        press_str(&mut state, &mut text, &mut cursor, "cw");
        press_str(&mut state, &mut text, &mut cursor, "dog");
        press(&mut state, &mut text, &mut cursor, KeyCode::Esc);
        assert_eq!(text, "dog cat");

        press_str(&mut state, &mut text, &mut cursor, "w");
        press(&mut state, &mut text, &mut cursor, KeyCode::Char('.'));

        assert_eq!(text, "dog dog");
    }
}

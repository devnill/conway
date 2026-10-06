//! Terminal attention notifications (`[tui.attention]`): a short out-of-band
//! signal written straight to the terminal's own output stream -- never into
//! the transcript -- when something worth the operator's attention happens
//! while they are looking elsewhere. Two pieces live here:
//!
//! - **Pure byte-construction** ([`attention_bytes`]/[`emit_bytes`]), the
//!   policy gate ([`should_notify`]), and the event vocabulary's own
//!   display phrase ([`notification_text`]) -- all plain functions, tested
//!   with no terminal and no `App` at all.
//! - [`AttentionWriter`], the seam a test replaces with a `Vec<u8>` sink:
//!   production writes through [`StdoutWriter`] (the same `std::io::stdout`
//!   handle `ratatui::backend::CrosstermBackend<std::io::Stdout>` itself
//!   writes the frame through -- a SEPARATE handle to the identical
//!   destination, not a shared one, since `Backend` carries no generic
//!   accessor for its own writer: `std::io::stdout()` is cheap to open and
//!   line-buffered-safe to interleave with here, the same assumption every
//!   other direct `execute!(std::io::stdout(), ...)` call in `tui/mod.rs`
//!   already makes for the alternate-screen/bracketed-paste/focus-change
//!   control sequences).
//!
//! `crate::tui::app::attention` is the third piece: `App::maybe_notify`/
//! `App::drain_attention_queue`, which hold `AppState::attention` (the
//! config) and `self.attention_writer` (the seam) and apply the two
//! functions below to whatever `AppState::pending_attention` has queued.
//!
//! ## Why BEL, OSC 9, and OSC 777, and why bell is the default
//!
//! `BEL` (`\x07`) is the lowest common denominator: every terminal emulator
//! this project has ever had to consider renders it as *something*
//! noticeable with zero configuration, inside or outside tmux, on hardware
//! that predates OSC 9/777 entirely -- see [`TuiSection::attention`]'s own
//! doc. OSC 9 (`ESC ] 9 ; <text> BEL`) and OSC 777 (`ESC ] 777 ; notify ;
//! conway ; <text> BEL`) are richer, operator-opt-in upgrades: a real
//! desktop notification carrying the event's own short text, on the
//! terminals that implement one or the other -- see `docs/interactive.md`'s
//! own per-terminal survey.
//!
//! ## tmux: DCS passthrough, by decision
//!
//! An OSC sequence written to tmux's own pty is swallowed by tmux itself
//! unless wrapped in a DCS passthrough envelope (`ESC P tmux ; <payload with
//! every embedded ESC doubled> ESC \`) -- plain `BEL` needs no such
//! wrapping, tmux already forwards it as its own bell action. [`emit_bytes`]
//! decides this FOR the operator, detecting `$TMUX` and wrapping
//! automatically, rather than asking them to wrap it themselves: an
//! operator who already set `[tui.attention].method = "osc777"` wants the
//! notification to WORK inside tmux, not to additionally research DCS
//! passthrough first. The one thing conway genuinely cannot do for them
//! (tmux's own `allow-passthrough` option defaults to `off` as of tmux 3.3,
//! and conway has no way to set another program's config) is stated plainly
//! in `docs/interactive.md` instead.
//!
//! [`TuiSection::attention`]: crate::tui::config::TuiSection::attention

use crate::tui::config::{AttentionConfig, AttentionEvent, AttentionMethod, AttentionWhen};

/// The BEL byte, `\x07` -- shared between the bare-bell method and every
/// OSC sequence's own terminator.
const BEL: u8 = 0x07;

/// The cap on an emitted notification's own text, in `char`s -- kept short
/// by design: a terminal notification is a glance, not a transcript entry.
/// Long enough to carry a short phrase
/// (`notification_text`'s own longest value is well under this), short
/// enough that a future caller handing this a whole tool-call description
/// cannot turn one terminal control sequence into an unbounded write.
const MAX_EVENT_TEXT_CHARS: usize = 60;

/// Whether `event` should produce a real notification right now: the
/// configured method is not `off`, `event` is one of the configured
/// `events`, and `focused` clears the configured `when` gate.
///
/// `focused`: `Some(true)` -- a `CEvent::FocusGained` has been observed and
/// no `CEvent::FocusLost` since. `Some(false)` -- the reverse. **`None`
/// (no focus event ever observed -- a terminal that never sent
/// `EnableFocusChange` reporting in the first place) counts as unfocused,
/// not "unknown, skip it"**: a terminal with no focus reporting at all
/// must still get notified under the default `when = "unfocused"`, rather
/// than silently never firing because the one signal that would have
/// proven it unfocused never arrived.
pub(crate) fn should_notify(
    config: &AttentionConfig,
    event: AttentionEvent,
    focused: Option<bool>,
) -> bool {
    if config.method == AttentionMethod::Off {
        return false;
    }
    if !config.events.contains(&event) {
        return false;
    }
    match config.when {
        AttentionWhen::Always => true,
        AttentionWhen::Unfocused => focused != Some(true),
    }
}

/// The short, fixed phrase [`sanitize_event_text`] sanitizes/truncates for
/// each [`AttentionEvent`] variant. `ChildReported`/`Error` have phrases
/// here even though neither is wired to a real producer today (see
/// that enum's own doc) -- a `settings.json` naming either in `events`
/// loads cleanly and, the day a producer IS wired, has a phrase ready
/// rather than an empty string.
pub(crate) fn notification_text(event: AttentionEvent) -> &'static str {
    match event {
        AttentionEvent::TurnFinished => "conway: turn finished",
        AttentionEvent::PermissionPending => "conway: permission requested",
        AttentionEvent::ChildReported => "conway: a child agent reported",
        AttentionEvent::Error => "conway: an error occurred",
    }
}

/// Replaces every control/format character [`conway::is_laundered_char`]
/// flags (this includes `ESC`/`BEL` -- both are plain `Cc` control bytes)
/// with [`conway::sanitize_control_chars`]'s own placeholder, then
/// truncates to [`MAX_EVENT_TEXT_CHARS`] `char`s. Applied to EVERY text this
/// module ever turns into bytes, regardless of whether the caller's input
/// is one of [`notification_text`]'s own fixed phrases (which need no
/// sanitizing) or something else a future caller builds dynamically --
/// never trust-gated by the caller, so a future dynamic source cannot
/// forget the step.
pub(crate) fn sanitize_event_text(raw: &str) -> String {
    let clean = conway::sanitize_control_chars(raw);
    if clean.chars().count() <= MAX_EVENT_TEXT_CHARS {
        return clean;
    }
    let mut truncated: String = clean
        .chars()
        .take(MAX_EVENT_TEXT_CHARS.saturating_sub(1))
        .collect();
    truncated.push('…');
    truncated
}

/// Wraps `payload` (a complete OSC escape sequence) in tmux's DCS
/// passthrough envelope: `ESC P tmux ; <payload, every embedded ESC
/// doubled> ESC \`. See this module's own doc, "tmux: DCS passthrough, by
/// decision".
fn tmux_passthrough(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(b"\x1bPtmux;");
    for &byte in payload {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Builds the exact bytes `method` emits for an ALREADY-sanitized `text`
/// (callers go through [`emit_bytes`], which sanitizes first -- this
/// function is split out only so a test can assert the three methods' raw
/// shapes independently of sanitizing/tmux-wrapping). `None` for `Off`.
pub(crate) fn attention_bytes(method: AttentionMethod, text: &str) -> Option<Vec<u8>> {
    match method {
        AttentionMethod::Off => None,
        AttentionMethod::Bell => Some(vec![BEL]),
        AttentionMethod::Osc9 => Some(format!("\x1b]9;{text}\x07").into_bytes()),
        AttentionMethod::Osc777 => Some(format!("\x1b]777;notify;conway;{text}\x07").into_bytes()),
    }
}

/// The full pipeline a caller actually wants: sanitize `raw_text`, build
/// `method`'s bytes, and wrap them for tmux passthrough when `inside_tmux`
/// (never for `Bell` -- tmux forwards a bare bell on its own, no OSC
/// payload to swallow). `None` for `Off`, mirroring [`attention_bytes`].
pub(crate) fn emit_bytes(
    method: AttentionMethod,
    raw_text: &str,
    inside_tmux: bool,
) -> Option<Vec<u8>> {
    let text = sanitize_event_text(raw_text);
    let bytes = attention_bytes(method, &text)?;
    if inside_tmux && matches!(method, AttentionMethod::Osc9 | AttentionMethod::Osc777) {
        Some(tmux_passthrough(&bytes))
    } else {
        Some(bytes)
    }
}

/// The seam a test replaces: where the already-built bytes actually land.
/// `Send` because it lives on `App`, whose own future the app loop drives
/// across `.await` points. Best-effort by design -- a failed/short write to
/// a notification channel is never worth failing the whole TUI over, the
/// same posture `tui::mod::restore_terminal`'s every step already takes.
pub(crate) trait AttentionWriter: Send {
    fn write_attention(&mut self, bytes: &[u8]);
}

/// Production: a fresh `std::io::stdout()` handle per call -- see this
/// module's own doc for why a separate handle, rather than ratatui's own
/// backend writer, is the right target here.
pub(crate) struct StdoutWriter;

impl AttentionWriter for StdoutWriter {
    fn write_attention(&mut self, bytes: &[u8]) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- should_notify ----

    fn config(
        method: AttentionMethod,
        when: AttentionWhen,
        events: Vec<AttentionEvent>,
    ) -> AttentionConfig {
        AttentionConfig {
            method,
            when,
            events,
        }
    }

    #[test]
    fn off_never_notifies_regardless_of_event_or_focus() {
        let cfg = config(
            AttentionMethod::Off,
            AttentionWhen::Always,
            vec![AttentionEvent::TurnFinished],
        );
        assert!(!should_notify(
            &cfg,
            AttentionEvent::TurnFinished,
            Some(false)
        ));
        assert!(!should_notify(&cfg, AttentionEvent::TurnFinished, None));
    }

    #[test]
    fn an_event_not_in_the_configured_list_never_notifies() {
        let cfg = config(
            AttentionMethod::Bell,
            AttentionWhen::Always,
            vec![AttentionEvent::PermissionPending],
        );
        assert!(!should_notify(
            &cfg,
            AttentionEvent::TurnFinished,
            Some(false)
        ));
    }

    #[test]
    fn unfocused_gate_fires_when_not_definitively_focused() {
        let cfg = config(
            AttentionMethod::Bell,
            AttentionWhen::Unfocused,
            vec![AttentionEvent::TurnFinished],
        );
        assert!(
            !should_notify(&cfg, AttentionEvent::TurnFinished, Some(true)),
            "a definitively focused terminal must not notify under `when = unfocused`"
        );
        assert!(
            should_notify(&cfg, AttentionEvent::TurnFinished, Some(false)),
            "a definitively unfocused terminal must notify"
        );
        assert!(
            should_notify(&cfg, AttentionEvent::TurnFinished, None),
            "unknown focus (no focus event ever observed) must notify too -- a terminal with \
             no focus reporting must still get notified"
        );
    }

    #[test]
    fn always_fires_even_while_definitively_focused() {
        let cfg = config(
            AttentionMethod::Bell,
            AttentionWhen::Always,
            vec![AttentionEvent::TurnFinished],
        );
        assert!(should_notify(
            &cfg,
            AttentionEvent::TurnFinished,
            Some(true)
        ));
    }

    // ---- sanitize_event_text ----

    #[test]
    fn ordinary_text_passes_through_unchanged() {
        assert_eq!(
            sanitize_event_text("conway: turn finished"),
            "conway: turn finished"
        );
    }

    #[test]
    fn a_control_character_never_reaches_the_sanitized_output() {
        let raw = "turn finished\x1b]0;evil\x07 for real\x07";
        let sanitized = sanitize_event_text(raw);
        assert!(
            sanitized.chars().all(|c| !c.is_control()),
            "a control/escape character leaked through: {sanitized:?}"
        );
        assert!(
            sanitized.contains('\u{FFFD}'),
            "the control bytes must be replaced with evidence, not silently dropped: {sanitized:?}"
        );
    }

    #[test]
    fn long_text_is_truncated_to_the_short_cap() {
        let raw = "x".repeat(500);
        let sanitized = sanitize_event_text(&raw);
        assert!(
            sanitized.chars().count() <= MAX_EVENT_TEXT_CHARS,
            "notification text must stay short: {} chars",
            sanitized.chars().count()
        );
        assert!(sanitized.ends_with('…'));
    }

    // ---- attention_bytes / emit_bytes ----

    #[test]
    fn off_builds_no_bytes_at_all() {
        assert_eq!(attention_bytes(AttentionMethod::Off, "anything"), None);
        assert_eq!(emit_bytes(AttentionMethod::Off, "anything", false), None);
        assert_eq!(emit_bytes(AttentionMethod::Off, "anything", true), None);
    }

    #[test]
    fn bell_is_the_bare_bel_byte_and_ignores_the_text() {
        assert_eq!(
            attention_bytes(AttentionMethod::Bell, "ignored"),
            Some(vec![0x07])
        );
    }

    #[test]
    fn osc9_wraps_the_sanitized_text_in_the_documented_sequence() {
        let bytes = emit_bytes(AttentionMethod::Osc9, "turn finished", false)
            .expect("osc9 must build bytes");
        assert_eq!(bytes, b"\x1b]9;turn finished\x07".to_vec());
    }

    #[test]
    fn osc777_wraps_the_sanitized_text_in_the_documented_sequence() {
        let bytes = emit_bytes(AttentionMethod::Osc777, "bad\x1btext", false)
            .expect("osc777 must build bytes");
        assert_eq!(
            bytes,
            "\x1b]777;notify;conway;bad\u{FFFD}text\x07"
                .to_string()
                .into_bytes()
        );
    }

    #[test]
    fn bell_is_never_tmux_wrapped() {
        let inside =
            emit_bytes(AttentionMethod::Bell, "ignored", true).expect("bell must build bytes");
        let outside =
            emit_bytes(AttentionMethod::Bell, "ignored", false).expect("bell must build bytes");
        assert_eq!(inside, outside);
        assert_eq!(inside, vec![0x07]);
    }

    #[test]
    fn osc_sequences_are_dcs_passthrough_wrapped_inside_tmux_with_the_exact_bytes() {
        let wrapped = emit_bytes(AttentionMethod::Osc9, "hi", true).expect("must build");
        // Hardcoded, not re-derived from `plain` -- a self-referential
        // comparison (re-running the same doubling logic on both sides)
        // could pass even if that shared logic were wrong; this pins the
        // exact byte sequence a tmux pane actually receives: `ESC P tmux ;`
        // + the plain OSC 9 sequence with its own leading `ESC` doubled +
        // `ESC \`.
        let expected = b"\x1bPtmux;\x1b\x1b]9;hi\x07\x1b\\".to_vec();
        assert_eq!(wrapped, expected, "{wrapped:?}");

        let plain = emit_bytes(AttentionMethod::Osc9, "hi", false).expect("must build");
        assert_ne!(plain, wrapped, "tmux must change the bytes written");
    }

    // ---- notification_text ----

    #[test]
    fn every_event_has_a_short_ascii_phrase() {
        for event in [
            AttentionEvent::TurnFinished,
            AttentionEvent::PermissionPending,
            AttentionEvent::ChildReported,
            AttentionEvent::Error,
        ] {
            let text = notification_text(event);
            assert!(!text.is_empty(), "{event:?}");
            assert!(
                text.chars().count() <= MAX_EVENT_TEXT_CHARS,
                "{event:?}: {text}"
            );
        }
    }
}

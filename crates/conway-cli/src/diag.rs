//! Diagnostic output: every function here writes to `stderr` only,
//! and none takes a stdout handle. This is the mechanism that enforces
//! "stdout carries only program output" across the whole CLI -- a renderer
//! or command handler that wants to tell the user something can only reach
//! for `diag::{error,warn,info,progress}`, never a stray `println!`.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

/// Set once at startup from `--verbose`'s count; `info` consults it on
/// every call rather than threading a verbosity level through every
/// call site.
static VERBOSITY: AtomicU8 = AtomicU8::new(0);

/// Records the effective `-v`/`--verbose` count for [`info`] to consult.
pub fn set_verbosity(level: u8) {
    VERBOSITY.store(level, Ordering::Relaxed);
}

/// Unconditional stderr diagnostic for a fatal or user-facing error.
pub fn error(msg: impl AsRef<str>) {
    let _ = writeln!(std::io::stderr(), "conway: error: {}", msg.as_ref());
}

/// Unconditional stderr diagnostic for a non-fatal warning.
///
/// **Reserve this for something an operator would act on.** It is not
/// gated by `--verbose`, so anything emitted here competes for attention
/// with every other warning in the run -- and a warning that fires on
/// routine success does not merely add noise, it hides the ones that
/// matter. The whole tool-call lifecycle was emitted here once and a
/// genuine failure went unnoticed among dozens of healthy lines; see
/// `crate::render::text`'s own comment and board item
/// `01M0PSJZ18R02JJ5NHH3G6ZV9S`. Routine progress belongs in [`info`].
pub fn warn(msg: impl AsRef<str>) {
    let _ = writeln!(std::io::stderr(), "conway: warning: {}", msg.as_ref());
}

/// Stderr diagnostic suppressed unless `--verbose` was passed at least once.
///
/// The right home for routine progress -- routing decisions, the tool-call
/// lifecycle, anything a person wants when they are investigating and not
/// when they are working.
pub fn info(msg: impl AsRef<str>) {
    if VERBOSITY.load(Ordering::Relaxed) >= 1 {
        let _ = writeln!(std::io::stderr(), "conway: {}", msg.as_ref());
    }
}

/// Unconditional stderr progress notice: proves a still-running turn is
/// alive, never gated by `--verbose`.
///
/// Board item `01M1WVK9PF5G57Y6R36B94S0RB`: a brand-new user's default
/// `conway -p "<prompt>"` (`text` output mode) sat completely silent for
/// 90+ seconds against a real local backend -- no spinner, no "thinking",
/// nothing on stdout OR stderr to distinguish "still working" from "hung".
/// `--output-format jsonl`/`json` already stream real activity immediately
/// and are unaffected by this; `text` mode's own contract keeps stdout
/// carrying only the model's own reply (`crate::render::text`'s module
/// doc), so this -- like every other diagnostic -- goes to stderr instead.
///
/// Distinct from both [`warn`] (reserved for something an operator would
/// act on) and [`info`] (gated behind `--verbose`, for routine detail a
/// person only wants when investigating): this exists purely so a slow but
/// healthy run visibly proves it is alive, which is exactly the case
/// [`info`]'s gating would hide by default and [`warn`]'s "act on this"
/// framing does not fit.
pub fn progress(msg: impl AsRef<str>) {
    let _ = writeln!(std::io::stderr(), "conway: {}", msg.as_ref());
}

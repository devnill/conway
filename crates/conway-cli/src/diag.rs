//! Diagnostic output (WI-111): every function here writes to `stderr` only,
//! and none takes a stdout handle. This is the mechanism that enforces
//! "stdout carries only program output" across the whole CLI -- a renderer
//! or command handler that wants to tell the user something can only reach
//! for `diag::{error,warn,info}`, never a stray `println!`.

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
/// Not yet called: the first real caller is WI-112's render loop
/// (`Event::Lagged`/`BackendDegraded` notices).
#[allow(dead_code)]
pub fn warn(msg: impl AsRef<str>) {
    let _ = writeln!(std::io::stderr(), "conway: warning: {}", msg.as_ref());
}

/// Stderr diagnostic suppressed unless `--verbose` was passed at least once.
///
/// Not yet called: the first real caller is WI-112's `ModelDecision`
/// rendering.
#[allow(dead_code)]
pub fn info(msg: impl AsRef<str>) {
    if VERBOSITY.load(Ordering::Relaxed) >= 1 {
        let _ = writeln!(std::io::stderr(), "conway: {}", msg.as_ref());
    }
}

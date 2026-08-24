//! Process-group execution primitives shared by every consumer that spawns
//! a child process and must guarantee no orphaned grandchild survives a
//! timeout or cancellation (ONE implementation, callers call it rather than
//! restating it).
//!
//! Extracted from `crates/conway-tools/src/shell/bash.rs`'s private `unix`
//! module: `BashTool` and
//! [`crate::hook_runner::ProcessHookRunner`] both spawn via
//! `tokio::process::Command` with `process_group(0)` (the child becomes its
//! own process-group leader, so its pid doubles as the pgid every
//! termination path signals) and both need the identical
//! SIGTERM-then-SIGKILL group-kill sequence on their timeout path. This is
//! that sequence, called by both rather than restated by either.
//!
//! **Published, not private (board item `01M0EKVR1BEXXS75NV2JC4HZZ9`).**
//! This module used to be `mod process;` (private), which is why
//! `conway-plugin-subprocess` and `conway-plugin-mcp` each hand-copied
//! [`unix::kill_group`] instead of reusing it -- a first-party plugin
//! crate may not depend on `conway-tools` directly (the plugin tier gets
//! exactly the `conway` facade, nothing more privileged), so a private
//! module here left every downstream author with two choices: copy the
//! function, or breach that discipline. Every author correctly chose to
//! copy, and the count reached five call sites across three crates before
//! this item consolidated them. Now `pub`, and re-exported through
//! `conway::plugin::kill_group` (`crates/conway/src/lib.rs`, gated on this
//! crate's own `builtin-tools` feature -- see that re-export's doc for the
//! full argument for landing it on the facade rather than leaving it
//! duplicated or publishing this crate directly).
//!
//! **The five-way diff, and the one behavioral difference it found.** All
//! five copies used the identical `TERM_GRACE = Duration::from_secs(2)`
//! and the identical SIGTERM-then-wait-then-SIGKILL-then-wait shape. They
//! differed in exactly one place: this crate's original returned
//! `Option<ExitStatus>` via `match tokio::time::timeout(..).await { Ok(Ok(status))
//! => Some(status), _ => { ..SIGKILL.. } }` -- so ANY non-success outcome
//! within the grace period (a timeout elapsing, OR `child.wait()` itself
//! returning an `Err`) falls through to the SIGKILL fallback. The two
//! plugin crates' copies instead wrote `if timeout(..).await.is_err() {
//! ..SIGKILL.. }`, which only checks whether the OUTER timeout elapsed --
//! an inner `Ok(Err(_))` from `child.wait()` (the wait syscall itself
//! failing, not the child exiting) would silently skip the SIGKILL
//! fallback in those two copies. This is this crate's own
//! `kill_group`, kept as the specification: it is strictly more
//! defensive (an extra guaranteed SIGKILL is harmless -- `kill`/`wait` on
//! an already-reaped or already-dead pid just returns an error that is
//! already discarded) and it hands the caller the exit status the two
//! plugin copies' callers never needed but this crate's own `shell::bash`
//! does (to report the timed-out command's own exit code). The
//! consolidated function below is this signature, unchanged.

// Board item `01M0TV7ZDS8X4F4TEJPRZB9P6T` adds a second, generic
// consolidation alongside `unix::kill_group` above: the shared
// child-process SESSION lifecycle (spawn + id-correlated NDJSON round trip
// + per-call timeout + fail-closed teardown) `conway-plugin-mcp` and
// `conway-plugin-subprocess` each hand-rolled independently. `unix` (above)
// stays untouched -- this is a sibling module, not a change to the
// five-way-diff module this doc block itself documents. See
// `child_session`'s own module doc for the full argument and its
// `cfg(unix)` gate (matching `unix`'s own: the generic session calls
// `unix::kill_group` directly).
#[cfg(unix)]
pub mod child_session;

#[cfg(unix)]
pub mod unix {
    use std::process::ExitStatus;

    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    use tokio::process::Child;
    use tokio::time::Duration;

    /// Grace period between SIGTERM and SIGKILL when killing a group.
    pub const TERM_GRACE: Duration = Duration::from_secs(2);

    /// SIGTERM the whole process group `pgid`, give it [`TERM_GRACE`] to
    /// exit, then SIGKILL and wait again. Always reaps `child` (never
    /// leaves a zombie).
    ///
    /// `pgid` is positive (the group leader's own pid); this signals the
    /// GROUP by negating it (`kill(-pgid, ..)`, the POSIX convention), never
    /// the single leader process alone -- the whole point of having spawned
    /// with `process_group(0)` in the first place.
    pub async fn kill_group(child: &mut Child, pgid: i32) -> Option<ExitStatus> {
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);
        match tokio::time::timeout(TERM_GRACE, child.wait()).await {
            Ok(Ok(status)) => Some(status),
            _ => {
                let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
                child.wait().await.ok()
            }
        }
    }
}

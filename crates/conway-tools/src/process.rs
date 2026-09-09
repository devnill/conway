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

// Board item `01M1X2ZCCZEW322YCMGW57K75D` adds a third shared spawn concern
// alongside `unix::kill_group` (above) and `child_session` (the generic
// session lifecycle): the OS-level race where a freshly-written executable
// is momentarily busy when `Command::spawn()` tries to exec it. CI hit this
// intermittently as ETXTBSY on a subprocess-plugin script written to a
// `TempDir` microseconds before spawn. `unix` (above) stays untouched -- this
// is a sibling module, not a change to the five-way-diff module this doc
// block documents. See `spawn_retry`'s own module doc for the full argument
// and its `cfg(unix)` gate (matching `unix`'s own: both real call sites that
// consume it are unix-only -- `child_session::spawn` and
// `conway-plugin-subprocess`'s `spawn_one_shot` are both `#[cfg(unix)]`).
#[cfg(unix)]
pub mod spawn_retry;

#[cfg(unix)]
pub mod unix {
    use std::process::ExitStatus;

    use nix::errno::Errno;
    use nix::sys::signal::{kill, Signal};
    use nix::unistd::Pid;
    use tokio::process::Child;
    use tokio::time::{Duration, Instant};

    /// Grace period between SIGTERM and SIGKILL when killing a group.
    pub const TERM_GRACE: Duration = Duration::from_secs(2);

    /// How often the grace window re-checks whether any group member is
    /// still alive. Short relative to [`TERM_GRACE`] (so escalation fires
    /// close to the deadline, not up to a whole interval late) and long
    /// enough not to spin the loop needlessly while waiting out a group
    /// that is genuinely still running. Matches the polling interval this
    /// crate's own integration tests already use to observe group death
    /// from the outside (`tests/shell_bash.rs`'s and
    /// `tests/hook_runner.rs`'s `assert_group_dead`).
    const POLL_INTERVAL: Duration = Duration::from_millis(20);

    /// SIGTERM the whole process group `pgid`, give it [`TERM_GRACE`] to
    /// exit, then SIGKILL and wait again. Always reaps `child` (never
    /// leaves a zombie).
    ///
    /// `pgid` is positive (the group leader's own pid); this signals the
    /// GROUP by negating it (`kill(-pgid, ..)`, the POSIX convention), never
    /// the single leader process alone -- the whole point of having spawned
    /// with `process_group(0)` in the first place.
    ///
    /// **Why the grace window watches the GROUP, not the leader.** An
    /// earlier version of this function timed the grace window against
    /// `child.wait()` alone -- i.e. against the leader's own exit. That is
    /// insufficient, and not merely in theory: under a non-interactive
    /// `bash -c` (job control off), a trailing `cmd &` does NOT give the
    /// backgrounded process its own process group -- the shell forks it,
    /// returns, and exits, leaving a long-lived grandchild sharing the
    /// leader's group. SIGTERM above targets that whole group, but the
    /// leader -- having nothing left to do -- exits within milliseconds,
    /// almost always well inside `TERM_GRACE`. Watching only `child.wait()`
    /// then resolves on that leader's exit and returns immediately,
    /// **before the grace window has actually elapsed and before anything
    /// has confirmed the rest of the group is gone** -- so a backgrounded
    /// process that ignores SIGTERM (a `trap '' TERM` handler is the
    /// direct way to do that deliberately; plenty of ordinary programs do
    /// it incidentally) is never escalated to SIGKILL and survives
    /// teardown entirely. The escalation used to fire reliably only in the
    /// case that needs it least: a leader that itself outlives the grace
    /// window. This function instead polls the GROUP itself (`kill(-pgid,
    /// 0)`, signal 0 -- reports whether anything is still addressable at
    /// that pgid without signalling it) until it is empty or the grace
    /// window elapses, independent of whether the leader in particular has
    /// already exited.
    ///
    /// **Edge cases, decided rather than assumed:**
    /// - **A group that exits cleanly mid-grace is never SIGKILLed
    ///   afterward.** The poll loop reaps the leader opportunistically
    ///   (non-blocking `try_wait`, not a blocking `wait`) the moment it
    ///   exits, so a leader that has already exited does not itself keep
    ///   registering as a live group member via the zombie state below --
    ///   the group-emptiness check reflects reality, not a stale reap. The
    ///   instant the group check reports empty, the loop stops; SIGKILL is
    ///   only ever sent on the branch where the deadline is reached with
    ///   the group check still reporting a survivor.
    /// - **A zombie group member does not make this hang, and does not
    ///   fool the emptiness check into declaring victory early.** A
    ///   process that has exited but not yet been reaped by ITS OWN
    ///   parent (e.g. a backgrounded grandchild, reparented after the
    ///   original leader exits, briefly zombied under its new parent)
    ///   still has a live process-table entry at that pgid, so `kill(-pgid,
    ///   0)` correctly keeps reporting the group non-empty until it is
    ///   actually reaped -- this function never calls `wait` on anything
    ///   but its own direct child (the leader), so it cannot block
    ///   reaping something it does not own; it is bounded by `TERM_GRACE`
    ///   regardless, exactly as an uninterruptible-sleep (`D`-state) member
    ///   is: `kill()` never blocks on the signal's *effect*, only submits
    ///   it, so a `D`-state process cannot stall this loop -- it just may
    ///   still be alive (and briefly re-poll as such) after SIGKILL is
    ///   sent, since delivery to a process stuck in the kernel is deferred
    ///   until it returns to user space. That one detail is not new here:
    ///   the original implementation's own post-SIGKILL `child.wait()`
    ///   could already block on an uninterruptible LEADER exactly the same
    ///   way; this function does not change that, since returning a real
    ///   `ExitStatus` for the leader requires actually reaping it.
    /// - **`EPERM` from `kill(-pgid, 0)` is treated identically to
    ///   `ESRCH`: group empty, stop polling.** Every process in this group
    ///   descends from a child this function's own caller spawned, so
    ///   while the group is genuinely still ours, signalling it can never
    ///   return `EPERM` -- only `ESRCH` (a `pgid` reused by an unrelated,
    ///   NON-owned process, differing effective UID) can, and this crate's
    ///   own integration tests already document that exact race under
    ///   `cargo test`'s pid churn (`tests/shell_bash.rs`'s
    ///   `assert_group_dead`): "still proof our group is dead -- had it
    ///   still been alive, we own it and `kill` would return `Ok`." A
    ///   surviving member of OUR group is never a reason to see `EPERM`;
    ///   seeing it means the pgid no longer resolves to anything of ours,
    ///   so there is nothing left here to wait out or escalate against.
    pub async fn kill_group(child: &mut Child, pgid: i32) -> Option<ExitStatus> {
        let _ = kill(Pid::from_raw(-pgid), Signal::SIGTERM);

        let deadline = Instant::now() + TERM_GRACE;
        let mut status: Option<ExitStatus> = None;

        loop {
            // Non-blocking: reap the leader the moment it exits so its
            // `ExitStatus` is captured, independent of how much longer the
            // rest of the group takes to follow it down. Safe to keep
            // calling after the leader is already reaped (`Ok(None)`
            // forever after).
            if status.is_none() {
                if let Ok(Some(exit)) = child.try_wait() {
                    status = Some(exit);
                }
            }

            if !group_has_members(pgid) {
                break;
            }

            if Instant::now() >= deadline {
                let _ = kill(Pid::from_raw(-pgid), Signal::SIGKILL);
                break;
            }

            tokio::time::sleep(POLL_INTERVAL).await;
        }

        match status {
            Some(exit) => Some(exit),
            // The leader outlived the whole grace window (and has just
            // been SIGKILLed above, alongside the rest of the group) --
            // block until it actually exits so its real `ExitStatus` is
            // still returned, matching this function's signature exactly
            // as before.
            None => child.wait().await.ok(),
        }
    }

    /// `kill(-pgid, 0)` -- signal 0, which reports whether anything is
    /// still addressable at this pgid WITHOUT signalling it. See
    /// [`kill_group`]'s own doc for how each outcome (including `EPERM`)
    /// is interpreted and why.
    fn group_has_members(pgid: i32) -> bool {
        match kill(Pid::from_raw(-pgid), None) {
            Ok(()) => true,
            Err(Errno::ESRCH) => false,
            Err(Errno::EPERM) => false,
            Err(_) => false,
        }
    }
}

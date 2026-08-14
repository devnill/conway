//! Process-group execution primitives shared by every consumer in this
//! crate that spawns a child process and must guarantee no orphaned
//! grandchild survives a timeout or cancellation (ONE implementation,
//! callers call it rather than restating it).
//!
//! Extracted from `crates/conway-tools/src/shell/bash.rs`'s private `unix`
//! module: `BashTool` and
//! [`crate::hook_runner::ProcessHookRunner`] both spawn via
//! `tokio::process::Command` with `process_group(0)` (the child becomes its
//! own process-group leader, so its pid doubles as the pgid every
//! termination path signals) and both need the identical
//! SIGTERM-then-SIGKILL group-kill sequence on their timeout path. This is
//! that sequence, called by both rather than restated by either.

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

//! Minimal OS-process signal/liveness helpers for the dogfood gates 3-4
//! integration tests (`dogfood_background_and_children.rs`).
//!
//! **Not** part of `tests/common`'s own shared module tree -- `common/
//! mod.rs` is out of this wave's file fence (a sibling agent may need it
//! too), so this file cannot be wired in via a `pub mod child_procs;` line
//! there. Each consuming test file instead pulls it in directly with
//! `#[path = "common/child_procs.rs"] mod child_procs;`, which only needs
//! this FILE to exist at that path on disk -- not `mod.rs`'s cooperation.
//! It lives under `tests/common/` anyway (rather than beside the test
//! files themselves) because it is genuinely shared harness surface for
//! this wave's two dogfood files, matching the spirit of every other
//! `tests/common/*` module even though it is not reachable through
//! `common::` itself.
//!
//! Shells out to the real `kill` utility rather than adding a `libc`/
//! `nix` dependency this crate does not otherwise carry -- the same "no
//! second dependency for a one-off primitive" discipline `common/pty.rs`'s
//! own doc states for itself (it made the identical call for
//! `portable-pty` vs. hand-rolling).

#![allow(dead_code)]

use std::process::Command;
use std::time::{Duration, Instant};

/// How often the poll loops below re-check their condition. Not a
/// synchronization primitive on its own -- exactly `common::pty::
/// PtySession`'s own `POLL_INTERVAL` doc: the timeout a caller passes is
/// what makes a stuck wait fail loudly rather than hang.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Whether a process named `pid` is currently alive, via `kill -0` (sends
/// no actual signal; only checks permission + existence). A pid that has
/// already been reaped, or was never valid, answers `false`.
pub fn is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Sends `signal` (bare name, e.g. `"TERM"`, `"KILL"`) to `pid`. Never
/// panics on failure -- the process may already be gone, which is a
/// perfectly ordinary outcome for [`KillOnDrop`]'s own teardown use. A
/// caller that needs to KNOW the signal actually took effect follows up
/// with [`wait_until_dead`], which fails loudly on its own timeout
/// instead.
pub fn send_signal(pid: u32, signal: &str) {
    let _ = Command::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .status();
}

/// Polls [`is_alive`] until it answers `false`, or panics naming `pid`
/// once `timeout` elapses -- the same "loud, never a silent hang" contract
/// `common::pty::PtySession::wait_for_any` has, applied to process death
/// instead of a text pattern. This poll loop IS the wait-with-timeout that
/// replaces a fixed `sleep` here; nothing calling this should also sleep a
/// fixed amount first.
pub fn wait_until_dead(pid: u32, timeout: Duration) {
    let start = Instant::now();
    while is_alive(pid) {
        if start.elapsed() > timeout {
            panic!(
                "timed out after {timeout:?} waiting for pid {pid} to exit (still alive per \
                 `kill -0`)"
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Generic poll-with-timeout: blocks until `predicate` answers `true`, or
/// panics naming `what` once `timeout` elapses. For a caller whose
/// condition is not simply "is this pid alive" -- e.g. "has the mock
/// backend received the inner `conway` process's first request yet".
pub fn wait_until<F: FnMut() -> bool>(mut predicate: F, timeout: Duration, what: &str) {
    let start = Instant::now();
    while !predicate() {
        if start.elapsed() > timeout {
            panic!("timed out after {timeout:?} waiting for: {what}");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Extracts the pid `echo $!` printed as a backgrounded bash call's ENTIRE
/// captured stdout -- the first non-empty line following the `stdout:`
/// section header `crates/conway-tools/src/shell/bash.rs`'s own result
/// formatter writes (`docs/tools.md`'s "What `&` does inside `bash`"
/// section shows the exact shape: `stdout:\n<pid>\n\nstderr:\n...`).
pub fn pid_from_bash_stdout(tool_result_text: &str) -> u32 {
    let (_, after) = tool_result_text.split_once("stdout:\n").unwrap_or_else(|| {
        panic!("no 'stdout:' section in bash tool result: {tool_result_text:?}")
    });
    let first_line = after.lines().next().unwrap_or("").trim();
    first_line.parse().unwrap_or_else(|e| {
        panic!(
            "expected the backgrounded pid alone on stdout, got {first_line:?}: {e}\nfull \
             result: {tool_result_text:?}"
        )
    })
}

/// RAII guard: SIGKILLs a tracked pid on drop, unconditionally -- the
/// backgrounded-grandchild analogue of `common::pty::PtySession`'s own
/// `Drop` impl (kill on drop, including under an unwinding panic, so a
/// failed assertion never leaks a real OS process past the end of its own
/// test). `Drop` still runs during an unwind under this workspace's
/// default `panic = unwind` test profile, which is what makes this
/// reachable at all -- the identical guarantee `pty.rs`'s own doc states
/// for itself. A pid that already exited by the time this drops is a
/// silent no-op ([`send_signal`] never panics).
pub struct KillOnDrop(pub u32);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        send_signal(self.0, "KILL");
    }
}

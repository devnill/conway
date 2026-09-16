//! Spawn-with-retry for the OS-level ETXTBSY race (board item
//! `01M1X2ZCCZEW322YCMGW57K75D`): a thin wrapper around
//! `tokio::process::Command::spawn()` that tolerates
//! `std::io::ErrorKind::ExecutableFileBusy` -- the error a kernel returns
//! when it is asked to `execve` a file that is still being written to.
//!
//! **Why this belongs in `conway-tools`, once.** `conway-plugin-subprocess`
//! writes a subprocess-plugin script to a `TempDir` and then spawns it; on
//! a loaded CI host the write can land microseconds before `spawn()` tries
//! to `exec` it, and the kernel momentarily reports the file as busy
//! (`ETXTBSY`). This is not a bug in either side -- it is a TOCTOU race
//! between two syscalls that share no lock -- and it is intermittent in the
//! exact way CI flakes are: reproducible locally never, reproducible on the
//! runner once in a thousand runs. The fix is to retry the spawn a small
//! bounded number of times, sleeping briefly between attempts, because the
//! race is measured in milliseconds. That logic is identical at every call
//! site that spawns a freshly-written executable, so it lives here as a
//! single shared helper alongside [`super::unix::kill_group`] (the
//! process-group teardown) and [`super::child_session`] (the generic session
//! lifecycle), and is re-exported through `conway::plugin` the same way
//! those are (see `crates/conway/src/lib.rs`'s re-export doc for why a
//! first-party plugin crate may not depend on `conway-tools` directly and
//! so reaches this through the facade).
//!
//! **What this is NOT.** It is not a general spawn-retry: only
//! [`std::io::ErrorKind::ExecutableFileBusy`] is retried. Any other error
//! kind (`NotFound`, `PermissionDenied`, a bad argv, ...) returns on the
//! FIRST attempt with no added latency and no behavior change -- those are
//! deterministic failures a retry would only hide or stall. It adds no new
//! dependency (it sleeps via `tokio::time::sleep`, already transitively
//! present at every call site) and it does not touch the error type or text
//! its callers surface: every call site maps the final error through the
//! shared `E::spawn(config_id, detail)` constructor. That was once true only
//! of the `ChildSession`-mediated path -- `spawn_one_shot` built the variant
//! by hand -- and is now true of both.
//!
//! **Testability.** The helper accepts an injectable spawn attempt (a
//! closure `FnMut() -> Result<Child, std::io::Error>`) so the retry/backoff
//! DECISION LOGIC can be tested without reproducing a real OS-level
//! `ETXTBSY` race -- the unit tests below feed a fake that returns
//! `ExecutableFileBusy` for its first N calls and then succeeds, and a fake
//! that returns a different `ErrorKind` on the first call, asserting on the
//! returned value AND the call count. The real call sites pass a closure
//! that calls `cmd.spawn()`.

use std::future::Future;
use std::io;

use tokio::process::Child;
use tokio::time::{sleep, Duration};

/// Maximum number of spawn attempts before giving up. The ETXTBSY race is
/// measured in milliseconds, so a handful of attempts with a short sleep
/// between each is more than enough to outlast it on any reasonable host.
pub const MAX_SPAWN_ATTEMPTS: u8 = 8;

/// Sleep between retry attempts. Kept small: the race is a few
/// milliseconds, and every retry adds this much latency to a spawn that, in
/// the common case, succeeds on the first attempt and never sleeps at all.
pub const RETRY_SLEEP: Duration = Duration::from_millis(10);

/// Spawn a child process, retrying only `ExecutableFileBusy`.
///
/// `spawn_attempt` is the closure that actually performs one
/// `Command::spawn()` call (and is the thing the unit tests replace with a
/// fake). It is called up to [`MAX_SPAWN_ATTEMPTS`] times: if it returns
/// `Ok(child)` that child is returned immediately; if it returns an `Err`
/// whose `kind()` is [`io::ErrorKind::ExecutableFileBusy`], the helper
/// sleeps [`RETRY_SLEEP`] and tries again; if it returns ANY OTHER error
/// kind, that error is returned immediately on the first attempt -- no
/// added latency, no behavior change. If every attempt is exhausted on
/// `ExecutableFileBusy`, the LAST error is returned.
///
/// The `Stdio` argument is unused by the retry logic itself; it exists so
/// the real call sites can build their `Command` (with `stdin`/`stdout`/
/// `stderr` piped and `process_group(0)`) and pass a closure that captures
/// it, keeping the helper's signature focused on the spawn call rather than
/// on command construction. It is not required by the helper and may be
/// omitted by callers that construct their command differently.
pub async fn spawn_with_retry<F>(spawn_attempt: F) -> io::Result<Child>
where
    F: FnMut() -> io::Result<Child>,
{
    spawn_with_retry_configurable(spawn_attempt, MAX_SPAWN_ATTEMPTS, RETRY_SLEEP).await
}

/// The same retry decision logic as [`spawn_with_retry`], with the attempt
/// count and inter-attempt sleep exposed as parameters. This is the seam the
/// unit tests drive (so they can use a tiny attempt count and a zero sleep
/// rather than the production defaults) and the one [`spawn_with_retry`]
/// delegates to.
pub async fn spawn_with_retry_configurable<F>(
    mut spawn_attempt: F,
    max_attempts: u8,
    retry_sleep: Duration,
) -> io::Result<Child>
where
    F: FnMut() -> io::Result<Child>,
{
    let mut last_err: Option<io::Error> = None;
    for attempt in 0..max_attempts {
        match spawn_attempt() {
            Ok(child) => return Ok(child),
            Err(err) if err.kind() == io::ErrorKind::ExecutableFileBusy => {
                last_err = Some(err);
                // Don't sleep after the final attempt -- the loop ends and we
                // return the last error, so a trailing sleep would only stall.
                if attempt + 1 < max_attempts {
                    sleep(retry_sleep).await;
                }
            }
            Err(err) => return Err(err),
        }
    }
    // Every attempt returned ExecutableFileBusy: surface the last one.
    Err(last_err.expect("max_attempts > 0 guarantees at least one attempt"))
}

// A no-op assertion that the helper's public return type is the Future
// shape the call sites expect, so a signature drift here fails the build
// rather than silently breaking a call site.
const _: fn() = || {
    fn _assert_future<F>(_: F)
    where
        F: Future<Output = io::Result<Child>>,
    {
    }
    _assert_future(spawn_with_retry(|| {
        Err(io::Error::from(io::ErrorKind::ExecutableFileBusy))
    }));
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;
    use std::sync::atomic::{AtomicU8, Ordering};

    /// A fake spawn attempt that records how many times it was called and
    /// returns `ExecutableFileBusy` for its first `fail_n` calls, then
    /// succeeds. The "success" value is a real `Child` spawned from
    /// `std::env::current_exe()` (the running test binary, guaranteed to
    /// exist), so the returned `Ok` is a genuine `tokio::process::Child`
    /// the assertion can inspect -- the test asserts on the actual returned
    /// value, not a stand-in.
    ///
    /// **The self-replication hazard this guards against (board item
    /// `01M2MVFM7GD0XYJ7G7DN08A46P`).** `current_exe()` IS this crate's own
    /// unit-test binary (`target/debug/deps/conway_tools-<hash>` -- the one
    /// Cargo builds for the lib's `#[cfg(test)]` modules, distinct from the
    /// per-file integration binaries named after their `tests/*.rs` file).
    /// Spawning it with NO arguments re-invokes the default test harness,
    /// which runs every `#[test]` in this binary -- including this very
    /// test, which spawns ANOTHER unfiltered copy, which spawns another...
    /// a self-replicating process chain with no termination condition, each
    /// generation reparented to PID 1 once its immediate parent exits. That
    /// is exactly what was observed live: a rotating set of
    /// `conway_tools-<hash>` processes, PIDs changing between consecutive
    /// `ps` calls, one's PPID pointing at another `conway_tools` process,
    /// present even after a clean `exit 0` workspace run. [`ARGV_NO_MATCH`]
    /// below is the fix: the re-exec'd harness is given a filter substring
    /// that matches no test name in this binary, so it reports "0 tests"
    /// and exits within milliseconds instead of recursing -- a real
    /// termination condition rather than none. [`KillOnDrop`] is the second,
    /// independent layer: even a near-instant child is a live process for a
    /// few milliseconds, and dropping a `tokio::process::Child` (like
    /// `std::process::Child`) never signals it -- only an explicit kill
    /// does. Mirrors this repo's own precedent for the identical shape:
    /// `crates/conway-cli/tests/common/pty.rs`'s `impl Drop for
    /// PtySession`, which kills its child on drop specifically so a panic
    /// between spawn and test-end still cannot leak it.
    struct FakeSpawn {
        calls: AtomicU8,
        fail_n: u8,
    }

    /// A filter argument guaranteed to match no `#[test]` name in this
    /// binary, so a `current_exe()` re-exec started with it as its sole
    /// argv runs the standard cargo-test harness CLI, matches zero tests,
    /// and exits immediately -- rather than defaulting to "run everything"
    /// the way a bare, argument-less re-exec of the harness would. See
    /// [`FakeSpawn`]'s own doc for why an argument-less re-exec is the bug.
    const ARGV_NO_MATCH: &str = "__spawn_retry_fixture_guard_no_such_test__";

    /// Kills the spawned re-exec'd test binary on drop. `Drop::drop` cannot
    /// `.await`, so this uses `start_kill` (fire-and-forget SIGKILL) rather
    /// than the async `kill` -- sufficient here because the child is either
    /// already exited (the common case, given [`ARGV_NO_MATCH`]) or about
    /// to be, and this test binary's own process exiting shortly after
    /// reaps whatever is left. Runs on a panic too: this workspace's test
    /// profile is `panic = unwind` (see the `pty.rs` precedent this
    /// mirrors), so `Drop` still fires during an unwind.
    struct KillOnDrop(Child);

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.start_kill();
        }
    }

    impl FakeSpawn {
        fn new(fail_n: u8) -> Self {
            Self {
                calls: AtomicU8::new(0),
                fail_n,
            }
        }

        fn call_count(&self) -> u8 {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl FakeSpawn {
        fn attempt(&self) -> io::Result<Child> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_n {
                return Err(io::Error::from(io::ErrorKind::ExecutableFileBusy));
            }
            // A real child so the returned Ok is a genuine Child value.
            // `current_exe()` is guaranteed to exist (it IS the running
            // test binary), unlike `/bin/true` which may not be present
            // on every host or sandbox the test suite runs in. `ARGV_NO_MATCH`
            // is load-bearing, not decorative -- see `FakeSpawn`'s own doc.
            tokio::process::Command::new(std::env::current_exe().unwrap())
                .arg(ARGV_NO_MATCH)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
    }

    #[tokio::test]
    async fn retries_executable_file_busy_then_succeeds() {
        // Fail the first 3 calls, succeed on the 4th.
        let fake = FakeSpawn::new(3);
        let calls_ref = &fake;

        let result = spawn_with_retry_configurable(
            || calls_ref.attempt(),
            MAX_SPAWN_ATTEMPTS,
            Duration::from_millis(0),
        )
        .await;

        assert!(
            result.is_ok(),
            "should succeed after retries: {:?}",
            result.err()
        );
        // `KillOnDrop`, not a bare binding: this is a real re-exec'd child
        // process (see `FakeSpawn`'s own doc), and dropping a
        // `tokio::process::Child` never signals it. The guard fires on the
        // ordinary path below AND on a panic from the `assert_eq!` that
        // follows, which a plain cleanup call placed after it would miss.
        let _child = KillOnDrop(result.unwrap());
        // 3 failures + 1 success = 4 calls total.
        assert_eq!(
            fake.call_count(),
            4,
            "must have retried exactly 3 times then succeeded"
        );
    }

    #[tokio::test]
    async fn returns_other_error_immediately() {
        // A fake that returns NotFound -- a non-retried kind -- on the very
        // first call. The helper must surface it immediately with exactly one
        // call made.
        let calls = AtomicU8::new(0);
        let calls_ref = &calls;
        let result = spawn_with_retry_configurable(
            || {
                calls_ref.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::from(io::ErrorKind::NotFound))
            },
            MAX_SPAWN_ATTEMPTS,
            Duration::from_millis(0),
        )
        .await;

        let err = result.expect_err("NotFound must be returned, not retried");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "must not retry a non-ExecutableFileBusy error"
        );
    }

    #[tokio::test]
    async fn returns_last_error_when_all_attempts_busy() {
        // Always returns ExecutableFileBusy; with max_attempts=3, the helper
        // must make exactly 3 calls and return the last error.
        let calls = AtomicU8::new(0);
        let calls_ref = &calls;
        let result = spawn_with_retry_configurable(
            || {
                calls_ref.fetch_add(1, Ordering::SeqCst);
                Err(io::Error::from(io::ErrorKind::ExecutableFileBusy))
            },
            3,
            Duration::from_millis(0),
        )
        .await;

        let err = result.expect_err("exhausted retries must surface the last error");
        assert_eq!(err.kind(), io::ErrorKind::ExecutableFileBusy);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            3,
            "must make exactly max_attempts calls"
        );
    }
}

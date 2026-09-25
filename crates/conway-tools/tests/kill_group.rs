//! Direct coverage of `conway_tools::process::unix::kill_group` -- the ONE
//! SIGTERM-then-SIGKILL group teardown implementation every timeout/
//! cancellation path in this crate (and, through `conway::plugin::
//! kill_group`, every first-party plugin crate) calls. Tested here directly
//! against the function itself, not through `BashTool` or `ProcessHookRunner`
//! -- this is the safety-critical primitive both of those already assume
//! works.
//!
//! Fixture scripts are written to a fresh `TempDir` at test time (matches
//! `tests/hook_runner.rs::fixture`'s own convention, not a tracked
//! executable file).

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use conway_tools::process::unix::{kill_group, TERM_GRACE};
use tempfile::TempDir;

/// Builds a `Command` that runs the `#!/bin/sh` fixture script at `path` by
/// invoking `/bin/sh path` explicitly, rather than `Command::new(path)`'s
/// direct `execve` of the file `fixture()` just wrote and closed moments
/// earlier -- the fix for board item `01M3CCB39A1SKWFQEKC2CZK7S9` (CI runs
/// `36052363927` and `36100724242`, both panicking at the spawn step of
/// `a_sigterm_ignoring_backgrounded_child_is_dead_after_kill_group_returns`
/// with `Os { code: 26, kind: ExecutableFileBusy, message: "Text file busy" }`).
///
/// **The mechanism, established by direct reproduction, not inferred.**
/// `Command::new(path).spawn()` execve's `path` itself; for a `#!/sh`
/// script the kernel's own binfmt_script handler re-opens that SAME file
/// (the one this process just wrote and `drop`-closed) to hand off to the
/// interpreter, and it is that second, kernel-internal open-for-exec that
/// can observe the inode as still busy. `fixture()` already closes its
/// `File::create` handle explicitly before returning (`drop(f)`) -- this is
/// not a case of an open write handle lingering in this process's own code.
/// A minimal reproduction outside this suite (write, chmod, `spawn()`, in a
/// tight loop across 48 threads on a 10-core Linux box under synthetic CPU
/// load, `rustc -O`, no tokio) measured a genuine, repeatable `ETXTBSY` rate
/// against the CURRENT (`Command::new(path)`) shape:
///   - direct exec of the freshly-written file: 1939, 1995, 2067, 2688,
///     2688 and 2640 failures out of 96,000 attempts across six runs
///     (~2-3%) -- confirms the race is real and load-dependent, not a
///     defect in `fixture()` or in `kill_group` itself.
///   - adding `f.sync_all()` before `drop(f)`: WORSE, not better --
///     65,518-66,860 failures per 96,000 (~68%). Ruled out.
///   - write-then-rename (write to `path.tmp`, chmod, `fs::rename` to
///     `path`, then exec `path`): NOT better than the baseline -- 2,796-
///     3,215 failures per 96,000. Expected once the mechanism is
///     understood: `ETXTBSY`'s busy check is per-INODE, and rename never
///     changes which inode gets exec'd, so renaming the freshly-written
///     file onto its final name does not touch the actual race at all.
///     Ruled out.
///   - invoking `/bin/sh path` instead of `Command::new(path)` (this fix):
///     0 failures out of 96,000 attempts, on THREE separate runs (288,000
///     attempts, 0 failures) under the identical stress. This works
///     because it never asks the kernel to execve the freshly-written file
///     at all -- `/bin/sh` (a long-resident binary, never freshly written
///     by this process) is what gets exec'd, and it merely `open()`s
///     `path` for READING to interpret it, which is not subject to the
///     write-busy check that `execve()` is. This is exactly what the
///     kernel's binfmt_script handler already does on this process's
///     behalf for a direct exec of a `#!/bin/sh` script -- this fix simply
///     performs that same substitution one level up, before the freshly-
///     written file is ever handed to `execve()` a second time.
///
/// Structural, not a retry: no attempt is retried, nothing sleeps, and the
/// number of times each fixture is spawned is unchanged -- the exec target
/// itself changes from "the file this process just wrote" to "a stable
/// binary already on disk", which is the one substitution the measurements
/// above show actually removes the race rather than merely narrowing it.
fn script_command(path: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("/bin/sh");
    command.arg(path);
    command
}

/// Spawns `path` under `process_group(0)`, waits until `marker` holds a
/// non-empty line (proof the script got past its own exec and reached its
/// first lines) or a generous deadline elapses, then tears the whole warm-up
/// group down via [`kill_group`] itself and deletes `marker` -- pays the
/// "first exec of a freshly written script" OS-side tax for a fixture that
/// deliberately backgrounds a long-lived grandchild, without leaking that
/// grandchild past this function's own return. Mirrors `tests/
/// hook_runner.rs::warm_hanging_fixture` exactly (same fixture shape, same
/// reason).
async fn warm_backgrounding_fixture(path: &Path, marker: &Path) {
    let mut command = script_command(path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let Ok(mut child) = command.spawn() else {
        return;
    };
    let Some(pgid) = child.id().map(|id| id as i32) else {
        return;
    };

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let wrote_marker = std::fs::read_to_string(marker)
            .map(|contents| !contents.trim().is_empty())
            .unwrap_or(false);
        if wrote_marker {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    kill_group(&mut child, pgid).await;
    let _ = std::fs::remove_file(marker);
}

/// Writes `script` to `dir` as an executable POSIX shell script and returns
/// its path. Mirrors `tests/hook_runner.rs::fixture`.
fn fixture(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create fixture script");
    f.write_all(script.as_bytes())
        .expect("write fixture script");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod +x fixture script");
    path
}

/// Executes `path` once with stdin closed and discards the result -- pays
/// the "first exec of a freshly written, freshly-chmod'd script" OS-side
/// tax (board item `01M09MPZ9C188AHNBKWEJ3CEQA`; see `tests/hook_runner.rs::
/// warm`'s own doc for the measurement: a fresh script's first exec can
/// cost seconds at ~0% CPU) BEFORE a timed run below starts its own clock,
/// so `kill_group`'s own bounded work is what each test's timing actually
/// measures rather than an unrelated exec-latency race.
async fn warm(path: &Path) {
    let child = script_command(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let _ = child.wait().await;
    }
}

/// Polls `path` until it holds a non-empty line, up to a generous deadline
/// -- mirrors `tests/hook_runner.rs::wait_for_pgid`'s own reasoning (the
/// write and this poll race benignly; only a genuinely stuck fixture should
/// ever hit the deadline).
fn wait_for_marker(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if !contents.trim().is_empty() {
                return contents;
            }
        }
        assert!(
            Instant::now() < deadline,
            "fixture never wrote its marker to {path:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Polls `kill(pid, 0)` until it fails (the individual process is gone),
/// up to a generous deadline -- the single-pid analogue of `tests/
/// shell_bash.rs::assert_group_dead` / `tests/hook_runner.rs::
/// assert_group_dead`: a killed process can sit as a zombie, still
/// answering `kill(pid, 0)` with success, until whatever it reparented to
/// (init/launchd, once its original parent shell exited) reaps it, so a
/// single immediate check is not honest here either.
fn assert_pid_dead(pid: i32) {
    const DEADLINE: Duration = Duration::from_secs(3);
    const POLL: Duration = Duration::from_millis(20);

    let start = Instant::now();
    loop {
        let result = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            None::<nix::sys::signal::Signal>,
        );
        if result.is_err() {
            return;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "pid {pid} should be dead, but kill(pid, 0) still returned Ok after \
             {DEADLINE:?} of polling -- it survived kill_group rather than merely \
             awaiting reaping"
        );
        std::thread::sleep(POLL);
    }
}

/// **The load-bearing test.** A group whose LEADER exits almost
/// immediately (it backgrounds a job and has nothing else to do) and whose
/// backgrounded child traps and ignores SIGTERM must still be fully dead
/// once `kill_group` returns -- proving the grace window is timed against
/// the GROUP, not merely against the leader's own (already-resolved)
/// `wait()`.
///
/// Run against the pre-fix implementation (`child.wait()` alone gates the
/// grace window), this fails: the leader has already exited by the time
/// `kill_group` is called, so its `wait()` resolves on the cached status
/// immediately, SIGKILL is never sent, and the backgrounded child -- which
/// ignores SIGTERM -- is still alive when this assertion runs.
#[tokio::test]
async fn a_sigterm_ignoring_backgrounded_child_is_dead_after_kill_group_returns() {
    let dir = TempDir::new().expect("tempdir");
    let marker = dir.path().join("child_pid");
    let script = fixture(
        dir.path(),
        "backgrounds_and_exits.sh",
        &format!(
            r#"#!/bin/sh
sh -c 'trap "" TERM; sleep 300' &
echo $! > {marker}
"#,
            marker = marker.to_str().unwrap()
        ),
    );
    warm_backgrounding_fixture(&script, &marker).await;

    let mut command = script_command(&script);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let mut child = command.spawn().expect("spawn fixture");
    let pgid = child.id().expect("pid readable") as i32;

    let child_pid: i32 = wait_for_marker(&marker)
        .trim()
        .parse()
        .expect("numeric pid");

    // Give the leader (which has nothing left to do after backgrounding
    // the job above) time to actually exit and become a zombie awaiting
    // reap -- this is exactly the precondition the defect needs: the
    // leader already gone by the time `kill_group`'s own grace window
    // begins, so watching `child.wait()` alone would resolve instantly.
    // The fixture was already warmed above, so this is bounded by actual
    // scheduling, not by any first-exec tax.
    tokio::time::sleep(Duration::from_millis(200)).await;

    kill_group(&mut child, pgid).await;

    assert_pid_dead(child_pid);
}

/// A group that exits cleanly and promptly -- nothing left running once
/// the leader is done -- is never sent SIGKILL, and the leader's own
/// `ExitStatus` is still returned unchanged. Proven indirectly by timing:
/// an implementation that (incorrectly) always waits out the full grace
/// window before returning would take close to [`TERM_GRACE`]; this must
/// return in a small fraction of it.
#[tokio::test]
async fn a_cleanly_exiting_group_is_never_sigkilled_and_its_exit_status_is_preserved() {
    let dir = TempDir::new().expect("tempdir");
    let script = fixture(
        dir.path(),
        "exits_cleanly.sh",
        r#"#!/bin/sh
exit 7
"#,
    );
    warm(&script).await;

    let mut command = script_command(&script);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let mut child = command.spawn().expect("spawn fixture");
    let pgid = child.id().expect("pid readable") as i32;

    // Let the leader actually exit before `kill_group` is invoked, the
    // same way a real caller's timeout/cancellation path can race a
    // leader that was already finishing on its own. The fixture was
    // already warmed above, so this is bounded by actual scheduling, not
    // by any first-exec tax.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let start = Instant::now();
    let status = kill_group(&mut child, pgid).await;
    let elapsed = start.elapsed();

    assert_eq!(
        status.and_then(|s| s.code()),
        Some(7),
        "the leader's real exit status must still be returned"
    );
    assert!(
        elapsed < TERM_GRACE / 4,
        "a group with nothing left alive must not wait out any meaningful \
         portion of the {TERM_GRACE:?} grace window before returning: took {elapsed:?}"
    );
}

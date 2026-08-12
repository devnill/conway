//! Integration coverage for [`ProcessHookRunner`] (board item
//! 01KZRZY1MNM872BZ6AKEBG3SKE): one library-level end-to-end pass driving a
//! single made-up event against a fixture script, plus the fail-closed
//! guarantees the runner is the one place responsible for -- a nonexistent
//! command, a nonzero exit, unparseable stdout, and a hang past the
//! deadline that must not leave a backgrounded grandchild behind.
//!
//! No `ToolCtx`/`PermissionGate` involvement anywhere here: this suite
//! exercises runner mechanics only (spawn, stdin, stdout, exit status),
//! per the owning item's own "demonstrable scope" note.
//!
//! Fixture scripts are written to a fresh `TempDir` at test time (POSIX
//! `sh`, `chmod 0o755`) rather than committed as tracked executable files
//! -- this crate's tests have no existing convention for the latter (see
//! `tests/fs_core.rs`'s `set_permissions` use for the sibling pattern this
//! mirrors).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use conway_core::error::HookFailure;
use conway_core::hook::{ContextDelta, HookAnswer, HookEvent, HookInvocation};
use conway_core::ports::HookRunner;
use conway_tools::hook_runner::ProcessHookRunner;
use tempfile::TempDir;

/// Writes `script` to `dir` as an executable POSIX shell script and returns
/// its path.
fn fixture(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn invocation(command: Vec<String>, timeout_ms: u64, payload: serde_json::Value) -> HookInvocation {
    HookInvocation {
        command,
        timeout_ms,
        event: HookEvent {
            name: "pre_tool_use".into(),
            payload,
        },
    }
}

/// Asserts `kill(-pgid, 0)` can no longer reach the group -- POLLED, not
/// checked once, because group teardown after `kill_group` is asynchronous
/// (the direct child is reaped, but a backgrounded grandchild reparents to
/// init and is briefly a zombie, which still answers `kill(pid, 0)` with
/// success). Mirrors `tests/shell_bash.rs`'s own `assert_group_dead`
/// exactly -- same hazard, same fix, both callers of the same extracted
/// `kill_group`.
fn assert_group_dead(pgid: i32) {
    const DEADLINE: Duration = Duration::from_secs(2);
    const POLL: Duration = Duration::from_millis(20);

    let start = std::time::Instant::now();
    loop {
        let result = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(-pgid),
            None::<nix::sys::signal::Signal>,
        );
        if result.is_err() {
            return;
        }
        assert!(
            start.elapsed() < DEADLINE,
            "process group {pgid} should be dead, but kill(-pgid, 0) still returned Ok after \
             {DEADLINE:?} of polling -- the group survived the timeout path rather than merely \
             awaiting reaping"
        );
        std::thread::sleep(POLL);
    }
}

// --------------------------------------------------------- end to end ---

/// The demonstrable-scope test: ONE made-up event driven all the way
/// through -- spawn, write the event as JSON to stdin, read the answer from
/// stdout. The fixture only emits the populated `HookAnswer` when it
/// actually observed the exact payload text on its stdin (a `case`
/// pattern-match on a marker string the test alone controls), so a
/// correctly-shaped default `{}` answer coming back would mean the payload
/// never arrived -- the assertion is on the RETURNED ANSWER's content, not
/// on any intermediate signal like "a process spawned" (P-15).
#[tokio::test]
async fn drives_one_event_end_to_end_and_returns_the_scripts_answer() {
    let dir = TempDir::new().unwrap();
    let script = fixture(
        dir.path(),
        "echo_answer.sh",
        r#"#!/bin/sh
input=$(cat)
case "$input" in
  *marker-8f2c1a*)
    printf '{"context":{"appends":[{"note":"seen"}],"excludes":["seg-1"]}}'
    ;;
  *)
    printf '{}'
    ;;
esac
"#,
    );

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!({"marker": "marker-8f2c1a"}),
    );

    let answer = runner.run(&invocation).await.expect("hook should succeed");
    assert_eq!(
        answer,
        HookAnswer {
            context: ContextDelta {
                appends: vec![serde_json::json!({"note": "seen"})],
                excludes: vec!["seg-1".to_string()],
            },
        }
    );
}

/// Empty stdout on a clean exit is a valid, deliberately minimal answer,
/// not a parse failure.
#[tokio::test]
async fn empty_stdout_on_success_is_the_default_answer() {
    let dir = TempDir::new().unwrap();
    let script = fixture(dir.path(), "silent.sh", "#!/bin/sh\nexit 0\n");

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!(null),
    );

    let answer = runner.run(&invocation).await.expect("hook should succeed");
    assert_eq!(answer, HookAnswer::default());
}

// ------------------------------------------------------- fail-closed ---

#[tokio::test]
async fn nonexistent_command_fails_closed_not_a_panic() {
    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec!["/definitely/does/not/exist/conway-hook-fixture".to_string()],
        5_000,
        serde_json::json!(null),
    );

    let err = runner
        .run(&invocation)
        .await
        .expect_err("a nonexistent command must fail, never silently succeed");
    assert!(
        matches!(err, HookFailure::Spawn { .. }),
        "expected Spawn, got {err:?}"
    );
}

#[tokio::test]
async fn nonzero_exit_fails_closed() {
    let dir = TempDir::new().unwrap();
    let script = fixture(dir.path(), "fails.sh", "#!/bin/sh\nexit 7\n");

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!(null),
    );

    let err = runner
        .run(&invocation)
        .await
        .expect_err("a nonzero exit must fail, never be read as success");
    assert_eq!(err, HookFailure::NonzeroExit { code: Some(7) });
}

#[tokio::test]
async fn unparseable_stdout_fails_closed_even_on_a_clean_exit() {
    let dir = TempDir::new().unwrap();
    let script = fixture(
        dir.path(),
        "garbage.sh",
        "#!/bin/sh\nprintf 'not json at all'\nexit 0\n",
    );

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!(null),
    );

    let err = runner
        .run(&invocation)
        .await
        .expect_err("malformed stdout must fail even though the process exited 0");
    assert!(
        matches!(err, HookFailure::UnparseableAnswer { .. }),
        "expected UnparseableAnswer, got {err:?}"
    );
}

// --------------------------------------------- timeout / process group ---

/// Serializes the two heavy fixtures below (each forks repeatedly inside a
/// `while true` loop) so they never race each other for CPU: observed
/// flaky under `cargo test`'s default parallelism when both ran at once --
/// the fixture's own `echo $$ > file` line, which runs before anything
/// hangs, was sometimes still not visible even after several seconds of
/// polling, purely from scheduling contention between the two forking
/// loops. Running one at a time removes that contention; it changes
/// nothing about what either test proves.
static PROCESS_GROUP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A hook that traps SIGTERM and would otherwise run forever is killed
/// (SIGTERM, then SIGKILL after the grace period) and reported as a timeout
/// -- within BOUNDED time, proven by wrapping the whole call in an outer
/// `tokio::time::timeout` shorter than "forever."
#[tokio::test]
async fn hang_trapping_sigterm_is_killed_and_reported_as_timed_out() {
    let _guard = PROCESS_GROUP_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let pgid_file = dir.path().join("pgid");
    let script = fixture(
        dir.path(),
        "hangs.sh",
        &format!(
            r#"#!/bin/sh
trap '' TERM
echo $$ > {pgid_file}
while true; do sleep 1; done
"#,
            pgid_file = pgid_file.to_str().unwrap()
        ),
    );

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        2_000,
        serde_json::json!(null),
    );

    let result = tokio::time::timeout(Duration::from_secs(20), runner.run(&invocation))
        .await
        .expect("the runner must return within 20s even though the script traps SIGTERM");

    assert_eq!(
        result,
        Err(HookFailure::TimedOut { after_ms: 2_000 }),
        "got {result:?}"
    );

    let pgid: i32 = wait_for_pgid(&pgid_file).trim().parse().unwrap();
    assert_group_dead(pgid);
}

/// A hook that backgrounds a grandchild before its own timeout fires does
/// not leave that grandchild running once the timeout path completes --
/// `kill_group` signals the whole process group (every member shares the
/// pgid `process_group(0)` assigned), not merely the direct child.
#[tokio::test]
async fn backgrounded_grandchild_does_not_survive_the_timeout_path() {
    let _guard = PROCESS_GROUP_LOCK.lock().await;
    let dir = TempDir::new().unwrap();
    let pgid_file = dir.path().join("pgid");
    let script = fixture(
        dir.path(),
        "backgrounds.sh",
        &format!(
            r#"#!/bin/sh
trap '' TERM
echo $$ > {pgid_file}
sleep 300 &
while true; do sleep 1; done
"#,
            pgid_file = pgid_file.to_str().unwrap()
        ),
    );

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        2_000,
        serde_json::json!(null),
    );

    let result = tokio::time::timeout(Duration::from_secs(20), runner.run(&invocation))
        .await
        .expect("the runner must return within 20s");
    assert_eq!(result, Err(HookFailure::TimedOut { after_ms: 2_000 }));

    // The backgrounded `sleep 300 &` inherited the same pgid as the script
    // itself (process_group(0) makes the script the group leader; a plain
    // `&` background job does not call setsid, so it stays in the group).
    let pgid: i32 = wait_for_pgid(&pgid_file).trim().parse().unwrap();
    assert_group_dead(pgid);
}

/// The pgid file is written by the fixture BEFORE it starts hanging, but
/// this test's own process only starts polling for it after spawning --
/// poll rather than assume it is already there the instant `run` returns
/// (the file write and the runner's own return race benignly; both settle
/// well inside this poll's budget).
fn wait_for_pgid(path: &Path) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if !contents.trim().is_empty() {
                return contents;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "fixture never wrote its pgid to {path:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

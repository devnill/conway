//! Integration coverage for [`ProcessHookRunner`] -- one library-level end-to-end pass driving a
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
use conway_tools::process::unix::kill_group;
use tempfile::TempDir;

/// Writes `script` to `dir` as an executable POSIX shell script and returns
/// its path.
fn fixture(dir: &Path, name: &str, script: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Executes `path` once, with stdin closed, and discards everything about
/// the result -- before returning, so a timed run against the SAME file
/// immediately after pays the "first exec of a freshly written,
/// freshly-chmod'd script" OS-side tax (board item `01M09MPZ9C188AHNBKWEJ3CEQA`;
/// see `warm_hanging_fixture`'s own doc below for the measurement) OUTSIDE
/// the clock, not inside it.
///
/// This simpler helper, not `warm_hanging_fixture`, is the right one for
/// every fixture below that actually exits on its own (a `cat`-then-`case`
/// or a plain `exit N`, none of which trap SIGTERM or background a
/// grandchild): waiting for exit is safe and sufficient here, unlike the
/// two hang fixtures `warm_hanging_fixture` exists for.
async fn warm(path: &Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        // Outcome deliberately discarded: warming only cares that the exec
        // happened (and whatever OS-side check gates it completed), not
        // what the script did with no input.
        let _ = child.wait().await;
    }
}

/// Executes `path` once and force-kills it as soon as `marker` is written
/// (or a generous deadline elapses), before the REAL, timed run below spawns
/// the SAME file through [`ProcessHookRunner`]'s own bounded `timeout_ms`
/// clock -- pays the "first exec of a freshly written, freshly-chmod'd
/// script" OS-side tax (board item `01M09MPZ9C188AHNBKWEJ3CEQA`; see
/// `conway-plugin-subprocess`'s `tests/common/mod.rs::warm` for the full
/// measurement: one run caught 23.5s wall clock at ~0% CPU for a brand-new
/// script's first exec, with later execs of the SAME file costing tens of
/// milliseconds) here, discarded, so `run`'s own `timeout_ms` deadline below
/// measures the mechanism it says it measures, not an OS-dependent
/// first-exec cost.
///
/// Confirmed as a live race in THIS crate, not inherited from the sibling
/// crate's report: with `hang_trapping_sigterm_is_killed_and_reported_as_
/// timed_out`'s 2000ms `timeout_ms` and no warm-up, injecting a few hundred
/// freshly written/exec'd scripts' worth of concurrent churn from an
/// unrelated process reproduced the exact panic `wait_for_pgid` below
/// guards against ("fixture never wrote its pgid") on every attempt -- the
/// runner's own kill fired before the fixture ever reached its `echo $$`
/// line. The same churn against an ALREADY-WARMED copy of the identical
/// fixture measured ~70ms to `echo $$`, confirming the tax is keyed to the
/// specific FILE (consistent with the `com.apple.provenance` extended
/// attribute a fresh file carries), not shared/system-wide contention --
/// so warming each fixture, not raising `timeout_ms`, is the fix.
///
/// This is intentionally NOT a call to `conway-plugin-subprocess`'s own
/// `common::warm`: that helper lives in a different crate's private
/// `tests/` module and cannot be imported here, and more fundamentally its
/// contract does not fit these fixtures anyway. That helper waits for the
/// warmed process to EXIT; these two fixtures deliberately never exit on
/// their own (`while true; do sleep 1; done` past a trapped SIGTERM), so
/// waiting for exit would turn warming into the hang itself. Instead this
/// waits only until `marker` is written -- proof the script got past its
/// own exec and reached its own first lines -- or a generous deadline
/// elapses, then tears down the warm-up child via [`kill_group`] (the SAME
/// `process_group(0)` + SIGTERM-then-SIGKILL primitive `ProcessHookRunner`
/// itself uses on the real, timed path below) and reaps it. Using
/// `kill_group` here, not a bare `Child::kill`, matters for
/// `backgrounded_grandchild_does_not_survive_the_timeout_path`'s own
/// fixture specifically: it backgrounds a `sleep 300 &` right after
/// writing `marker`, so a warm-up that only killed the direct child could
/// race that background job's own spawn and leak a five-minute `sleep`
/// orphan; signaling the whole group tears down whatever the script has
/// spawned by the time `marker` appears, backgrounded job included.
/// `marker` is deleted afterward so the REAL run below starts from a clean
/// file rather than a stale pid this warm-up child already wrote: a
/// stale-but-present pgid would let `assert_group_dead` succeed instantly
/// against an already-dead warm-up process, proving nothing about the REAL
/// invocation this test means to time.
async fn warm_hanging_fixture(path: &Path, marker: &Path) {
    let mut command = tokio::process::Command::new(path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let Ok(mut child) = command.spawn() else {
        // The real, timed run immediately after will hit (and report) the
        // same spawn failure -- not this helper's problem to report.
        return;
    };
    let Some(pgid) = child.id().map(|id| id as i32) else {
        // Already exited before its pid could be read -- nothing left to
        // warm or tear down.
        return;
    };

    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while tokio::time::Instant::now() < deadline {
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

/// Runs `invocation`, retrying a bounded number of times if the spawn loses a
/// race that this test binary creates for itself.
///
/// `fixture` writes an executable script and a test then execs it. That is
/// safe in isolation -- `std::fs::write` closes its handle before returning,
/// and Rust opens with `O_CLOEXEC`. It is NOT safe under `cargo test`'s
/// parallelism: while thread A holds a write fd to the script for an instant,
/// thread B may `fork` for its own spawn, and `fork` duplicates every open fd
/// into the child. Until that child reaches its `exec`, the script's inode is
/// open-for-write in another process, and exec'ing it returns `ETXTBSY`
/// ("Text file busy"). `O_CLOEXEC` narrows that window; it cannot close it.
/// Same race as golang/go#22315.
///
/// THE RETRY IS DELIBERATELY HERE AND NOT IN `ProcessHookRunner`. An operator
/// whose hook script is genuinely being rewritten as conway spawns it should
/// get exactly what the runner returns today: a fail-closed `Spawn` error. It
/// is not conway's job to paper over a script changing underneath it, and
/// adding a retry to a security-adjacent path to fix a test problem would be
/// the wrong trade. The ETXTBSY seen here is manufactured by this binary's own
/// threading and is not the behaviour any of these tests are about.
///
/// Bounded, and it panics loudly when exhausted rather than looping until the
/// harness times out.
async fn run_retrying_spawn_race(
    runner: &ProcessHookRunner,
    invocation: &HookInvocation,
) -> Result<HookAnswer, HookFailure> {
    const ATTEMPTS: u32 = 10;
    for attempt in 0..ATTEMPTS {
        let result = runner.run(invocation).await;
        match &result {
            Err(HookFailure::Spawn { detail }) if detail.contains("Text file busy") => {
                tokio::time::sleep(Duration::from_millis(20 * u64::from(attempt + 1))).await;
            }
            _ => return result,
        }
    }
    panic!(
        "spawn lost the ETXTBSY race {ATTEMPTS} times in a row for {:?}; that is no longer a \
         race, investigate rather than raising the bound",
        invocation.command
    );
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
/// on any intermediate signal like "a process spawned".
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
    warm(&script).await;

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!({"marker": "marker-8f2c1a"}),
    );

    let answer = run_retrying_spawn_race(&runner, &invocation)
        .await
        .expect("hook should succeed");
    assert_eq!(
        answer,
        HookAnswer {
            context: ContextDelta {
                appends: vec![serde_json::json!({"note": "seen"})],
                excludes: vec!["seg-1".to_string()],
            },
            ..HookAnswer::default()
        }
    );
}

/// Empty stdout on a clean exit is a valid, deliberately minimal answer,
/// not a parse failure.
#[tokio::test]
async fn empty_stdout_on_success_is_the_default_answer() {
    let dir = TempDir::new().unwrap();
    let script = fixture(dir.path(), "silent.sh", "#!/bin/sh\nexit 0\n");
    warm(&script).await;

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!(null),
    );

    let answer = run_retrying_spawn_race(&runner, &invocation)
        .await
        .expect("hook should succeed");
    assert_eq!(answer, HookAnswer::default());
}

/// Multi-thread-flavor coverage for `01M03FNRGWNMMRKXBJKCEE14QJ`.
///
/// **What this item found, and what it could not confirm.** `unix::drive`
/// used to run the stdin write, both output drains, and `child.wait()` in
/// a single `tokio::join!`. `conway-plugin-subprocess` -- a sibling crate
/// that reused this exact shape for a new subprocess plugin host -- hit a
/// hang while bisecting its own implementation and fixed it by draining
/// all three pipes concurrently, THEN reaping `child.wait()` sequentially
/// (see that crate's `spawn_one_shot`, whose own comment documents its
/// bisection). `unix::drive` here was changed to the identical shape on
/// that report.
///
/// Every existing test of this runner used plain `#[tokio::test]`
/// (current-thread), so none of them ever exercised the multi-thread
/// flavor `conway-cli`'s own `#[tokio::main]` actually runs under -- this
/// is the only test in the suite that opts into `flavor = "multi_thread"`
/// for exactly that reason, closing a real coverage gap regardless of
/// what follows.
///
/// What this item's own investigation could NOT do: reproduce a hang, or
/// even a measurable, reproducible latency difference between the old
/// four-way join and the fixed sequential-wait shape, against the tokio
/// version this workspace has pinned (`1.53.1`). A controlled A/B (both
/// shapes, matched contention, same harness) was run against real
/// hardware -- native Apple Silicon macOS, native aarch64 Linux under
/// cgroup CPU throttling -- and separately under x86_64 Linux via QEMU
/// emulation; none produced a consistent, reproducible direction across
/// repeated rounds. The completion report for this item has the full
/// experimental detail. This test therefore asserts CORRECTNESS under the
/// multi-thread flavor (the property that must hold either way), not a
/// specific hang -- an honest test that reproduces a hang must show one,
/// and this one could not.
#[tokio::test(flavor = "multi_thread")]
async fn drives_one_event_end_to_end_under_a_multi_thread_runtime() {
    let dir = TempDir::new().unwrap();
    let script = fixture(
        dir.path(),
        "echo_answer_mt.sh",
        r#"#!/bin/sh
input=$(cat)
case "$input" in
  *marker-mt-9d3b*)
    printf '{"context":{"appends":[{"note":"seen"}],"excludes":["seg-1"]}}'
    ;;
  *)
    printf '{}'
    ;;
esac
"#,
    );
    warm(&script).await;

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        5_000,
        serde_json::json!({"marker": "marker-mt-9d3b"}),
    );

    // Bounded per this item's own hard rule: a test that could reproduce a
    // hang must report it as a failure, never wedge the harness.
    let answer = tokio::time::timeout(
        Duration::from_secs(15),
        run_retrying_spawn_race(&runner, &invocation),
    )
    .await
    .expect(
        "ProcessHookRunner did not return within 15s under a multi-thread runtime \
         (01M03FNRGWNMMRKXBJKCEE14QJ)",
    )
    .expect("hook should succeed");
    assert_eq!(
        answer,
        HookAnswer {
            context: ContextDelta {
                appends: vec![serde_json::json!({"note": "seen"})],
                excludes: vec!["seg-1".to_string()],
            },
            ..HookAnswer::default()
        }
    );
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
    warm(&script).await;

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
    warm(&script).await;

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
/// polling. At the time this was attributed to scheduling contention
/// between the two forking loops; `warm_hanging_fixture` below (board item
/// `01M0HXD6CKDZGVZP29FKKBQQ6S`) names the more precise root cause -- a
/// freshly-written, freshly-chmod'd script's first exec, not steady-state
/// scheduling -- but this lock still removes one real source of CPU
/// contention between the two, so it stays; it changes nothing about what
/// either test proves.
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
    warm_hanging_fixture(&script, &pgid_file).await;

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        2_000,
        serde_json::json!(null),
    );

    let result = tokio::time::timeout(
        Duration::from_secs(20),
        run_retrying_spawn_race(&runner, &invocation),
    )
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
    warm_hanging_fixture(&script, &pgid_file).await;

    let runner = ProcessHookRunner::new();
    let invocation = invocation(
        vec![script.to_str().unwrap().to_string()],
        2_000,
        serde_json::json!(null),
    );

    let result = tokio::time::timeout(
        Duration::from_secs(20),
        run_retrying_spawn_race(&runner, &invocation),
    )
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
///
/// This 10s deadline used to be racing `ProcessHookRunner`'s own 2000ms
/// `timeout_ms` for the SAME fixture's first exec (board item
/// `01M0HXD6CKDZGVZP29FKKBQQ6S`): a freshly-written, freshly-chmod'd
/// script's first exec can cost seconds at ~0% CPU (measured directly
/// against these exact fixtures under injected churn: a worst case of
/// ~8s to reach `echo $$`), so under load the runner could kill the
/// process before it ever wrote its pgid, and this loop would then also
/// exhaust its own budget and panic below -- a wrong-reason failure, not
/// a real assertion failure about group teardown. Both callers now call
/// `warm_hanging_fixture` on the SAME script before the timed `run`, which
/// pays that tax outside `timeout_ms`'s clock; with warming in place this
/// loop only has to cover an already-warm process actually reaching its
/// own second line, which is near-instant, so 10s stays a correctness
/// backstop, not a budget this loop is expected to need.
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

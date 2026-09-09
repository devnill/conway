//! Coverage for `ChildSession`'s bounded grace-before-kill and first-call
//! warm-up budget (board item `01M1YQ3MJQSCQTMVAZ3GCSTB8P`) -- the shared
//! machinery every MCP-over-stdio and subprocess-plugin round trip goes
//! through, tested here directly against `conway_tools::process::
//! child_session::ChildSession` (the ONE implementation, per P-14) rather
//! than through either consumer crate's own wire dialect.
//!
//! Each fixture "server" is a plain POSIX `sh` script this suite writes into
//! a fresh temp dir at run time (matching `tests/hook_runner.rs`'s own
//! convention, not a tracked executable file): it reads NDJSON request
//! lines of the trivial shape `{"id":N}` from stdin, counts how many it has
//! received, sleeps a configurable amount depending on whether the current
//! line is its FIRST or a LATER one, then (unless told never to answer)
//! replies `{"id":N,"count":C}` -- `count` is what lets a test assert on
//! the server's own received-request count (P-15: acceptance 1 requires
//! this, not merely that the call succeeded, since only the count can tell
//! "waited longer" apart from "silently resent").
//!
//! WARN capture: `tracing-test` is not a dependency of this crate, so this
//! suite implements the identical minimal `tracing::Subscriber` (using only
//! the `tracing` crate, already a direct dependency) `crates/conway-session/
//! tests/recovery_tests.rs` already established for this exact purpose,
//! installed via `tracing::subscriber::set_default` for the test's
//! duration. Relies on the default `#[tokio::test]` current-thread runtime
//! keeping the whole test on one OS thread, since `set_default` is
//! thread-local -- true here because the grace/warn logic under test runs
//! on the CALLING task (`ChildSession::await_response`), not the session's
//! own background reader task.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use conway_tools::process::child_session::{ChildSession, ChildSessionError, NotificationRoute};
use tempfile::TempDir;

// ---------------------------------------------------------------------
// A minimal `ChildSessionError` impl -- this suite's own stand-in for
// `McpPluginError`/`SubprocessPluginError`, needed only to instantiate
// `ChildSession<E>` directly. Not itself under test: `ChildSession` is
// generic over `E`, and both real consumers implement this trait as a
// one-line-per-variant translation (see `child_session.rs`'s own module
// doc) -- this is that same shape, minimized.
// ---------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum TestError {
    Spawn(String),
    TimedOut { after_ms: u64 },
    SessionDied(String),
    MalformedFrame(String),
}

impl ChildSessionError for TestError {
    fn spawn(_config_id: &str, detail: String) -> Self {
        TestError::Spawn(detail)
    }
    fn timed_out(_config_id: &str, after_ms: u64) -> Self {
        TestError::TimedOut { after_ms }
    }
    fn session_died(_config_id: &str, detail: String) -> Self {
        TestError::SessionDied(detail)
    }
    fn malformed_frame(_config_id: &str, detail: String) -> Self {
        TestError::MalformedFrame(detail)
    }
}

// ---------------------------------------------------------------------
// Minimal tracing WARN capture (no tracing-subscriber dependency) --
// verbatim shape of `crates/conway-session/tests/recovery_tests.rs`'s own
// `CaptureLog`/`CaptureSubscriber`/`install_capture`, not restated from
// scratch: same three pieces, same trait impl, so a reader who has seen
// one has seen both.
// ---------------------------------------------------------------------

#[derive(Clone, Default)]
struct CaptureLog {
    entries: Arc<Mutex<Vec<String>>>,
}

impl CaptureLog {
    fn contains(&self, needle: &str) -> bool {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.contains(needle))
    }
}

struct CaptureSubscriber {
    log: CaptureLog,
}

struct MessageVisitor(String);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for CaptureSubscriber {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.log.entries.lock().unwrap().push(visitor.0);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

fn install_capture() -> (CaptureLog, tracing::subscriber::DefaultGuard) {
    let log = CaptureLog::default();
    let guard = tracing::subscriber::set_default(CaptureSubscriber { log: log.clone() });
    // `tracing`'s per-callsite `Interest` cache is a GLOBAL, once-per-process
    // decision: whichever thread first executes a given `tracing::warn!`
    // invocation site caches whether ANYONE is interested, and every later
    // invocation of that SAME site -- from any thread, under any later
    // `set_default` -- reuses that cached answer rather than asking again.
    // This suite's own tests run in parallel (the default `cargo test`
    // posture) and more than one reaches the ONE `tracing::warn!` call site
    // in `ChildSession::await_response`; without this call, whichever test's
    // thread got there first "wins" the cache and a sibling test racing it
    // can see zero captured events even though the warning genuinely fired
    // in production code (confirmed by reproducing the flake, deterministically,
    // under `--test-threads=2`). Forces every callsite to re-ask the NOW-current
    // (this thread's, just-installed) subscriber instead of trusting a stale
    // global cache.
    tracing::callsite::rebuild_interest_cache();
    (log, guard)
}

// ---------------------------------------------------------------------
// Fixture: a stdin-driven NDJSON echo server, configurable via env vars.
// ---------------------------------------------------------------------

/// A POSIX `sh` fixture that reads `{"id":N}` lines from stdin, replying
/// `{"id":N,"count":C}` where `C` is the 1-based count of requests this
/// process has seen so far -- the observable that lets a test assert on the
/// server's own RECEIVED-REQUEST COUNT (P-15), not merely on whether a call
/// succeeded. Reads `FAKE_FIRST_DELAY_S` (seconds to sleep before answering
/// request #1) and `FAKE_OTHER_DELAY_S` (seconds to sleep before answering
/// every later request), each defaulting to `0`; if `FAKE_NEVER_ANSWER` is
/// set to any non-empty value, every request is counted but NEVER answered
/// (the genuinely-hung-server fixture). Exits cleanly on stdin EOF (closed
/// stdin, as `warm` below gives it, is an immediate EOF -- the loop simply
/// never runs and the script exits 0).
const FAKE_SERVER: &str = r#"#!/bin/sh
count=0
while IFS= read -r line; do
  count=$((count + 1))
  if [ "$count" -eq 1 ]; then
    delay="${FAKE_FIRST_DELAY_S:-0}"
  else
    delay="${FAKE_OTHER_DELAY_S:-0}"
  fi
  if [ "$delay" != "0" ]; then
    sleep "$delay"
  fi
  if [ -n "$FAKE_NEVER_ANSWER" ]; then
    continue
  fi
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  printf '{"id":%s,"count":%s}\n' "$id" "$count"
done
"#;

/// Writes `contents` to `<dir>/<name>`, marks it executable, and returns the
/// argv this test hands to [`ChildSession::spawn`]'s own `command` --
/// mirrors `tests/hook_runner.rs::fixture` verbatim.
fn fixture(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create fixture script");
    f.write_all(FAKE_SERVER.as_bytes())
        .expect("write fixture script");
    drop(f);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod +x fixture script");
    path
}

/// Executes `path` once with stdin closed (an immediate EOF for
/// [`FAKE_SERVER`]'s own read loop, so this exits promptly) and discards the
/// result -- pays the "first exec of a freshly written, freshly-chmod'd
/// script" OS-side tax (board item `01M09MPZ9C188AHNBKWEJ3CEQA`; see
/// `tests/hook_runner.rs::warm`'s own doc for the measurement) BEFORE a
/// timed [`ChildSession::spawn`] below ever starts its own clock.
async fn warm(path: &Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let _ = child.wait().await;
    }
}

/// Spawns a warmed [`FAKE_SERVER`] under `env`, with `timeout_ms`/
/// `first_call_timeout_ms` as given. `NotificationRoute::WarnAndDrop`: this
/// suite's fixture never writes a no-`id` line, so the choice is arbitrary
/// (matching `conway-plugin-mcp`'s own, simpler consumer).
async fn spawn_fake(
    dir: &Path,
    name: &str,
    env: &[(String, String)],
    timeout_ms: u64,
    first_call_timeout_ms: u64,
) -> ChildSession<TestError> {
    let path = fixture(dir, name);
    warm(&path).await;
    ChildSession::spawn(
        "test-fixture",
        &[path.display().to_string()],
        env,
        timeout_ms,
        first_call_timeout_ms,
        NotificationRoute::WarnAndDrop,
    )
    .await
    .expect("spawn a warmed fixture must succeed")
}

fn request(id: u64) -> Vec<u8> {
    let mut json = format!("{{\"id\":{id}}}").into_bytes();
    json.push(b'\n');
    json
}

fn env_pair(k: &str, v: &str) -> (String, String) {
    (k.to_string(), v.to_string())
}

// ---------------------------------------------------------------------
// Acceptance 1 -- grace delivers a late-but-real answer, never a resend.
// ---------------------------------------------------------------------

/// A server answering just past the original per-call deadline, but well
/// inside the bounded grace ceiling, still gets its answer delivered: the
/// call succeeds, a WARN was emitted, the session is NOT dead, and the
/// server's own received-request count is exactly 1 -- proving this was a
/// longer wait for the SAME request, never a second one sent.
#[tokio::test]
async fn a_late_answer_inside_the_grace_ceiling_is_delivered_once_with_a_warning() {
    let (log, _guard) = install_capture();
    let dir = TempDir::new().expect("tempdir");

    // timeout_ms == first_call_timeout_ms: this call's base deadline is 500ms
    // regardless of whether `ChildSession` treats it as "the first" ordinary
    // round trip, isolating the grace mechanism from the separate first-call
    // budget (acceptance 3's own concern). The fixture answers at 800ms --
    // past the 500ms base, but inside the 1500ms full ceiling
    // (`base * GRACE_CEILING_FACTOR`, factor 3) -- so this must succeed only
    // because of the grace extension, not because of a bigger base.
    let session = spawn_fake(
        dir.path(),
        "late.sh",
        &[env_pair("FAKE_FIRST_DELAY_S", "0.8")],
        500,
        500,
    )
    .await;

    let start = Instant::now();
    let value = session
        .framed_round_trip(1, request(1))
        .await
        .expect("a late-but-inside-the-ceiling answer must still be delivered");
    let elapsed = start.elapsed();

    assert_eq!(
        value.get("count").and_then(|v| v.as_u64()),
        Some(1),
        "the server's own received-request count must be exactly 1 -- a \
         second value here would mean this host resent the request, which \
         this item must never do: got {value:?}"
    );
    assert!(
        !session.is_dead(),
        "a call that succeeded, even late, must not leave the session dead"
    );
    assert!(
        elapsed >= Duration::from_millis(700) && elapsed < Duration::from_millis(1500),
        "expected the call to take roughly the fixture's own 800ms delay, \
         well inside the 1500ms full ceiling: took {elapsed:?}"
    );
    assert!(
        log.contains("exceeded its per-call deadline"),
        "expected a WARN naming the deadline overrun, got: {:?}",
        log.entries.lock().unwrap()
    );
}

// ---------------------------------------------------------------------
// Acceptance 2 -- a genuinely hung server is still killed, at the full
// ceiling, exactly as before.
// ---------------------------------------------------------------------

/// A server that never answers at all is still killed and reported
/// `TimedOut`, once the FULL ceiling (`base * GRACE_CEILING_FACTOR`)
/// elapses, exactly as today -- the fail-closed guarantee on a genuinely
/// hung/malicious server is unweakened; only a late-but-real answer gets
/// patience.
#[tokio::test]
async fn a_server_that_never_answers_is_still_killed_at_the_full_ceiling() {
    // Not asserted on here, but installed anyway: this test's own grace wait
    // reaches the SAME `tracing::warn!` call site `a_late_answer_inside_the_
    // grace_ceiling_is_delivered_once_with_a_warning` asserts on, and
    // `tracing`'s per-callsite `Interest` cache is a GLOBAL, once-per-process
    // decision (see `install_capture`'s own doc). A thread that reaches this
    // call site with NO subscriber installed at all can poison that decision
    // as "nobody is ever interested" for the WHOLE process, silently
    // starving the sibling test's own capture -- installing one here too,
    // even unused, keeps every hit of this call site "subscribed."
    let (_log, _guard) = install_capture();
    let dir = TempDir::new().expect("tempdir");

    // base 300ms; GRACE_CEILING_FACTOR (3) puts the full ceiling at 900ms.
    let session = spawn_fake(
        dir.path(),
        "hung.sh",
        &[env_pair("FAKE_NEVER_ANSWER", "1")],
        300,
        300,
    )
    .await;

    let start = Instant::now();
    let err = session
        .framed_round_trip(1, request(1))
        .await
        .expect_err("a server that never answers must still fail closed, not hang");
    let elapsed = start.elapsed();

    match err {
        TestError::TimedOut { after_ms } => {
            assert_eq!(
                after_ms, 900,
                "the reported deadline must be the FULL ceiling actually \
                 waited (300ms base + 600ms grace), not just the base"
            );
        }
        other => panic!("expected TimedOut, got {other:?}"),
    }
    assert!(
        session.is_dead(),
        "a call that exhausted the full ceiling must mark the session dead"
    );
    assert!(
        elapsed >= Duration::from_millis(850),
        "the call must actually wait out the full 900ms ceiling (not kill \
         early, on the old 300ms base alone): took {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the call must return once the full ceiling elapses (plus a bounded \
         kill-group grace), not hang: took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------
// Acceptance 3 -- the first ordinary round trip gets the warm-up budget;
// the second does not.
// ---------------------------------------------------------------------

/// A fixture slow ONLY on its first request answers well inside the larger
/// `first_call_timeout_ms` budget -- no grace needed, the bigger BASE alone
/// explains success -- proving the first ordinary round trip after spawn
/// really does get the warm-up deadline.
#[tokio::test]
async fn the_first_ordinary_round_trip_gets_the_warm_up_budget() {
    let dir = TempDir::new().expect("tempdir");

    // ordinary timeout_ms 300ms (ceiling with grace: 900ms); first-call
    // budget 1200ms. The fixture answers its FIRST request at 1000ms --
    // inside the 1200ms first-call base (no grace needed), but well past
    // even the ordinary 900ms full ceiling, so success here is only
    // possible because this genuinely is the session's first ordinary call.
    let session = spawn_fake(
        dir.path(),
        "warm_first.sh",
        &[env_pair("FAKE_FIRST_DELAY_S", "1.0")],
        300,
        1200,
    )
    .await;

    let start = Instant::now();
    let value = session
        .framed_round_trip(1, request(1))
        .await
        .expect("the first ordinary round trip must get the warm-up budget");
    let elapsed = start.elapsed();

    assert_eq!(value.get("count").and_then(|v| v.as_u64()), Some(1));
    assert!(!session.is_dead());
    assert!(
        elapsed < Duration::from_millis(1200),
        "expected success well inside the 1200ms first-call base: took {elapsed:?}"
    );
}

/// The IDENTICAL slowness (1000ms), when it lands on the SECOND ordinary
/// round trip instead of the first, is killed at the ordinary deadline (plus
/// its own bounded grace) -- proving the warm-up budget does NOT persist
/// past the one call it is meant for.
#[tokio::test]
async fn the_second_ordinary_round_trip_does_not_inherit_the_warm_up_budget() {
    // See `a_server_that_never_answers_is_still_killed_at_the_full_ceiling`'s
    // own comment: this test's grace wait reaches the SAME `tracing::warn!`
    // call site the sibling warn-asserting test does, and installing a
    // subscriber here too (even unused) keeps every hit "subscribed" against
    // `tracing`'s global per-callsite `Interest` cache.
    let (_log, _guard) = install_capture();
    let dir = TempDir::new().expect("tempdir");

    // Same 300ms ordinary / 1200ms first-call split as the sibling test
    // above, but the delay is on the SECOND request (`FAKE_OTHER_DELAY_S`),
    // not the first (`FAKE_FIRST_DELAY_S` left at its 0 default, so call #1
    // answers immediately and consumes the warm-up slot for nothing).
    let session = spawn_fake(
        dir.path(),
        "warm_second.sh",
        &[env_pair("FAKE_OTHER_DELAY_S", "1.0")],
        300,
        1200,
    )
    .await;

    let first = session
        .framed_round_trip(1, request(1))
        .await
        .expect("the first call (instant) must succeed and consume the warm-up slot");
    assert_eq!(first.get("count").and_then(|v| v.as_u64()), Some(1));

    let start = Instant::now();
    let err = session.framed_round_trip(2, request(2)).await.expect_err(
        "the second call, no longer eligible for the warm-up budget, must time out at the \
             ordinary deadline's own (300ms base + 600ms grace = 900ms) ceiling",
    );
    let elapsed = start.elapsed();

    match err {
        TestError::TimedOut { after_ms } => {
            assert_eq!(
                after_ms, 900,
                "expected the ORDINARY ceiling, not the first-call one"
            );
        }
        other => panic!("expected TimedOut, got {other:?}"),
    }
    assert!(session.is_dead());
    assert!(
        elapsed < Duration::from_secs(5),
        "must fail closed promptly once the ordinary ceiling elapses: took {elapsed:?}"
    );
}

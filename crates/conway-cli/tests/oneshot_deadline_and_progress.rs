//! Acceptance coverage for board item `01M1WVK9PF5G57Y6R36B94S0RB`: a
//! brand-new user's default `conway -p "<prompt>"` (`text` output mode) sat
//! completely silent for 90+ seconds against a real, slow local backend --
//! no spinner, no "thinking", nothing on stdout OR stderr to distinguish
//! "still working" from "hung". See `crates/conway-cli/src/oneshot.rs`'s
//! own module doc, reconciliation #9, for the full design.
//!
//! Every assertion here reads the real compiled binary's own stdout/stderr
//! content (never an internal signal) -- this item's own binding constraint
//! ("any test asserting the progress indicator appears must assert on the
//! actual observable output").

mod common;

use std::time::{Duration, Instant};

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, write_fixture_with_deadline};

/// Acceptance 1: `text` mode prints something observable before the full
/// reply lands, when the backend takes a few seconds to respond. The mock
/// holds the connection open (headers only) for 5 seconds before its first
/// SSE delta -- long enough for at least two of the progress ticker's
/// 2-second ticks (`oneshot::PROGRESS_NOTICE_INTERVAL`) to fire, comfortably
/// short of a test timeout, and comfortably under the 90-second silence
/// this item closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_mode_prints_a_progress_notice_before_the_full_reply_lands() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Delay(Duration::from_secs(5)),
        Chunk::Text("done"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        out.stdout, b"done\n",
        "stdout must still carry only the reply itself, unchanged by this item"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("waiting for a response"),
        "expected a progress notice on stderr before the reply landed, got: {stderr:?}"
    );
}

/// The `jsonl`/`json` output modes already stream real activity
/// immediately and are explicitly out of this item's scope -- the progress
/// ticker must never fire for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn progress_notice_never_fires_outside_text_mode() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Delay(Duration::from_secs(3)),
        Chunk::Text("done"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--output-format", "jsonl"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("waiting for a response"),
        "jsonl mode already shows real activity immediately; the text-mode-only progress \
         ticker must not fire here: {stderr:?}"
    );
}

/// Acceptance 2: a backend that never responds at all (`Chunk::Hang`, the
/// same mechanism the pre-existing SIGINT tests use for an unresponsive
/// connection) causes one-shot to fail within a bounded deadline, with a
/// named, actionable error -- rather than hanging indefinitely. A 2-second
/// `[limits].deadline_secs` (via `write_fixture_with_deadline`) exercises
/// the identical deadline-enforcement mechanism as one-shot's real 300-
/// second production default (`oneshot::DEFAULT_ONE_SHOT_DEADLINE_SECS`)
/// without a test actually waiting that long -- from the outside, "no CLI
/// flag bounded this run, the backend hung, the process still failed loud
/// well under the old 90-second silence" is exactly what both prove.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unresponsive_backend_fails_loud_within_the_bounded_deadline() {
    let mock = MockBackend::start(Script(vec![vec![Chunk::Hang]])).await;
    let fixture = write_fixture_with_deadline(&mock, 40, 2);

    let started = Instant::now();
    // Deliberately no `--max-seconds`: this must prove the DEFAULT bound,
    // not the pre-existing flag-driven one.
    let out = run_conway(&["-p", "hi"], &fixture);
    let elapsed = started.elapsed();

    assert_eq!(
        out.status.code(),
        Some(5),
        "expected BudgetExceeded (exit 5), stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        elapsed < Duration::from_secs(20),
        "a stalled backend must fail loud well under the old 90-second silence, took {elapsed:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no response from mock/mock-model"),
        "the deadline error must name the backend that was routed to: {stderr:?}"
    );
    assert!(
        stderr.contains("deadline"),
        "the deadline error must say why the run stopped: {stderr:?}"
    );
}

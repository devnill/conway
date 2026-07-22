//! WI-117: integration tests for the session-continuity flags
//! (`--session`/`--resume`/`--fork-from`) against the real, compiled
//! `conway` binary.
//!
//! ## Reconciliations (disclosed against the currently-committed facade)
//!
//! Three of this item's own criteria describe a "drive a new/branched turn
//! against a pre-existing session" outcome that this suite's own
//! investigation (documented at length in `oneshot.rs`'s module doc,
//! reconciliation #4, and `resolve_session`'s doc comment) proves is **not
//! reachable** from one-shot mode today, given the currently-committed
//! `conway`/`conway-runtime`: there is exactly one way to register a *live*
//! root agent (`Runtime::start_root`, reached only through
//! `Conway::new_session`), and it always mints its own fresh `SessionId`
//! and always `store.create`s a brand-new session. Every one of
//! `--session <new-id>`, `--resume <id>`, and `--fork-from <ref>` wants to
//! drive a turn against a session it did not just freshly create that way,
//! and so cannot, under the committed facade:
//!
//! - **`session_flag_sets_id`**: the plan doc's "`--session <new-id>`
//!   creates that id" half is unimplementable -- `SessionSpec` has no `id`
//!   field and `Conway::new_session` hardcodes `SessionId::new()`. This
//!   test asserts the real, disclosed behavior (exit 2, naming the gap)
//!   for a fresh id, alongside the "reusing an existing id ... exits 2"
//!   half, which *is* fully implementable and passes as specified.
//! - **`resume_continues_transcript`**: `SessionHandle::prompt` (via
//!   `Runtime::prompt`) looks the root agent up in `Runtime`'s in-memory
//!   `agents` map, which `Conway::resume` never populates (confirmed by
//!   reading `conway-runtime/src/runtime.rs`'s `prompt`/`agent_session`) --
//!   `RuntimeError::AgentNotFound`, deterministically, for every resumed
//!   handle. This is the *same* carried gap WI-103's own `F-103-1` finding
//!   already flagged for `Conway::resume` itself ("prompt() after resume");
//!   this test is that gap's first observation from a real, compiled `-p`
//!   invocation. Asserts the real code (2, with an empty stdout and a
//!   stderr diagnostic naming the gap) rather than the plan doc's literal
//!   "exit 0, second request carries the first turn's text" -- which this
//!   suite's mock backend proves never even receives a second request.
//! - **`fork_from_creates_child`**/**`fork_from_without_seq_uses_head`**:
//!   *do* pass as specified -- `Conway::fork_from` (WI-103, store-only)
//!   never needs a live parent, so the child session is genuinely created
//!   and independently visible (verified here via `Conway::sessions`
//!   opened fresh against the fixture's on-disk store, not via a
//!   `conway sessions tree` CLI subcommand -- see the note below). What
//!   these two tests do *not* get is a live turn on the child: same root
//!   cause, one layer later (the child's own agent is never registered
//!   either), and `Conway::fork_from` does not even persist
//!   `ForkSpec::directive` anywhere. Both tests still assert exit 0 (the
//!   fork itself is a real, complete success) and empty stdout.
//!
//! None of this is a bug in this test suite's assertions -- it is the
//! observed behavior of already-committed code, traced and disclosed
//! exactly as `tests/oneshot.rs`'s own `exit_4_no_backend`/
//! `exit_3_permission_termination` disclose their analogous gaps.
//!
//! ## Why this suite never shells out to `conway sessions tree`/`list`
//!
//! The plan doc's own test recipes name `conway sessions list`/
//! `sessions tree` as the verification mechanism for "capture the session
//! id" and "child visible in the tree." Those subcommands are WI-116's
//! scope, a sibling item with **no dependency edge to or from WI-117**
//! (both depend only on earlier, already-landed items -- WI-116 on
//! WI-101/103/111/113, this item on WI-103/112/113). This suite does not
//! take on an ordering dependency on a
//! concurrently-in-flight sibling for its own correctness: every fact the
//! plan doc's recipe would read off `sessions list`/`tree` output (a
//! session's existence, a child's `origin.parent`/`origin.at_seq`) is read
//! here directly off `Conway::sessions`/`SessionMeta`, opened fresh against
//! the same on-disk store the subprocess just wrote -- the same underlying
//! data a real `sessions tree` renderer would format, just read through the
//! facade instead of through a not-yet-built CLI layer.

mod common;

use std::sync::Arc;

use common::{command, run_conway, write_fixture, Fixture};
use conway::config::CliOverrides;
use conway::gates::AllowListGate;
use conway::{Conway, ConwayBuilder, PermissionGate, SessionFilter, SessionId};

use common::mock_backend::{Chunk, MockBackend, Script};

/// Opens a fresh, read-only `Conway` against `fixture`'s on-disk session
/// store -- the same store the compiled binary's subprocess runs wrote to.
///
/// Two deviations from just calling `ConwayBuilder::from_config(..).build()`,
/// both required only because this helper builds a `Conway` directly rather
/// than going through `main.rs`'s own construction path:
/// - `CliOverrides::cwd` is set explicitly to `fixture.dir.path()` rather
///   than relying on `ConwayConfig`'s own `default_cwd()` (a serde default
///   evaluated at TOML-parse time against *this test process's* cwd, which
///   is the crate root, not the fixture's temp dir) -- without this
///   override, the default `.conway/sessions` session-store path would
///   resolve relative to the wrong directory and this function would
///   silently open an empty store.
/// - A gate is supplied explicitly: the fixture's `conway.toml` leaves
///   `permissions.mode` at its config-level default (`"prompt"`, meant for
///   an interactive embedder), and `ConwayBuilder::build` refuses to
///   assemble a `Conway` for `"prompt"` mode without one (`main.rs`'s own
///   one-shot dispatch path supplies `oneshot::build_gate`'s
///   `AllowListGate` for exactly this reason before ever calling
///   `build_conway`). This helper only ever calls read-only `Conway`
///   methods (`resume`/`sessions`/`transcript`), so which concrete gate is
///   used is immaterial -- an empty, fail-closed `AllowListGate` (the same
///   default `oneshot::build_gate` itself produces) is enough to satisfy
///   `build`'s validation.
async fn open_conway(fixture: &Fixture) -> Conway {
    let gate: Arc<dyn PermissionGate> = Arc::new(AllowListGate::new(Vec::new(), Vec::new()));
    ConwayBuilder::from_config(&fixture.config_path)
        .expect("load fixture config")
        .with_cli_overrides(CliOverrides {
            cwd: Some(fixture.dir.path().to_path_buf()),
            ..Default::default()
        })
        .with_permission_gate(gate)
        .build()
        .expect("build conway against the fixture's own store")
}

/// The one session a freshly-populated fixture has created so far.
async fn only_session_id(fixture: &Fixture) -> SessionId {
    let conway = open_conway(fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 1, "expected exactly one session so far");
    sessions[0].id
}

fn ok_script() -> Script {
    Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])
}

// ---------------------------------------------------------------------
// --resume
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_unknown_session_exits_2_empty_stdout() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let unknown = SessionId::new();
    let out = run_conway(&["-p", "hi", "--resume", &unknown.to_string()], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "stdout must stay empty");
    assert!(
        !out.stderr.is_empty(),
        "stderr should explain the unknown session"
    );
}

/// See this file's module doc for why the real, observed outcome here is
/// exit 2 (not the plan doc's literal exit 0) -- `--resume` reattaches
/// successfully, but nothing in the currently-committed facade can drive a
/// *new* turn against the reattached session.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_transcript() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "remember X"], &fixture);
    assert!(
        first.status.success(),
        "first run must succeed: stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let sid = only_session_id(&fixture).await;

    let second = run_conway(
        &["-p", "what did I say", "--resume", &sid.to_string()],
        &fixture,
    );

    assert_eq!(
        second.status.code(),
        Some(2),
        "disclosed reconciliation: resuming succeeds, but one-shot mode cannot yet drive a new \
         turn against a resumed session (RuntimeError::AgentNotFound, deterministically -- see \
         this file's module doc). stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(second.stdout.is_empty(), "stdout must stay empty");
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(
        stderr.contains("cannot drive a new turn"),
        "stderr should name the disclosed gap: {stderr}"
    );

    // The mock only ever received the first run's request -- proving the
    // second invocation never reached the model, rather than silently
    // reissuing the prompt as if it were a fresh, unrelated session.
    assert_eq!(mock.requests().len(), 1);
}

// ---------------------------------------------------------------------
// --fork-from
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_creates_child() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success(), "first run must succeed");
    let parent = only_session_id(&fixture).await;

    let second = run_conway(
        &["-p", "branch this", "--fork-from", &format!("{parent}@1")],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        second.stdout.is_empty(),
        "no live turn ever runs for --fork-from today (see module doc); stdout purity holds \
         trivially"
    );

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let children: Vec<_> = sessions
        .iter()
        .filter(|s| s.origin.as_ref().is_some_and(|o| o.parent == parent))
        .collect();
    assert_eq!(children.len(), 1, "exactly one child of {parent}");
    assert_eq!(children[0].origin.as_ref().unwrap().at_seq.0, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_without_seq_uses_head() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success(), "first run must succeed");
    let parent = only_session_id(&fixture).await;

    let expected_head = {
        let conway = open_conway(&fixture).await;
        let parent_handle = conway.resume(parent).await.expect("resume parent");
        let root = parent_handle.root();
        let records = parent_handle.transcript(root).await.expect("transcript");
        records.len() as u64
    };

    let second = run_conway(
        &["-p", "branch this", "--fork-from", &parent.to_string()],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(second.stdout.is_empty());

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let children: Vec<_> = sessions
        .iter()
        .filter(|s| s.origin.as_ref().is_some_and(|o| o.parent == parent))
        .collect();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].origin.as_ref().unwrap().at_seq.0, expected_head);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_seq_beyond_head_exits_2() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success(), "first run must succeed");
    let parent = only_session_id(&fixture).await;

    let out = run_conway(
        &[
            "-p",
            "branch this",
            "--fork-from",
            &format!("{parent}@999999"),
        ],
        &fixture,
    );

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.to_lowercase().contains("head"),
        "stderr should name the parent's head: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_malformed_ref_exits_2() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--fork-from", "not-a-ulid"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("<session-id>[@<seq>]"), "stderr: {stderr}");
}

// ---------------------------------------------------------------------
// --session
// ---------------------------------------------------------------------

/// See this file's module doc: the "creates that id" half of this
/// criterion is unimplementable against the current facade (no
/// caller-chosen-id constructor exists at all); the "reusing an existing
/// id ... exits 2" half is fully implementable and passes as specified.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_flag_sets_id() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let fresh = SessionId::new();
    let fresh_run = run_conway(&["-p", "hi", "--session", &fresh.to_string()], &fixture);
    assert_eq!(
        fresh_run.status.code(),
        Some(2),
        "disclosed reconciliation: no facade constructor accepts a caller-chosen SessionId -- \
         see this file's module doc. stderr: {}",
        String::from_utf8_lossy(&fresh_run.stderr)
    );
    assert!(fresh_run.stdout.is_empty());
    let fresh_stderr = String::from_utf8_lossy(&fresh_run.stderr);
    assert!(
        fresh_stderr.contains("caller-chosen id"),
        "stderr: {fresh_stderr}"
    );

    let setup = run_conway(&["-p", "hi"], &fixture);
    assert!(setup.status.success(), "setup run must succeed");
    let existing = only_session_id(&fixture).await;

    let reuse_run = run_conway(&["-p", "hi", "--session", &existing.to_string()], &fixture);
    assert_eq!(reuse_run.status.code(), Some(2));
    assert!(reuse_run.stdout.is_empty());
    let reuse_stderr = String::from_utf8_lossy(&reuse_run.stderr);
    assert!(
        reuse_stderr.contains("already exists"),
        "stderr: {reuse_stderr}"
    );
}

// ---------------------------------------------------------------------
// Mutual exclusivity (belt-and-braces integration check; the criterion's
// own "unit test" is `oneshot.rs`'s `conflicting_continuity_flags_are_a_usage_error`)
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflicting_flags_exit_2_via_real_binary() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let sid = SessionId::new().to_string();
    let out = run_conway(&["-p", "hi", "--session", &sid, "--resume", &sid], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}

/// Sanity check that this suite's own harness usage (`command`, not just
/// `run_conway`) still resolves against the shared fixture helpers -- keeps
/// the `command` import from going unused should every test above switch to
/// `run_conway` during review.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plain_run_still_works_without_any_continuity_flag() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = command(&["-p", "hi"], &fixture)
        .output()
        .expect("run conway");
    assert!(out.status.success());
    assert_eq!(out.stdout, b"ok\n");
}

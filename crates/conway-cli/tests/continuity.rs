//! Integration tests for the session-continuity flags
//! (`--session`/`--resume`/`--fork-from`) against the real, compiled
//! `conway` binary.
//!
//! ## History (disclosed)
//!
//! wired and validated these three flags but could not drive a live
//! turn against a pre-existing session: `conway`/`conway-runtime` at that
//! point had exactly one way to register a *live* root agent
//! (`Runtime::start_root`, reached only through `Conway::new_session`),
//! which always minted its own fresh `SessionId` and always
//! `store.create`d a brand-new session. `session_flag_sets_id`'s "creates
//! that id" half, `resume_continues_transcript`, and
//! `fork_from_creates_child`'s live-turn half all asserted the then-real,
//! disclosed blocked behavior (exit 2 / an inert exit 0) instead.
//!
//! `Runtime::resume_root` and the facade wiring: caller-
//! chosen `SessionSpec::id`, a drivable `Conway::resume`, a
//! live-registered, context-inheriting `Conway::fork_from` child) closed
//! that gap. A later change flips every test below that asserted the old blocked
//! behavior to assert the now-working one, specified from the start:
//! `--session <new-id>` creates exactly that id; `--resume <id>` continues
//! the persisted transcript into a live second turn; `--fork-from <ref>`
//! drives a live turn on the forked child. The honest-error paths that were
//! always correct (`resume_unknown_session`, `fork_from_seq_beyond_head`,
//! `fork_from_malformed_ref`, conflicting flags) are unchanged.
//!
//! ## Why this suite now shells out to `conway sessions tree`
//!
//! its own module doc explained why it read fork-child facts (a
//! child's `origin.parent`/`origin.at_seq`) directly off `Conway::sessions`
//! rather than through `conway sessions tree`: that subcommand was later work's
//! scope, a sibling item concurrently in flight with no dependency edge to
//! or from later work, which has since landed (`sessions tree` is real,
//! compiled CLI surface -- `crates/conway-cli/src/commands/sessions.rs`),
//! and this item's own criterion for `fork_from_creates_child` names it
//! explicitly ("`conway sessions tree <sid>` shows exactly one child with
//! origin seq 1"), so `fork_from_creates_child` now asserts against the
//! real subcommand's stdout. The other fork tests
//! (`fork_from_without_seq_uses_head`, `fork_from_seq_beyond_head_exits_2`)
//! keep reading `Conway::sessions` directly -- they check facts (the
//! resolved head seq, the "no matching child" case) `sessions tree`'s
//! human-readable tree output was not built to assert against precisely,
//! and re-deriving them from that text would be a strictly weaker check
//! than reading the same underlying `SessionMeta` the renderer itself
//! reads.

mod common;

use common::{command, open_conway, run_conway, write_fixture, Fixture};
use conway::{SessionFilter, SessionId};

use common::mock_backend::{Chunk, MockBackend, Script};

// `open_conway` used to be a byte-identical copy local to this file; it
// moved to `common` (board item `01M0QK9GRM8HSNWRAR414TCX42`) once
// `[session].root`'s central-default resolution stopped being fixable by
// this file's own `CliOverrides::cwd` trick alone -- see that function's
// own doc for the full reasoning, including the module-level history above
// naming the original `from_config` ambient-read bug this helper (both
// versions) exists to avoid.

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

/// `--resume`: reattaches the persisted session from the first
/// `-p` invocation and drives a genuine second turn against it -- the
/// mock's second request must carry the first turn's own text, proving the
/// transcript was continued rather than the second invocation starting a
/// fresh, contextless session under the same id.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_continues_transcript() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("noted"), Chunk::Finish("stop")],
        vec![Chunk::Text("you said remember X"), Chunk::Finish("stop")],
    ]))
    .await;
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
        Some(0),
        "resuming must drive a real second turn: stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        second.stdout, b"you said remember X\n",
        "the resumed turn's own output must reach stdout"
    );

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "--resume must drive a real second request against the backend, got: {requests:?}"
    );
    let second_request = requests[1].to_string();
    assert!(
        second_request.contains("remember X"),
        "expected the resumed turn's request to carry the first turn's text: {second_request}"
    );
}

// ---------------------------------------------------------------------
// --fork-from
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_creates_child() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
        vec![Chunk::Text("branched"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    // A distinctive phrase (not just "hi") so the inheritance assertion
    // below can't be satisfied by accident (e.g. by a substring of the
    // child's own prompt or the mock's scripted responses).
    let first = run_conway(&["-p", "remember the root context"], &fixture);
    assert!(first.status.success(), "first run must succeed");
    let parent = only_session_id(&fixture).await;

    // Fork at the parent's own head (rather than an arbitrary earlier seq)
    // so the child's inherited prefix includes the parent's real prompt
    // text -- a root session created by this CLI's flag-free/`--session`
    // path always carries a leading empty `UserTurn` placeholder ahead of
    // its real prompt (`Runtime::start_root` is always called with
    // `RootSpec.prompt: None`; the real text lands via `run`'s own,
    // separate `handle.prompt`), so a fork at an earlier seq like `@1`
    // would inherit only that empty placeholder, not the real text.
    let parent_head = open_conway(&fixture)
        .await
        .session_head(parent)
        .await
        .expect("read parent head");
    let second = run_conway(
        &[
            "-p",
            "branch this",
            "--fork-from",
            &format!("{parent}@{}", parent_head.0),
        ],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        second.stdout, b"branched\n",
        "the -p prompt on the forked child (fork_from now registers a live, \
         context-inheriting child) must produce real output"
    );

    // This criterion names `conway sessions tree <sid>` as its own
    // verification mechanism (has since landed) -- shell out to the
    // real subcommand rather than reading `Conway::sessions` directly.
    let tree_out = run_conway(&["sessions", "tree", &parent.to_string()], &fixture);
    assert!(
        tree_out.status.success(),
        "sessions tree must succeed: stderr: {}",
        String::from_utf8_lossy(&tree_out.stderr)
    );
    let tree_text = String::from_utf8_lossy(&tree_out.stdout).into_owned();
    let tree_lines: Vec<&str> = tree_text.lines().collect();
    assert_eq!(
        tree_lines.len(),
        2,
        "expected the root plus exactly one child line: {tree_lines:?}"
    );
    assert!(
        tree_lines[1].starts_with("└─ "),
        "the single child must be the tree's only (and thus last) branch: {tree_lines:?}"
    );

    // `sessions tree`'s label carries role, not the fork origin's seq
    // number -- read that off `Conway::sessions`/`SessionMeta` directly,
    // against the same on-disk store the subprocess just wrote.
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
    assert_eq!(children[0].origin.as_ref().unwrap().at_seq.0, parent_head.0);

    // The module doc's "genuinely inheriting the parent's context" claim
    // needs more than structural checks (child count, origin seq) to back
    // it up -- assert, via the mock's own recorded requests, that the
    // child's backend call actually carried the parent's prior-turn text.
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "parent and child each drive exactly one request, got: {requests:?}"
    );
    let child_request = requests[1].to_string();
    assert!(
        child_request.contains("remember the root context"),
        "expected the forked child's backend request to carry the parent's inherited turn \
         text: {child_request}"
    );
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

/// Regression test: `--fork-from <ref>`
/// with no `@seq`, where `<ref>` itself names a fork child that has since
/// taken its own turn, must compute the fork point from that child's own
/// LOCAL head -- not its effective (ancestry-resolved) transcript length.
///
/// Before the fix, the no-`@seq` arm computed `at =
/// LogSeq(parent_handle.transcript(root).len())`, which for a
/// fork-of-a-fork overcounts the local head by exactly the size of the
/// inherited prefix: forking root A at its own head, then forking child B
/// from that point and driving B's own turn, B's own LOCAL head (just its
/// one turn's own records) is strictly smaller than B's *effective*
/// transcript length (A's whole inherited prefix, plus B's own records). A
/// bare `--fork-from B` used to compute `at` from that inflated effective
/// length, which `Conway::fork_from`'s bounds check then rejected as
/// `at > head(B)` -- a `SeqOutOfRange` naming a seq the user never typed.
/// This test drives that exact three-generation shape and asserts it now
/// succeeds, forks at B's true local head, and that the grandchild still
/// inherits the whole ancestor chain (root A's own turn text reaches the
/// grandchild's backend request).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_without_seq_on_a_fork_child_uses_local_head() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("ack-root"), Chunk::Finish("stop")],
        vec![Chunk::Text("ack-child"), Chunk::Finish("stop")],
        vec![Chunk::Text("ack-grandchild"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    // Root A takes one turn.
    let root_run = run_conway(&["-p", "remember alpha from the root"], &fixture);
    assert!(
        root_run.status.success(),
        "root turn must succeed: stderr: {}",
        String::from_utf8_lossy(&root_run.stderr)
    );
    let root = only_session_id(&fixture).await;
    let root_head = open_conway(&fixture)
        .await
        .session_head(root)
        .await
        .expect("read root head");

    // Fork B from A at A's own head (full inheritance), then drive B's own
    // first turn -- B's own LOCAL head only counts its own turn's records,
    // while B's *effective* transcript also carries A's whole inherited
    // prefix. This mismatch is exactly the shape that used to break the
    // next, no-`@seq` fork.
    let child_run = run_conway(
        &[
            "-p",
            "second gen",
            "--fork-from",
            &format!("{root}@{}", root_head.0),
        ],
        &fixture,
    );
    assert_eq!(
        child_run.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&child_run.stderr)
    );

    let child = {
        let conway = open_conway(&fixture).await;
        let sessions = conway
            .sessions(SessionFilter::default())
            .await
            .expect("list sessions");
        sessions
            .iter()
            .find(|s| s.origin.as_ref().is_some_and(|o| o.parent == root))
            .expect("child of root")
            .id
    };
    let child_conway = open_conway(&fixture).await;
    let child_local_head = child_conway
        .session_head(child)
        .await
        .expect("read child local head");
    let child_effective_len = {
        let child_handle = child_conway.resume(child).await.expect("resume child");
        let child_root = child_handle.root();
        child_handle
            .transcript(child_root)
            .await
            .expect("child transcript")
            .len() as u64
    };
    assert!(
        child_effective_len > child_local_head.0,
        "test precondition: the child's effective transcript ({child_effective_len}) must \
         exceed its local head ({}) for this regression to be meaningful -- otherwise the old, \
         buggy computation and the fixed one would coincide",
        child_local_head.0
    );

    // `--fork-from <child>` with NO `@seq`: before the fix, this computed
    // `at` from the child's inflated effective transcript length
    // (child_effective_len) against a smaller local head
    // (child_local_head), and `fork_from` rejected it with `SeqOutOfRange`.
    let grandchild_run = run_conway(
        &["-p", "third gen", "--fork-from", &child.to_string()],
        &fixture,
    );
    assert_eq!(
        grandchild_run.status.code(),
        Some(0),
        "fork-of-a-fork with no @seq must succeed at the child's own local head, not fail with \
         SeqOutOfRange: stderr: {}",
        String::from_utf8_lossy(&grandchild_run.stderr)
    );
    assert_eq!(grandchild_run.stdout, b"ack-grandchild\n");

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let grandchild_meta = sessions
        .iter()
        .find(|s| s.origin.as_ref().is_some_and(|o| o.parent == child))
        .expect("grandchild of child");
    assert_eq!(
        grandchild_meta.origin.as_ref().unwrap().at_seq.0,
        child_local_head.0,
        "the fork point must be the child's own LOCAL head, not its (larger) effective \
         transcript length"
    );

    // Full inheritance: the grandchild's own backend request must carry
    // the root's original turn text, proving the whole ancestor chain
    // (root's inherited prefix + child's own turn) reached the grandchild.
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        3,
        "root, child, and grandchild each drive exactly one request, got: {requests:?}"
    );
    let grandchild_request = requests[2].to_string();
    assert!(
        grandchild_request.contains("remember alpha from the root"),
        "expected the grandchild's request to carry the root's own turn text, proving full \
         ancestor-chain inheritance: {grandchild_request}"
    );
}

/// Regression test: `--fork-from` combined with
/// `--cwd` must be an honest, explicit usage error rather than silently
/// dropping `--cwd` -- `ForkSpec` has no field to carry a `cwd` override
/// through to `Conway::fork_from`, so plumbing it would mean growing that
/// facade signature (out of this item's minimal-blast-radius scope).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_with_cwd_is_a_usage_error() {
    let mock =
        MockBackend::start(Script(vec![vec![Chunk::Text("ok"), Chunk::Finish("stop")]])).await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success(), "first run must succeed");
    let parent = only_session_id(&fixture).await;

    let out = run_conway(
        &[
            "-p",
            "branch this",
            "--fork-from",
            &parent.to_string(),
            "--cwd",
            fixture.dir.path().to_str().expect("utf8 tempdir path"),
        ],
        &fixture,
    );

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--cwd") && stderr.contains("--fork-from"),
        "stderr should name both conflicting flags: {stderr}"
    );
}

/// Regression test: `--fork-from` combined with
/// `--role-override` must actually take effect -- wired through
/// `ForkSpec::role`, which `Conway::fork_from` already honors
/// (`spec.role.or(parent_meta.role)`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_with_role_override_sets_child_role() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
        vec![Chunk::Text("branched"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success(), "first run must succeed");
    let parent = only_session_id(&fixture).await;

    let second = run_conway(
        &[
            "-p",
            "branch this",
            "--fork-from",
            &parent.to_string(),
            "--role-override",
            "coder",
        ],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let child = sessions
        .iter()
        .find(|s| s.origin.as_ref().is_some_and(|o| o.parent == parent))
        .expect("child of parent");
    assert_eq!(
        child.role.as_ref().map(|r| r.as_str()),
        Some("coder"),
        "--role-override must reach the forked child's own SessionMeta.role via ForkSpec::role, \
         got: {child:?}"
    );
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

/// `--session <new-id>`: creates exactly the requested id; reusing
/// that same id on a later invocation without `--resume` still exits 2.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_flag_sets_id() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);

    let fresh = SessionId::new();
    let fresh_run = run_conway(&["-p", "hi", "--session", &fresh.to_string()], &fixture);
    assert_eq!(
        fresh_run.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&fresh_run.stderr)
    );
    assert_eq!(fresh_run.stdout, b"ok\n");

    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    assert!(
        sessions.iter().any(|m| m.id == fresh),
        "--session must create a session under exactly the requested id, got: {sessions:?}"
    );

    let reuse_run = run_conway(&["-p", "hi", "--session", &fresh.to_string()], &fixture);
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

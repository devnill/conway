//! Integration tests for `conway sessions label`/`conway sessions unlabel`
//! -- run against the real compiled `conway` binary. Board item
//! `01M1WVKVSDXHB68J66VZ9HE8B3`: closes the gap where `sessions list
//! --label`/`conway.discover`'s `label` parameter had a fully working read
//! side and no shipped way to write a label onto an already-existing
//! session. Mirrors `session_names.rs`'s own test structure for the
//! sibling `name`/`unname` pair, including its use of the compiled binary
//! rather than in-crate unit tests -- this is the CLI surface, not
//! `SessionStore::add_label`/`remove_label`'s own unit coverage (that lives
//! in `crates/conway-session/tests/`).

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{open_conway, run_conway, write_fixture, Fixture};
use conway::{SessionFilter, SessionId};
use serde_json::Value;

/// The one session a freshly-populated fixture has created so far --
/// byte-identical to `session_names.rs`'s own helper of the same name (each
/// `tests/*.rs` file compiles as its own independent crate, so sharing it
/// via `common` would grow that module's own surface for a single-file
/// need).
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn label_then_list_by_label_shows_it_and_unlabel_removes_it() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;

    // Before labeling, the label filter matches nothing.
    let before = run_conway(&["sessions", "list", "--label", "foo", "--json"], &fixture);
    assert!(before.status.success());
    let before_val: Value = serde_json::from_slice(&before.stdout).expect("stdout is JSON array");
    assert_eq!(
        before_val.as_array().expect("array").len(),
        0,
        "no session should match an unattached label yet: {before_val:?}"
    );

    let label_out = run_conway(&["sessions", "label", &sid.to_string(), "foo"], &fixture);
    assert!(
        label_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&label_out.stderr)
    );

    let after = run_conway(&["sessions", "list", "--label", "foo", "--json"], &fixture);
    assert!(after.status.success());
    let after_val: Value = serde_json::from_slice(&after.stdout).expect("stdout is JSON array");
    let after_arr = after_val.as_array().expect("array");
    assert_eq!(
        after_arr.len(),
        1,
        "exactly the labeled session should match: {after_arr:?}"
    );
    assert_eq!(after_arr[0]["id"].as_str(), Some(sid.to_string().as_str()));

    // `--label` with a different value still matches nothing -- this is an
    // exact match, not a substring/prefix match.
    let other_label = run_conway(&["sessions", "list", "--label", "bar", "--json"], &fixture);
    assert!(other_label.status.success());
    let other_val: Value =
        serde_json::from_slice(&other_label.stdout).expect("stdout is JSON array");
    assert_eq!(other_val.as_array().expect("array").len(), 0);

    let unlabel_out = run_conway(&["sessions", "unlabel", &sid.to_string(), "foo"], &fixture);
    assert!(
        unlabel_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unlabel_out.stderr)
    );

    let gone = run_conway(&["sessions", "list", "--label", "foo", "--json"], &fixture);
    assert!(gone.status.success());
    let gone_val: Value = serde_json::from_slice(&gone.stdout).expect("stdout is JSON array");
    assert_eq!(
        gone_val.as_array().expect("array").len(),
        0,
        "unlabel must remove the match: {gone_val:?}"
    );

    // The session itself is entirely unaffected -- `sessions show` still
    // resolves it by its own (unchanged) id, same contract `unname`'s own
    // test pins for names.
    let show_out = run_conway(&["sessions", "show", &sid.to_string(), "--json"], &fixture);
    assert!(show_out.status.success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn label_is_idempotent_and_a_session_can_carry_more_than_one() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;

    let first = run_conway(&["sessions", "label", &sid.to_string(), "foo"], &fixture);
    assert!(first.status.success());
    // Re-labeling with the same label is a no-op success, not a refusal --
    // deliberately unlike `name`'s ULID-shape/collision refusals, since a
    // label is not a bijective identifier.
    let again = run_conway(&["sessions", "label", &sid.to_string(), "foo"], &fixture);
    assert!(
        again.status.success(),
        "re-labeling an already-labeled session must be idempotent: {}",
        String::from_utf8_lossy(&again.stderr)
    );

    let second_label = run_conway(&["sessions", "label", &sid.to_string(), "bar"], &fixture);
    assert!(second_label.status.success());

    let foo_out = run_conway(&["sessions", "list", "--label", "foo", "--json"], &fixture);
    let foo_val: Value = serde_json::from_slice(&foo_out.stdout).expect("json");
    assert_eq!(foo_val.as_array().expect("array").len(), 1);

    let bar_out = run_conway(&["sessions", "list", "--label", "bar", "--json"], &fixture);
    let bar_val: Value = serde_json::from_slice(&bar_out.stdout).expect("json");
    assert_eq!(
        bar_val.as_array().expect("array").len(),
        1,
        "a session must be able to carry more than one label at once"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlabel_a_label_the_session_never_had_is_a_no_op_success() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;

    // Deliberately unlike `unname` on an unbound name (a usage error): a
    // label is not a bijective binding a caller could target incorrectly,
    // so removing one that was never attached is idempotent, not a
    // refusal -- see `SessionStore::remove_label`'s own doc.
    let out = run_conway(
        &["sessions", "unlabel", &sid.to_string(), "never-set"],
        &fixture,
    );
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn label_unknown_session_id_exits_2_with_a_named_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    let bogus = SessionId::new();

    let out = run_conway(&["sessions", "label", &bogus.to_string(), "foo"], &fixture);
    assert_eq!(
        out.status.code(),
        Some(2),
        "an unknown session id must be a usage error (exit 2), not a crash or a silent success"
    );
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains(&bogus.to_string()),
        "the refusal must name the unknown session id: {stderr:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unlabel_unknown_session_id_exits_2_with_a_named_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    let bogus = SessionId::new();

    let out = run_conway(
        &["sessions", "unlabel", &bogus.to_string(), "foo"],
        &fixture,
    );
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        stderr.contains(&bogus.to_string()),
        "the refusal must name the unknown session id: {stderr:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn label_resolves_the_target_by_an_existing_name() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;

    let name_out = run_conway(&["sessions", "name", &sid.to_string(), "daily"], &fixture);
    assert!(name_out.status.success());

    // `label`'s `ID` argument accepts a name exactly like `show`/`tree`/
    // `export`/`name`/`unname` do -- the same id-or-name grammar throughout
    // this subcommand family.
    let label_out = run_conway(&["sessions", "label", "daily", "foo"], &fixture);
    assert!(
        label_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&label_out.stderr)
    );

    let list_out = run_conway(&["sessions", "list", "--label", "foo", "--json"], &fixture);
    let value: Value = serde_json::from_slice(&list_out.stdout).expect("json");
    let arr = value.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"].as_str(), Some(sid.to_string().as_str()));
}

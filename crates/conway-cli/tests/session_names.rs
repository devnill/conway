//! Integration tests for `conway sessions name`/`conway sessions unname`,
//! and for `--session`/`--resume`/`--fork-from` accepting a name wherever
//! they accept an id -- run against the real compiled `conway` binary.
//! Pure `NamesStore` logic (round-tripping, the ULID-shape refusal, the
//! collision refusal, rename-by-moving-the-one-name) is unit-tested
//! in-crate, in `src/session_names.rs`'s own `#[cfg(test)] mod tests`; this
//! file exercises the CLI surface those units are wired into.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, open_conway, run_conway, write_fixture, Fixture};
use conway::{SessionFilter, SessionId};
use serde_json::Value;

/// The one session a freshly-populated fixture has created so far --
/// byte-identical to `continuity.rs`'s own helper of the same name (each
/// `tests/*.rs` file compiles as its own independent crate, so sharing it
/// via `common` would grow that module's own surface for a single-file
/// need; every sibling suite in this directory makes the same call).
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
// sessions name / unname
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_then_list_shows_the_name_and_unnamed_rows_stay_blank() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());
    let named = only_session_id(&fixture).await;

    let name_out = run_conway(&["sessions", "name", &named.to_string(), "daily"], &fixture);
    assert!(
        name_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&name_out.stderr)
    );

    // A second, unnamed session, so the table has one named row and one
    // blank row to check.
    let second = run_conway(&["-p", "hi"], &fixture);
    assert!(second.status.success());

    let list_out = run_conway(&["sessions", "list", "--json"], &fixture);
    assert!(list_out.status.success());
    let value: Value = serde_json::from_slice(&list_out.stdout).expect("stdout is a JSON array");
    let arr = value.as_array().expect("top-level array");
    assert_eq!(arr.len(), 2, "expected exactly two sessions: {arr:?}");

    let named_obj = arr
        .iter()
        .find(|v| v["id"].as_str() == Some(named.to_string().as_str()))
        .unwrap_or_else(|| panic!("no element for {named} in {arr:?}"));
    assert_eq!(named_obj["name"].as_str(), Some("daily"));

    let other_obj = arr
        .iter()
        .find(|v| v["id"].as_str() != Some(named.to_string().as_str()))
        .expect("the other session");
    // Blank, not a synthesized placeholder like `null` rendered as text or
    // `"-"` -- `serde_json::Value::Null` is exactly what an absent `Option`
    // serializes to, and the text table's own cell is checked below.
    assert!(other_obj["name"].is_null());

    let text_out = run_conway(&["sessions", "list"], &fixture);
    assert!(text_out.status.success());
    let text = String::from_utf8(text_out.stdout).expect("utf8 stdout");
    let named_short = &named.to_string()[..8];
    let named_line = text
        .lines()
        .find(|l| l.starts_with(named_short))
        .unwrap_or_else(|| panic!("no row for {named_short} in {text:?}"));
    assert!(
        named_line.contains("daily"),
        "named row must show the name: {named_line:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_resolves_the_target_by_an_existing_name_and_renames_it() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;

    let first = run_conway(&["sessions", "name", &sid.to_string(), "daily"], &fixture);
    assert!(first.status.success());

    // Rename by targeting the EXISTING name, not the id -- `sessions name`
    // resolves `ID` through the same id-or-name grammar `--session`/
    // `--resume` use.
    let renamed = run_conway(&["sessions", "name", "daily", "standup"], &fixture);
    assert!(
        renamed.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&renamed.stderr)
    );

    let list_out = run_conway(&["sessions", "list", "--json"], &fixture);
    let value: Value = serde_json::from_slice(&list_out.stdout).expect("stdout is a JSON array");
    let obj = value.as_array().expect("array")[0]
        .as_object()
        .expect("object");
    assert_eq!(
        obj["name"].as_str(),
        Some("standup"),
        "the session must carry exactly one name at a time: {obj:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_rejects_a_ulid_shaped_name() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;
    let ulid_shaped = SessionId::new().to_string();

    let out = run_conway(
        &["sessions", "name", &sid.to_string(), &ulid_shaped],
        &fixture,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(
        !out.stderr.is_empty(),
        "stderr should explain the ULID-shape refusal"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_rejects_a_name_already_bound_to_a_different_session() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "hi"], &fixture);
    assert!(first.status.success());
    let a = only_session_id(&fixture).await;
    let bind_a = run_conway(&["sessions", "name", &a.to_string(), "daily"], &fixture);
    assert!(bind_a.status.success());

    let second = run_conway(&["-p", "hi"], &fixture);
    assert!(second.status.success());
    let conway = open_conway(&fixture).await;
    let sessions = conway
        .sessions(SessionFilter::default())
        .await
        .expect("list sessions");
    let b = sessions
        .iter()
        .find(|m| m.id != a)
        .expect("second session")
        .id;

    let collide = run_conway(&["sessions", "name", &b.to_string(), "daily"], &fixture);
    assert_eq!(collide.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&collide.stderr).into_owned();
    assert!(
        stderr.contains("daily") && stderr.contains(&a.to_string()),
        "refusal must name the collision (the existing name and which session holds it): \
         {stderr:?}"
    );

    // The refusal must not have mutated the table: `a` still owns `daily`.
    let list_out = run_conway(&["sessions", "list", "--json"], &fixture);
    let value: Value = serde_json::from_slice(&list_out.stdout).expect("stdout is a JSON array");
    let arr = value.as_array().expect("array");
    let a_obj = arr
        .iter()
        .find(|v| v["id"].as_str() == Some(a.to_string().as_str()))
        .expect("session a");
    assert_eq!(a_obj["name"].as_str(), Some("daily"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unname_removes_the_binding_and_unknown_target_exits_2() {
    let mock = MockBackend::start(ok_script()).await;
    let fixture = write_fixture(&mock, 10);
    let created = run_conway(&["-p", "hi"], &fixture);
    assert!(created.status.success());
    let sid = only_session_id(&fixture).await;

    let bind = run_conway(&["sessions", "name", &sid.to_string(), "daily"], &fixture);
    assert!(bind.status.success());

    let unbind = run_conway(&["sessions", "unname", "daily"], &fixture);
    assert!(
        unbind.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&unbind.stderr)
    );

    let list_out = run_conway(&["sessions", "list", "--json"], &fixture);
    let value: Value = serde_json::from_slice(&list_out.stdout).expect("stdout is a JSON array");
    let obj = value.as_array().expect("array")[0]
        .as_object()
        .expect("object");
    assert!(obj["name"].is_null(), "name must be gone: {obj:?}");

    // The session itself is entirely unaffected -- `sessions show` still
    // resolves it by its own (unchanged) id.
    let show_out = run_conway(&["sessions", "show", &sid.to_string(), "--json"], &fixture);
    assert!(show_out.status.success());

    let again = run_conway(&["sessions", "unname", "daily"], &fixture);
    assert_eq!(
        again.status.code(),
        Some(2),
        "unnaming an already-unbound name must be a usage error, not a silent no-op"
    );
}

// ---------------------------------------------------------------------
// --session / --resume / --fork-from accept a name wherever they accept
// an id
// ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_accepts_a_name_and_continues_the_same_transcript() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("noted"), Chunk::Finish("stop")],
        vec![Chunk::Text("you said remember X"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "remember X"], &fixture);
    assert!(first.status.success());
    let sid = only_session_id(&fixture).await;
    let name_out = run_conway(&["sessions", "name", &sid.to_string(), "daily"], &fixture);
    assert!(name_out.status.success());

    let second = run_conway(
        &["-p", "what did I ask you to remember?", "--resume", "daily"],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        second.stdout, b"you said remember X\n",
        "--resume daily must continue the SAME transcript --session daily's id backs"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_unknown_name_exits_2_empty_stdout() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--resume", "no-such-name"], &fixture);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!out.stderr.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fork_from_accepts_a_name_with_a_seq_suffix() {
    let mock = MockBackend::start(Script(vec![
        vec![Chunk::Text("ok"), Chunk::Finish("stop")],
        vec![Chunk::Text("branched"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);

    let first = run_conway(&["-p", "remember the root context"], &fixture);
    assert!(first.status.success());
    let parent = only_session_id(&fixture).await;
    let name_out = run_conway(
        &["sessions", "name", &parent.to_string(), "trunk"],
        &fixture,
    );
    assert!(name_out.status.success());

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
            &format!("trunk@{}", parent_head.0),
        ],
        &fixture,
    );
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(second.stdout, b"branched\n");

    let tree_out = run_conway(&["sessions", "tree", &parent.to_string()], &fixture);
    assert!(tree_out.status.success());
    let tree_text = String::from_utf8_lossy(&tree_out.stdout).into_owned();
    assert_eq!(
        tree_text.lines().count(),
        2,
        "expected the root plus exactly one forked child: {tree_text:?}"
    );
}

// A sanity check that `--session`/`--resume` help text mentions the
// id-or-name grammar, keeping `--help` truthful about what this item added
// -- a light regression guard against the flag doc drifting back to "id
// only" without anyone noticing.
#[test]
fn help_mentions_names_for_session_and_resume() {
    let fixture = common::write_fixture_with("http://127.0.0.1:1/v1", "test-model", 10);
    let out = command(&["--help"], &fixture)
        .output()
        .expect("run conway --help");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        text.contains("--session") && text.contains("--resume"),
        "expected --session/--resume in --help output: {text:?}"
    );
    assert!(
        text.contains("name"),
        "expected --help to mention operator-chosen names somewhere in the \
         --session/--resume/--fork-from descriptions: {text:?}"
    );
}

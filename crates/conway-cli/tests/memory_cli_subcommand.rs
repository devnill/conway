//! Acceptance tests for board item `01M1WVQ36ASXCQMSB7Q5K20P44`: `conway
//! memory list`/`conway memory forget <id>`, the built-in audit/undo
//! surface over `conway.memory`'s store (`crates/conway-cli/src/commands/
//! memory.rs`). Driven against the real compiled `conway` binary, each step
//! its own fresh subprocess against the SAME on-disk `.conway/memory`
//! directory -- mirrors `tests/durable_memory.rs` and `tests/
//! memory_plugin_commands.rs`'s own standard: no stub, a real durable store,
//! genuinely round-tripped across process boundaries.
//!
//! Acceptance 1: after a session calls the real `remember` tool, `conway
//! memory list` shows it with enough detail (id, created, text) to identify
//! it. Acceptance 2: `conway memory forget <id>` removes it, and a
//! subsequent `conway memory list` no longer shows it.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, Fixture};

fn add_plugins_install(fixture: &Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({ "install": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].install");
}

/// The `ID` column of `conway memory list`'s first data row (line 2 -- line
/// 1 is the header).
fn first_listed_id(list_stdout: &str) -> String {
    list_stdout
        .lines()
        .nth(1)
        .expect("a data row after the header")
        .split_whitespace()
        .next()
        .expect("the row's first column is the memory id")
        .to_string()
}

/// The full round trip this item exists to deliver: a model-called
/// `remember` in one process, seen by `conway memory list` in a second,
/// removed by `conway memory forget` in a third, and gone from `conway
/// memory list` in a fourth -- each its own subprocess.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_then_memory_list_then_forget_round_trip_survives_a_restart() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "remember",
                args: serde_json::json!({ "text": "the deploy secret lives in vault" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("remembered"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    // 1. A real session calls the real `remember` tool (a fresh process).
    let remember_out = run_conway(
        &[
            "-p",
            "remember the deploy secret",
            "--allowed-tools",
            "remember",
        ],
        &fixture,
    );
    assert!(
        remember_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&remember_out.stderr)
    );

    // 2. `conway memory list`, in a SECOND, independent process -- must show
    //    the remembered text with enough detail to identify it (id +
    //    created + text, acceptance 1).
    let list_out = run_conway(&["memory", "list"], &fixture);
    assert!(
        list_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list_out.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list_out.stdout).into_owned();
    assert!(
        list_stdout.contains("the deploy secret lives in vault"),
        "`conway memory list` must show what the model remembered: {list_stdout}"
    );
    assert!(
        list_stdout.lines().next().unwrap().contains("ID")
            && list_stdout.lines().next().unwrap().contains("CREATED"),
        "the listing must be a table naming its columns: {list_stdout}"
    );
    let id = first_listed_id(&list_stdout);

    // 3. `conway memory forget <id>`, in a THIRD, independent process.
    let forget_out = run_conway(&["memory", "forget", &id], &fixture);
    assert!(
        forget_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&forget_out.stderr)
    );
    let forget_stdout = String::from_utf8_lossy(&forget_out.stdout);
    assert!(
        forget_stdout.contains("forgot") && forget_stdout.contains(&id),
        "{forget_stdout}"
    );

    // 4. `conway memory list` again, in a FOURTH, independent process --
    //    the removal must persist (acceptance 2).
    let list_after_out = run_conway(&["memory", "list"], &fixture);
    assert!(list_after_out.status.success());
    let list_after_stdout = String::from_utf8_lossy(&list_after_out.stdout);
    assert!(
        list_after_stdout.contains("no memories stored"),
        "the forgotten memory must not survive into a fourth process's listing: \
         {list_after_stdout}"
    );
}

/// `conway memory list` on a store nothing was ever written to reports "no
/// memories stored" rather than an empty table or an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_on_an_empty_store_reports_no_memories_stored() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let out = run_conway(&["memory", "list"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no memories stored"), "{stdout}");
}

/// `--json` emits every field, including the full (untruncated) text, as a
/// JSON array -- proven directly against the real store rather than merely
/// asserting the flag parses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_json_emits_id_created_and_text() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "remember",
                args: serde_json::json!({ "text": "json listing target" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("remembered"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let remember_out = run_conway(
        &["-p", "remember something", "--allowed-tools", "remember"],
        &fixture,
    );
    assert!(remember_out.status.success());

    let out = run_conway(&["memory", "list", "--json"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let arr: serde_json::Value = serde_json::from_str(stdout.trim()).expect("valid JSON array");
    let entries = arr.as_array().expect("a JSON array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["text"], "json listing target");
    assert!(entries[0]["id"].as_str().is_some());
    assert!(entries[0]["created"].as_str().is_some());
}

/// Forgetting an id that parses but names nothing stored is a named usage
/// error (exit 2), never a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_an_unknown_id_is_a_named_usage_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let unknown = conway::MemoryId::new().to_string();
    let out = run_conway(&["memory", "forget", &unknown], &fixture);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no such memory"), "{stderr}");
}

/// A malformed `forget` argument (not a valid `MemoryId`) is a named usage
/// error, not a panic or a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_a_malformed_id_is_a_named_usage_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let out = run_conway(&["memory", "forget", "not-a-memory-id"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("not a valid memory id") && stderr.contains("not-a-memory-id"),
        "{stderr}"
    );
}

/// `conway memory list`/`conway memory forget` work even when
/// `"conway.memory"` was never installed -- they always resolve as a
/// built-in subcommand (unlike `conway conway.memory.list`, which is
/// unknown without the plugin, per `tests/memory_plugin_commands.rs`).
/// Nothing could have been remembered through a plugin never installed, so
/// the honest answer is an empty listing, not an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_without_the_plugin_installed_is_empty_not_an_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = common::write_fixture(&mock, 10);
    // Deliberately no `add_plugins_install` call.

    let out = run_conway(&["memory", "list"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no memories stored"), "{stdout}");
}

//! Integration tests for `conway-plugin-memory`'s operator surface (board
//! item `01M0EMD54BWAVZGYWPXP4S5P1J`): `/conway.memory.list`,
//! `/conway.memory.remember`, `/conway.memory.forget` -- reachable both in
//! the TUI's `/`-prefixed dispatch and, exercised here, as
//! `conway conway.memory.<command>` through clap's external-subcommand
//! path (`commands::plugin::run`). Driven through the REAL shipped
//! `conway-plugin-memory` crate and the real compiled `conway` binary, the
//! same "no stub anywhere in core" standard `plugin_subcommand.rs` and
//! `checkout_and_mask_plugin.rs` already hold `conway-plugin-history` to.
//!
//! **The item's own acceptance criterion, proven directly**: "An operator
//! lists what the agent remembers, adds a note by hand, forgets one by id,
//! and all three survive a restart." Every `run_conway` call here spawns an
//! independent, fresh subprocess -- there is no in-process state to leak
//! between them -- so a memory remembered in one invocation, listed in a
//! second, and forgotten in a third genuinely round-trips through the
//! durable, on-disk `FsMemoryStore` (`conway.memory` selected via
//! `[plugins].install`) rather than merely a `Conway` handle kept alive
//! across the test.

mod common;

use common::{run_conway, write_fixture};

fn add_plugins_install(fixture: &common::Fixture, ids: &[&str]) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({ "install": ids });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].install");
}

/// Absent `[plugins].install`, every `conway.memory.*` command is simply an
/// unknown subcommand -- mirrors `plugin_subcommand.rs`'s identical
/// `conway.history.rewind` case: the plugin (not core) owns this surface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_without_the_plugin_installed() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["conway.memory.list"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conway.memory.list"), "{stderr}");
}

/// Installed but empty, `/conway.memory.list` reports "no memories stored"
/// rather than an empty line or an error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn list_on_an_empty_store_reports_no_memories_stored() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let out = run_conway(&["conway.memory.list"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no memories stored"), "{stdout}");
}

/// The full round trip, each step its own fresh subprocess against the
/// SAME on-disk `.conway/memory` directory -- proving the durable store,
/// not merely a live `Conway` handle, carries the memory across
/// invocations ("survives a restart").
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_list_forget_round_trip_survives_a_restart() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    // 1. Remember a hand-typed note (a fresh process).
    let remember_out = run_conway(
        &["conway.memory.remember", "the deploy secret lives in vault"],
        &fixture,
    );
    assert!(
        remember_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&remember_out.stderr)
    );
    let remember_stdout = String::from_utf8_lossy(&remember_out.stdout);
    assert!(remember_stdout.contains("remembered"), "{remember_stdout}");

    // 2. List, in a SECOND, independent process -- the durable store must
    //    have persisted what the first process wrote.
    let list_out = run_conway(&["conway.memory.list"], &fixture);
    assert!(
        list_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&list_out.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(
        list_stdout.contains("the deploy secret lives in vault"),
        "the remembered note must survive into a second process's listing: {list_stdout}"
    );
    let id = list_stdout
        .split_whitespace()
        .next()
        .expect("the listing's first token is the memory id")
        .to_string();

    // 3. Forget it by the id the SECOND process's listing reported, in a
    //    THIRD, independent process.
    let forget_out = run_conway(&["conway.memory.forget", &id], &fixture);
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

    // 4. List again, in a FOURTH, independent process -- the removal must
    //    likewise have persisted.
    let list_after_out = run_conway(&["conway.memory.list"], &fixture);
    assert!(list_after_out.status.success());
    let list_after_stdout = String::from_utf8_lossy(&list_after_out.stdout);
    assert!(
        list_after_stdout.contains("no memories stored"),
        "the forgotten note must not survive into a fourth process's listing: \
         {list_after_stdout}"
    );
}

/// A malformed `forget` argument surfaces as the real plugin's own
/// `CommandOutcome::Error`, mapped to exit 1 -- distinct from exit 2's "not
/// a recognized subcommand at all" (mirrors `plugin_subcommand.rs`'s
/// identical `conway.history.rewind` case for a malformed seq).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_with_a_malformed_id_surfaces_the_plugins_own_error() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let out = run_conway(&["conway.memory.forget", "not-a-memory-id"], &fixture);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.memory.forget") && stderr.contains("not-a-memory-id"),
        "stderr should carry the real plugin's own usage message: {stderr}"
    );
}

/// Forgetting an id that parses but names nothing stored is likewise a
/// named error, never a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forget_with_an_unknown_id_surfaces_a_named_error() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let unknown = conway::MemoryId::new().to_string();
    let out = run_conway(&["conway.memory.forget", &unknown], &fixture);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no such memory"), "{stderr}");
}

/// `remember` with no text at all is a named error, never a stored empty
/// memory.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remember_with_no_text_is_a_named_error() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    let out = run_conway(&["conway.memory.remember"], &fixture);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conway.memory.remember"), "{stderr}");
}

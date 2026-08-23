//! Integration tests for `conway-plugin-history`'s second and third
//! commands (board item 01KZY8QRAVVVKCRBZ6HAEGW3GG, "`/checkout` and a
//! reachable `ContextMask`"), driven through the REAL shipped
//! `conway-plugin-history` crate and the real compiled `conway` binary --
//! the same "no stub anywhere in core" standard `plugin_subcommand.rs`'s
//! own `/conway.history.rewind` tests already hold this plugin to.
//!
//! **This file's own anchor**: "a `/checkout` test asserting the prior
//! session's file bytes are unchanged" -- the item's own VERIFICATION
//! ANCHOR, taken literally: [`checkout_forks_and_leaves_the_source_
//! sessions_file_bytes_unchanged`] reads the source session's own on-disk
//! `.jsonl` file (`.conway/sessions/<sid>.jsonl`, `conway-session`'s own
//! naming scheme) before and after `conway.history.checkout`, and asserts
//! byte-for-byte equality -- not merely that `Conway::session_head` didn't
//! change (`plugin_cmd.rs`'s own `ForkSession` precedent checks that;
//! this file checks the literal bytes clap's own subprocess wrote).
//! [`checkout_target_is_still_listed`] is the acceptance criterion's other
//! half: `conway sessions list` after a checkout still names the source
//! session.

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

/// `conway-session`'s own on-disk naming scheme (`store.rs::session_path`):
/// `<session.root>/<sid>.jsonl`; the fixture leaves `[session].root`
/// unconfigured, so `session.root` resolves to `common::session_dir`'s
/// central, project-keyed default (board item
/// `01M0QK9GRM8HSNWRAR414TCX42`), not a bare `<fixture>/.conway/sessions`.
fn session_file(fixture: &common::Fixture, sid: &str) -> std::path::PathBuf {
    common::session_dir(fixture).join(format!("{sid}.jsonl"))
}

/// Every PER-SESSION `.jsonl` file in the fixture's sessions dir --
/// deliberately excludes `index.jsonl` (`conway-session`'s own
/// `SessionIndex`, written to the SAME directory, `crates/conway-session/
/// src/index.rs`), which is not a session file and would otherwise silently
/// inflate every count in this file by one.
fn session_files(fixture: &common::Fixture) -> Vec<std::path::PathBuf> {
    let sessions_dir = common::session_dir(fixture);
    std::fs::read_dir(&sessions_dir)
        .expect("read sessions dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "jsonl")
                && p.file_stem().and_then(|s| s.to_str()) != Some("index")
        })
        .collect()
}

/// The checkout report names the SOURCE session (`target`) twice and the
/// CHILD once (this file's own `println!` at the headless dispatch site,
/// `commands/plugin.rs`'s `Checkout` arm) -- so finding "a" session id in
/// stdout is not enough to find the CHILD specifically. This returns the
/// first token that parses as a `SessionId` and is NOT `exclude`.
fn find_other_session_id(stdout: &str, exclude: conway::SessionId) -> conway::SessionId {
    stdout
        .split_whitespace()
        .filter_map(|tok| tok.parse::<conway::SessionId>().ok())
        .find(|sid| *sid != exclude)
        .unwrap_or_else(|| panic!("no session id other than {exclude} in stdout: {stdout:?}"))
}

// -----------------------------------------------------------------------
// /conway.history.checkout
// -----------------------------------------------------------------------

/// Absent `[plugins].install`, `conway.history.checkout` is an unknown
/// subcommand -- mirrors `plugin_subcommand.rs`'s identical `rewind` case,
/// proving the plugin (not core) owns this surface for `checkout` too.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_unknown_without_the_plugin_installed() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let target = conway::SessionId::new();
    let out = run_conway(&["conway.history.checkout", &target.to_string()], &fixture);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conway.history.checkout"), "{stderr}");
}

/// **The verification anchor.** A real turn creates and populates a source
/// session; `conway.history.checkout <source>` forks it and reports a
/// genuinely different child -- and the source session's own `.jsonl`
/// file, read as raw bytes off disk, is byte-for-byte identical before and
/// after.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_forks_and_leaves_the_source_sessions_file_bytes_unchanged() {
    use common::mock_backend::{Chunk, MockBackend, Script};
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("noted"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.history"]);

    let first = run_conway(&["-p", "remember X"], &fixture);
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    // `-p` with no `--session` prints nothing identifying the session id on
    // its own stdout (that is just the assistant's reply) -- so this reads
    // the on-disk store directly, exactly like `continuity.rs`'s own
    // `only_session_id` helper, to discover which file was written.
    let entries = session_files(&fixture);
    assert_eq!(entries.len(), 1, "expected exactly one session file so far");
    let source_path = entries[0].clone();
    let source_sid = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("session file has a stem")
        .to_string();
    let bytes_before = std::fs::read(&source_path).expect("read source session file");
    assert!(
        !bytes_before.is_empty(),
        "source session must have real content"
    );

    let out = run_conway(&["conway.history.checkout", &source_sid], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("checked out") && stdout.contains(&source_sid),
        "stdout should report the checkout outcome: {stdout}"
    );
    let source_sid_parsed: conway::SessionId = source_sid.parse().expect("source_sid is a ULID");
    let child_sid = find_other_session_id(&stdout, source_sid_parsed);
    assert_ne!(
        child_sid.to_string(),
        source_sid,
        "checkout must produce a genuinely different session"
    );

    let bytes_after = std::fs::read(&source_path).expect("re-read source session file");
    assert_eq!(
        bytes_before, bytes_after,
        "the source session's own on-disk bytes must be byte-for-byte unchanged after checkout"
    );

    // The child is real and independently listed.
    let child_path = session_file(&fixture, &child_sid.to_string());
    assert!(
        child_path.is_file(),
        "the checked-out child must have its own session file: {child_path:?}"
    );
}

/// The other half of "the previous session is untouched and still
/// listed": `conway sessions list` after a checkout still names the source
/// session, alongside the new child.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkout_target_is_still_listed() {
    use common::mock_backend::{Chunk, MockBackend, Script};
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("noted"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.history"]);

    let first = run_conway(&["-p", "hello"], &fixture);
    assert!(first.status.success());

    let list_before = run_conway(&["sessions", "list", "--json"], &fixture);
    assert!(list_before.status.success());
    let ids_before = session_ids_from_json_list(&list_before.stdout);
    assert_eq!(ids_before.len(), 1, "expected exactly one session so far");
    let source_sid = ids_before[0].clone();

    let checkout_out = run_conway(&["conway.history.checkout", &source_sid], &fixture);
    assert!(
        checkout_out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&checkout_out.stderr)
    );

    let list_after = run_conway(&["sessions", "list", "--json"], &fixture);
    assert!(list_after.status.success());
    let ids_after = session_ids_from_json_list(&list_after.stdout);
    assert!(
        ids_after.contains(&source_sid),
        "the checked-out-FROM session must still be listed: {ids_after:?}"
    );
    assert!(
        ids_after.len() > ids_before.len(),
        "checkout must add at least one new session (the throwaway CommandCtx session and the \
         forked child): before {ids_before:?}, after {ids_after:?}"
    );
}

fn session_ids_from_json_list(stdout: &[u8]) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_slice(stdout).expect("stdout is JSON");
    value
        .as_array()
        .expect("top-level array")
        .iter()
        .map(|obj| {
            obj.as_object()
                .expect("element is an object")
                .get("id")
                .expect("element has an id")
                .as_str()
                .expect("id is a string")
                .to_string()
        })
        .collect()
}

// -----------------------------------------------------------------------
// /conway.history.mask
// -----------------------------------------------------------------------

/// Absent `[plugins].install`, `conway.history.mask` is likewise unknown.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mask_unknown_without_the_plugin_installed() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["conway.history.mask", "0"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("conway.history.mask"), "{stderr}");
}

/// Installed, `conway.history.mask 0` runs for real against the fresh,
/// prompt-less session `commands::plugin::run` starts (module doc of that
/// function) -- reports success and appends a real `context_mask` record,
/// observable as a new, larger session file (an append, not a rewrite: the
/// file only grows).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mask_installed_appends_a_real_record() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.history"]);

    let out = run_conway(&["conway.history.mask", "0"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("masked") && stdout.contains("seq 0"),
        "stdout should report the real mask outcome: {stdout}"
    );

    let entries = session_files(&fixture);
    assert_eq!(
        entries.len(),
        1,
        "the mask command's own fresh session file"
    );
    let bytes = std::fs::read(&entries[0]).expect("read session file");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("context_mask"),
        "the session file must contain a real, persisted context_mask record: {text}"
    );
}

/// A malformed argument surfaces as the real plugin's own
/// `CommandOutcome::Error`, mapped to exit 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mask_installed_with_a_malformed_argument_surfaces_the_plugins_own_error() {
    use common::mock_backend::{MockBackend, Script};
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.history"]);

    let out = run_conway(&["conway.history.mask", "not-a-number"], &fixture);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.history.mask") && stderr.contains("not-a-number"),
        "stderr should carry the real plugin's own usage message: {stderr}"
    );
}

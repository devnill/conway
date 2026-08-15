//! Integration tests for plugin-contributed subcommands (board item
//! 01M00QG7GHHVDKRC0J87NH0FNR's second half): a plugin can add a slash
//! command to the TUI (`conway-plugin-history`'s
//! `/conway.history.rewind`), but before this item could not add a
//! subcommand to the binary at all. `cli::Command::External` (clap's own
//! `external_subcommand` idiom) now catches anything that is not a
//! built-in subcommand and `commands::plugin::run` resolves it against
//! every installed plugin's own `commands()`, reusing `tui::commands::
//! CommandRegistry::build` -- the SAME resolver the TUI's `/`-prefixed
//! dispatch already uses -- rather than a second copy of that logic.
//!
//! Driven through the REAL shipped `conway-plugin-history` crate (not a
//! local fixture plugin), the same "no stub anywhere in core" standard
//! `tui/app/plugin_cmd.rs`'s own real-plugin test applies: absent
//! `[plugins].install`, the command is simply unknown; installed, it runs
//! for real against the real on-disk session store.

mod common;

use common::mock_backend::{MockBackend, Script};
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

/// Absent `[plugins].install`, `conway.history.rewind` is simply an
/// unknown subcommand -- exit 2, naming the word typed, exactly like any
/// other unrecognized subcommand. No special case anywhere: the built-in
/// `sessions`/`routes` set and the plugin set are resolved through the
/// identical `Command::External` fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_without_the_plugin_installed_exits_usage_and_names_the_word() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no add_plugins_install call.

    let out = run_conway(&["conway.history.rewind", "0"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.history.rewind"),
        "stderr must name the unresolved subcommand: {stderr}"
    );
}

/// Installed, `conway.history.rewind <seq>` runs for real: it forks a
/// fresh, prompt-less session (this dispatch path's own "no live session
/// yet" starting point -- `commands::plugin::run`'s module doc) at seq 0
/// (its only legal seq, an empty log's own head) and reports the child's
/// session id on stdout -- observable, real output from the real plugin
/// crate's `Command::invoke`, not a parsed-flag assertion.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_forks_a_real_session_and_reports_the_child_id() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.history"]);

    let out = run_conway(&["conway.history.rewind", "0"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("forked session") && stdout.contains("at seq 0"),
        "stdout should report the real fork outcome: {stdout}"
    );

    // The reported child id must parse as a real `SessionId` and must
    // actually be resolvable through the same on-disk store -- not just a
    // string that looks right.
    let child_id_str = stdout
        .split_whitespace()
        .find_map(|tok| tok.parse::<conway::SessionId>().ok())
        .expect("stdout must contain a parseable session id");

    let conway_lib = open_conway(&fixture).await;
    let handle = conway_lib
        .resume(child_id_str)
        .await
        .expect("the reported child session must genuinely exist in the store");
    assert_eq!(handle.id(), child_id_str);

    // Never reached a model: this dispatch path only forks, never prompts.
    assert!(mock.requests().is_empty());
}

/// A malformed argument surfaces as the real plugin's own
/// `CommandOutcome::Error` (`conway-plugin-history`'s own usage message),
/// mapped to exit 1 -- distinct from exit 2's "not a recognized subcommand
/// at all".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_with_a_malformed_argument_surfaces_the_plugins_own_error() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.history"]);

    let out = run_conway(&["conway.history.rewind", "not-a-number"], &fixture);

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.history.rewind") && stderr.contains("not-a-number"),
        "stderr should carry the real plugin's own usage message: {stderr}"
    );
}

/// A totally unrelated word (no plugin-namespace shape at all) is the same
/// exit-2 unknown-subcommand outcome as a plugin-shaped-but-unregistered
/// one above -- `commands::plugin::run` treats every unresolved name
/// uniformly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_totally_unrelated_word_is_also_an_unknown_subcommand() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["not-a-real-subcommand"], &fixture);

    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not-a-real-subcommand"), "{stderr}");
}

/// Sanity: the built-in subcommands (`sessions`, `routes`) still resolve
/// through clap's own static match arms, never falling through to
/// `Command::External`/plugin resolution -- unaffected by this item.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn built_in_subcommands_are_unaffected() {
    let mock = MockBackend::start(Script(vec![])).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["sessions", "list"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

async fn open_conway(fixture: &common::Fixture) -> conway::Conway {
    use std::sync::Arc;

    use conway::config::CliOverrides;
    use conway::gates::AllowListGate;
    use conway::{ConwayBuilder, PermissionGate};

    let gate: Arc<dyn PermissionGate> = Arc::new(AllowListGate::new(Vec::new(), Vec::new()));
    ConwayBuilder::from_config_only(&fixture.config_path)
        .expect("load fixture config")
        .with_cli_overrides(CliOverrides {
            cwd: Some(fixture.dir.path().to_path_buf()),
            ..Default::default()
        })
        .with_permission_gate(gate)
        .with_backend_factory(Arc::new(conway_plugin_backends::OpenAiCompatBackendFactory))
        .build()
        .expect("build conway against the fixture's own store")
}

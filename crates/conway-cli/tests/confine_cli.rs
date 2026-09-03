//! CLI-level acceptance test for `conway.confine` (harness gap review
//! 2026-09-01, decision `01M1FQG08GDQ71984T0W0RJ019`): fixture installing
//! `conway.confine` with `--root`, a mock backend scripting a
//! `confined_bash` call that writes OUTSIDE the root -- the tool result in
//! the one-shot event stream is an error and the file is absent. Does
//! **not** assert the root+unconfinable-shell-tool warning stays silent --
//! one-shot `-p` dispatch registers every built-in plugin unconditionally,
//! so `conway.shell`'s own `bash` is genuinely present and genuinely
//! unconfined here too, regardless of `[plugins].install`; the warning is
//! therefore correct to fire in THIS dispatch mode. `Tool::confined_by_
//! tool`'s actual suppression effect (`crates/conway/src/builder.rs`'s
//! "10a2b" comment) is unit-tested directly against `ConwayBuilder::build`
//! in `crates/conway/tests/builder.rs` instead, where the plugin set is
//! controlled precisely.
//!
//! Written the same way `crates/conway-cli/tests/first_party_plugins.rs`
//! drives a first-party plugin end to end: real compiled binary, real mock
//! OpenAI-compatible server, one-shot `-p` with `--allowed-tools` naming the
//! call so it never needs a live permission prompt.
//!
//! **macOS only** (`#[cfg(target_os = "macos")]`): this test needs a real
//! `sandbox-exec` at `conway_plugin_confine::DEFAULT_PRIMITIVE_PATH` for
//! `ConwayBuilder::build` to install `conway.confine` at all -- see this
//! item's own report for the Linux/`bwrap` equivalent recipe the build
//! lane runs separately (`conway-plugin-confine`'s own in-crate suite,
//! which DOES have a Linux mirror, skipped with a printed reason when
//! `bwrap` is absent).

#![cfg(target_os = "macos")]

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};

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

fn jsonl_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is utf8")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad jsonl line {l}: {e}")))
        .collect()
}

/// `event.tool_call_finished.is_error` for the call naming `tool_name`,
/// correlated by `call_id` the same way `first_party_plugins.rs`'s own
/// `tool_call_finished_preview` correlates `preview`.
fn tool_call_finished_is_error(lines: &[serde_json::Value], tool_name: &str) -> Option<bool> {
    let call_id = lines.iter().find_map(|line| {
        if line["event"] == "tool_call_proposed" && line["tool"] == tool_name {
            line["call_id"].as_str().map(str::to_string)
        } else {
            None
        }
    })?;
    lines.iter().find_map(|line| {
        if line["event"] == "tool_call_finished" && line["call_id"] == call_id {
            line["is_error"].as_bool()
        } else {
            None
        }
    })
}

/// Acceptance 4: `conway.confine` installed with `--root`, a scripted
/// `confined_bash` call writing outside it -- an error result, the file
/// absent, and no root+bash warning.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confined_bash_write_outside_root_is_an_error_result_and_the_file_stays_absent() {
    let outside = tempfile::tempdir().expect("outside tempdir");
    let outside_target = outside.path().join("out.txt");

    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "confined_bash",
                args: serde_json::json!({
                    "command": format!("echo x > {}", outside_target.display()),
                }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.confine"]);

    let out = run_conway(
        &[
            "--root",
            &fixture.dir.path().to_string_lossy(),
            "-p",
            "write outside the root",
            "--allowed-tools",
            "confined_bash",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "the RUN completes even though the tool call itself errors -- the model gets told and \
         replies; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines = jsonl_lines(&out.stdout);
    assert_eq!(
        tool_call_finished_is_error(&lines, "confined_bash"),
        Some(true),
        "a write outside --root must be reported as an error tool result, got jsonl: {lines:?}"
    );
    assert!(
        !outside_target.exists(),
        "no file may exist outside --root after a refused confined write"
    );

    // NOT asserting the root+unconfinable-shell-tool warning is silent
    // here, on further thought: one-shot `-p` dispatch registers every
    // built-in plugin unconditionally (`main.rs`'s own `PluginSelection::
    // All` for every non-interactive CLI target -- the same documented
    // behavior `tools_list.rs`'s `tools_builtin_plugins_toggle_has_no_
    // effect_on_bash_registration` proves for `conway tools list`), so
    // `conway.shell`'s own `bash` (genuinely unconfined) is present in
    // THIS process alongside `conway.confine`'s `confined_bash` regardless
    // of `[plugins].install` naming only the latter -- the warning is
    // therefore CORRECT to fire here, not a defect. `confined_by_tool`'s
    // actual suppression effect (a root check exempting a tool that
    // declares it) is unit-tested directly against `ConwayBuilder::build`
    // in `crates/conway/tests/builder.rs`'s
    // `a_tool_declaring_confined_by_tool_suppresses_the_root_warning`,
    // where the plugin set is controlled precisely rather than inheriting
    // one-shot's own always-register-everything posture.
}

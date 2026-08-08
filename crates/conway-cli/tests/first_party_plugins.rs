//! CLI-level acceptance test for the first-party plugin tier's install
//! mechanism (board item 01KZDC3JQ7W4DY1MG6MBCVB2DV): `[plugins].install`
//! in `settings.json`, resolved by `conway-cli`'s own
//! `first_party_plugins::install` before `ConwayBuilder::build`, against the
//! REAL compiled `conway` binary and a mock OpenAI-compatible server -- no
//! unit test of the resolution function substitutes for this (P-15: a unit
//! test is not a liveness test).
//!
//! One-shot (`-p`) is the mode driven here; the TUI is not (no headless TUI
//! driver exists in this suite). This is still full, not partial, coverage
//! of the "every mode reachable" requirement (C-03/P-8): `conway-cli`'s
//! `main::build_conway` is the SINGLE choke point both the TUI and one-shot
//! dispatch targets call (see that function's own doc comment: `is_tui`
//! only affects the *built-in* selection, never the first-party-plugin
//! resolution immediately below it), so a real run through either one
//! exercises the identical `first_party_plugins::install` call the other
//! one would make from the same config. `crates/conway-plugin-skeleton`'s
//! own `tests/skeleton_end_to_end.rs` covers the library-embedder path the
//! same capability takes when no binary is involved at all.
//!
//! **The VERIFICATION ANCHOR pair asserts on the announced tool set** --
//! the exact wording the board item's acceptance criterion uses -- read
//! straight off the real wire request `MockBackend` received
//! (`request["tools"]`, the identical field `interactive_tools.rs`'s own
//! `announced_names` helper reads at the library level). This sidesteps a
//! real wrinkle: a model that actually tries to CALL a tool the request
//! never announced trips `conway-backends`' own streaming-tool-call
//! validation (`dialect: "openai"` defaults to `Streaming{validated:true}`)
//! as a transport-level bad-request, before the runtime's own
//! "unknown tool" resolution would even see it -- a different, unrelated
//! failure this test does not exist to cover. The separate
//! `skeleton_tool_is_callable_from_one_shot_once_installed` test below
//! drives a real call, always with the plugin installed (so the call is
//! legitimate), to prove invocation rather than mere announcement.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture};

/// [`common::write_fixture`] renders the shared `conway.json.tmpl` (backend,
/// role, `max_steps`) with no `[plugins]` section at all -- proving the
/// absent-by-default half needs nothing added, `PluginsConfig`'s own
/// `#[serde(default)]` already covers it (`config::schema`'s doc). This
/// helper adds exactly one key to that rendered file: `{"plugins":
/// {"install": [...]}}`, leaving everything else the shared fixture already
/// established untouched.
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

/// The sorted tool names named in the LAST `/chat/completions` request the
/// mock received -- the wire-level "announced tool set", mirroring
/// `conway/tests/interactive_tools.rs`'s own `announced_names` helper one
/// layer down (a `GenerateRequest.tools` field there; the JSON `tools[].
/// function.name` array an OpenAI-compatible request actually sends over
/// the wire here).
fn announced_names(request: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = request["tools"]
        .as_array()
        .expect("request must carry a tools array")
        .iter()
        .map(|t| {
            t["function"]["name"]
                .as_str()
                .expect("each tool entry names itself")
                .to_string()
        })
        .collect();
    names.sort();
    names
}

fn jsonl_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is utf8")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad jsonl line {l}: {e}")))
        .collect()
}

/// The `tool_call_finished.preview` (first 200 chars of the tool's own
/// reply text -- `conway-runtime`'s `preview_text` contract) for the call
/// naming `tool_name` in `event.tool_call_proposed`, correlated by
/// `call_id`.
fn tool_call_finished_preview(lines: &[serde_json::Value], tool_name: &str) -> Option<String> {
    let call_id = lines.iter().find_map(|line| {
        if line["event"] == "tool_call_proposed" && line["tool"] == tool_name {
            line["call_id"].as_str().map(str::to_string)
        } else {
            None
        }
    })?;
    lines.iter().find_map(|line| {
        if line["event"] == "tool_call_finished" && line["call_id"] == call_id {
            line["preview"].as_str().map(str::to_string)
        } else {
            None
        }
    })
}

/// VERIFICATION ANCHOR, half 1: absent by default. With no
/// `[plugins].install`, the announced tool set the real binary sends over
/// the wire never names `skeleton_ping`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skeleton_tool_is_absent_from_the_announced_set_without_plugins_install() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no `add_plugins_install` call: the rendered config has
    // no `[plugins]` section at all.

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        !names.iter().any(|n| n == "skeleton_ping"),
        "with no [plugins].install, 'skeleton_ping' must not be in the announced tool set, \
         got {names:?}"
    );
}

/// VERIFICATION ANCHOR, half 2: present once installed. The same request
/// shape, differing only by `[plugins].install`, now announces
/// `skeleton_ping` alongside every built-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skeleton_tool_is_present_in_the_announced_set_once_installed() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.plugin_skeleton"]);

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let requests = mock.requests();
    let names = announced_names(
        requests
            .last()
            .expect("mock must have received one request"),
    );
    assert!(
        names.iter().any(|n| n == "skeleton_ping"),
        "with [plugins].install = [\"conway.plugin_skeleton\"], 'skeleton_ping' must be in the \
         announced tool set, got {names:?}"
    );
}

/// Beyond mere announcement: once installed, the tool actually dispatches
/// through the real compiled binary to this plugin's own `Tool::invoke`,
/// and its exact reply text is observable in the finished event's preview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skeleton_tool_is_callable_from_one_shot_once_installed() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "skeleton_ping",
                args: serde_json::json!({ "message": "hi" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.plugin_skeleton"]);

    let out = run_conway(
        &[
            "-p",
            "ping it",
            "--allowed-tools",
            "skeleton_ping",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines = jsonl_lines(&out.stdout);
    assert_eq!(
        tool_call_finished_preview(&lines, "skeleton_ping").as_deref(),
        Some("skeleton pong: hi"),
        "the real plugin's own Tool::invoke must have produced this exact reply, dispatched \
         through the real compiled binary -- got jsonl: {lines:?}"
    );
}

/// An id in `[plugins].install` this binary does not link is a hard config
/// error (GP-14), never a silent no-op.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_plugins_install_id_is_a_hard_error() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.totally_unknown"]);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "an unrecognized plugins.install id must fail the run, not silently ignore it"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.totally_unknown"),
        "the error must name the offending id, got stderr: {stderr}"
    );
    // Board item 01KZFC2MD1FVNA674YJ9A19T8E: the message widened to list
    // linked router factory ids alongside linked plugin ids, resolved
    // against `[plugins].install` in the same pass.
    assert!(
        stderr.contains("conway.plugin_skeleton"),
        "the error must list the linked first-party plugin ids, got stderr: {stderr}"
    );
    assert!(
        stderr.contains("linked router factories"),
        "the error must also list linked router factory ids (empty today, honestly -- see \
         first_party_plugins::router_bundle's own doc), got stderr: {stderr}"
    );
}

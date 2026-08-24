//! CLI-level acceptance test for the subprocess plugin host (board item
//! `01KZY8PATND84AKY0J376E3DWV`): `[plugins].subprocess[]` in
//! `settings.json`, resolved by `conway-cli`'s own
//! `subprocess_plugins::install` before `ConwayBuilder::build`, against the
//! REAL compiled `conway` binary and a mock OpenAI-compatible server -- the
//! same "a unit test is not a liveness test" posture
//! `tests/first_party_plugins.rs` states for the in-process tier, applied
//! here to the out-of-process one.
//!
//! **This file is the item's own VERIFICATION ANCHOR, executed at the
//! layer that makes it literal.** The item's acceptance text: "A conway
//! binary built BEFORE the plugin exists loads and runs that plugin after
//! a settings.json change only... demonstrated by building the binary
//! first, then authoring the plugin, then running -- in that order." That
//! ordering is exactly what [`common::run_conway`] gives for free:
//! `assert_cmd::cargo::cargo_bin("conway")` resolves the binary `cargo
//! test` already finished compiling before this test process ever started,
//! and `greet_script_path` below writes the fixture PYTHON plugin -- never
//! compiled, never linked, no Rust involved at all -- into a fresh temp
//! dir at THIS TEST'S OWN runtime. The binary genuinely had no idea
//! `greet` existed when it was built.
//!
//! `tool_is_absent_without_a_plugins_subprocess_entry` /
//! `tool_is_present_once_named_in_plugins_subprocess` are the positive/
//! negative announcement pair, mirroring `first_party_plugins.rs`'s own
//! `skeleton_tool_is_{absent,present}_...` shape exactly.
//! `tool_is_callable_from_one_shot_once_configured` is the VERIFICATION
//! ANCHOR's OWN "calls its tool through a real agent turn" proof, driven
//! through the real binary rather than a library-level `Conway` (that
//! proof already exists, separately, in `crates/conway-plugin-subprocess/
//! tests/end_to_end.rs`). `removing_the_plugins_subprocess_entry_removes_
//! the_tool` is the item's own "shown to fail when the `[plugins]` entry
//! naming it is removed" clause, literally: the SAME rendered fixture, the
//! entry added then removed, asserting the tool disappears again.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture};

/// The full-featured fixture, duplicated (not shared via `common`) from
/// `crates/conway-plugin-subprocess/tests/common/mod.rs`'s own
/// `GREET_PLUGIN` -- a deliberate, small duplication across two different
/// test BINARIES (this crate's integration tests cannot depend on another
/// crate's `tests/` directory) rather than a shared library crate neither
/// suite otherwise needs. Declares one tool, `greet`, replying
/// `"hello, <name>"`.
const GREET_PLUGIN_PY: &str = r#"#!/usr/bin/env python3
import sys, json

req = json.loads(sys.stdin.read())
op = req.get("op")

if op == "tool.spec/1":
    print(json.dumps({
        "id": "acme.greet",
        "version": "0.1.0",
        "tools": [{
            "name": "greet",
            "description": "Greets the caller by name.",
            "schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
            },
            "category": "read",
            "permission": "safe",
        }],
    }))
elif op == "tool/1":
    tool = req.get("tool")
    args = req.get("arguments", {})
    if tool == "greet":
        name = args.get("name", "")
        print(json.dumps({
            "ok": True,
            "blocks": [{"type": "text", "text": f"hello, {name}"}],
            "is_error": False,
        }))
    else:
        print(json.dumps({
            "ok": False,
            "error": {"kind": "invalid_arguments", "detail": f"unknown tool {tool}"},
        }))
else:
    print(json.dumps({"ok": False, "error": {"kind": "internal", "detail": f"unknown op {op}"}}))
"#;

/// Writes [`GREET_PLUGIN_PY`] into `fixture`'s own temp dir (the SAME
/// directory `write_fixture` already rendered `conway.json` into) and
/// returns its absolute path, executable bit set.
fn write_greet_script(fixture: &common::Fixture) -> std::path::PathBuf {
    let path = fixture.dir.path().join("greet_plugin.py");
    std::fs::write(&path, GREET_PLUGIN_PY).expect("write greet_plugin.py fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x greet_plugin.py");
    }
    path
}

/// Executes `path` once, with stdin closed, and discards everything about
/// the result -- before returning, so the REAL run below (which re-execs
/// this SAME freshly-written, freshly-chmod'd script through the compiled
/// `conway` binary's own subprocess plugin host) pays the "first exec of a
/// fresh file" OS-side tax OUTSIDE `[plugins].subprocess[].timeout_ms`'s
/// clock, not inside it (board item `01M09MPZ9C188AHNBKWEJ3CEQA`; see
/// `conway-plugin-subprocess`'s `tests/common/mod.rs::warm` for the full
/// measurement). [`GREET_PLUGIN_PY`] reads stdin then answers unconditionally
/// -- it never hangs on empty input, so warming it is safe.
async fn warm(path: &std::path::Path) {
    let child = tokio::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        let _ = child.wait().await;
    }
}

/// Rewrites `fixture`'s rendered `conway.json` to add exactly one
/// `[plugins].subprocess[]` entry naming `script` -- the out-of-process
/// sibling of `first_party_plugins.rs`'s own `add_plugins_install`.
fn add_plugins_subprocess(fixture: &common::Fixture, id: &str, script: &std::path::Path) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    value["plugins"] = serde_json::json!({
        "subprocess": [{
            "id": id,
            "command": [script.display().to_string()],
            // The crate default (`conway_plugin_subprocess::DEFAULT_TIMEOUT_MS`,
            // 5000ms), NOT a raised number. This was previously 8000ms with a
            // comment blaming "this suite's own concurrent process load" --
            // that was papering over the real cause: a freshly-written,
            // freshly-chmod'd script's first exec can itself cost seconds at
            // ~0% CPU regardless of load (board item
            // `01M09MPZ9C188AHNBKWEJ3CEQA`; see `conway-plugin-subprocess`'s
            // `tests/common/mod.rs::warm` for the measurement). Every caller
            // of `write_greet_script` now calls `warm` on the SAME script
            // first, which pays that tax outside this deadline's clock, so
            // 5000ms -- already generous for a warm interpreter plus the
            // compiled binary's own startup -- no longer needs padding. Do
            // not raise this back up without first checking `warm` is still
            // being called.
            "timeout_ms": 5_000,
        }],
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins].subprocess");
}

/// Removes any `[plugins]` section entirely -- the literal "the entry
/// naming it is removed" the item's own VERIFICATION ANCHOR asks for.
fn remove_plugins_section(fixture: &common::Fixture) {
    let raw = std::fs::read_to_string(&fixture.config_path).expect("read rendered conway.json");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("parse conway.json");
    if let Some(obj) = value.as_object_mut() {
        obj.remove("plugins");
    }
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize conway.json"),
    )
    .expect("rewrite conway.json with [plugins] removed");
}

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
/// `[plugins].subprocess`, the announced tool set never names `greet`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_is_absent_without_a_plugins_subprocess_entry() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    // Deliberately no `add_plugins_subprocess` call: the rendered config
    // has no `[plugins]` section at all.

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
        !names.iter().any(|n| n == "greet"),
        "with no [plugins].subprocess, 'greet' must not be in the announced tool set, got \
         {names:?}"
    );
}

/// VERIFICATION ANCHOR, half 2: present once configured. The identical
/// request shape, differing only by `[plugins].subprocess`, now announces
/// `greet` -- a tool the compiled binary was never built knowing about.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_is_present_once_named_in_plugins_subprocess() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let script = write_greet_script(&fixture);
    warm(&script).await;
    add_plugins_subprocess(&fixture, "acme-greet", &script);

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
        names.iter().any(|n| n == "greet"),
        "with a [plugins].subprocess entry naming the real fixture script, 'greet' must be in \
         the announced tool set, got {names:?}"
    );
}

/// Beyond mere announcement: the real compiled binary dispatches through
/// `SubprocessTool::invoke`, which re-spawns the REAL fixture process for
/// the call, and the exact reply text it produced is observable in the
/// finished event's preview.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_is_callable_from_one_shot_once_configured() {
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "greet",
                args: serde_json::json!({ "name": "world" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let script = write_greet_script(&fixture);
    warm(&script).await;
    add_plugins_subprocess(&fixture, "acme-greet", &script);

    let out = run_conway(
        &[
            "-p",
            "greet the world",
            "--allowed-tools",
            "greet",
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
        tool_call_finished_preview(&lines, "greet").as_deref(),
        Some("hello, world"),
        "the real subprocess's own tool/1 reply must reach the finished event's preview, \
         dispatched through the real compiled binary -- got jsonl: {lines:?}"
    );
}

/// **The item's own VERIFICATION ANCHOR, literally: "shown to fail when
/// the `[plugins]` entry naming it is removed."** The identical rendered
/// fixture and script as the two tests above -- entry added, tool present;
/// entry removed, tool absent again, proved on the SAME binary and SAME
/// script without recompiling anything in between.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn removing_the_plugins_subprocess_entry_removes_the_tool() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("hi back"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    let script = write_greet_script(&fixture);
    warm(&script).await;
    add_plugins_subprocess(&fixture, "acme-greet", &script);

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(out.status.success());
    let requests = mock.requests();
    let names = announced_names(requests.last().expect("one request"));
    assert!(
        names.iter().any(|n| n == "greet"),
        "precondition: the entry must announce the tool before it is removed, got {names:?}"
    );

    remove_plugins_section(&fixture);

    let out = run_conway(&["-p", "hi"], &fixture);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = mock.requests();
    let names = announced_names(requests.last().expect("second request"));
    assert!(
        !names.iter().any(|n| n == "greet"),
        "once the [plugins] entry is removed, 'greet' must no longer be announced, got \
         {names:?}"
    );
}

/// A subprocess plugin that fails to spawn (an unresolvable command) fails
/// the WHOLE run with a named error, never a silent zero-tool
/// registration -- the out-of-process tier's own version of
/// `first_party_plugins.rs`'s `unknown_plugins_install_id_is_a_hard_error`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unspawnable_plugins_subprocess_entry_is_a_hard_error() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = write_fixture(&mock, 10);
    add_plugins_subprocess(
        &fixture,
        "broken-plugin",
        std::path::Path::new("/nonexistent/path/does-not-exist-conway-test"),
    );

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "an unspawnable [plugins].subprocess entry must fail the run, not silently ignore it"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("broken-plugin"),
        "the error must name the offending entry's own id, got stderr: {stderr}"
    );
}

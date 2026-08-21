//! Acceptance test for board item `01M09V3S2AQYB2VK6MANFRH1JM`: a memory
//! `remember`ed in one CLI process must be recalled by `list_memories` in a
//! LATER, SEPARATE process -- the actual property this item exists to
//! deliver, demonstrated against the real compiled `conway` binary rather
//! than asserted at the unit level (`first_party_plugins.rs`'s own
//! `resolve_memory_store_opens_the_durable_store_when_selected_and_it_persists`
//! covers the narrower "the right root is durable" claim one layer down).
//!
//! Two SEPARATE `Command::output()` invocations of the real binary
//! (`common::run_conway`), sharing only the SAME on-disk fixture directory
//! (`fixture.dir`, kept alive across both calls) -- exactly what two
//! ordinary `conway` invocations from the same shell cwd would share, and
//! nothing more. One `MockBackend` instance backs both calls (a shared
//! backend socket is not the property under test; the on-disk memory
//! directory is), scripted with both processes' turns in the order they
//! will be requested.

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

fn jsonl_lines(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8(stdout.to_vec())
        .expect("stdout is utf8")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad jsonl line {l}: {e}")))
        .collect()
}

/// The `tool_call_finished.preview` for the call naming `tool_name` in
/// `event.tool_call_proposed`, correlated by `call_id` -- mirrors
/// `tests/first_party_plugins.rs`'s identical helper.
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

/// Acceptance criterion 1: recall across a process boundary.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_memory_remembered_in_one_process_is_recalled_in_a_later_separate_process() {
    // One mock server, scripted with BOTH processes' turns, in the order
    // they will arrive: process A's `remember` tool call + its follow-up
    // text turn, then process B's `list_memories` tool call + its follow-up
    // text turn.
    let mock = MockBackend::start(Script(vec![
        vec![
            Chunk::ToolCall {
                name: "remember",
                args: serde_json::json!({ "text": "the launch code is DURABLE-42" }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("remembered"), Chunk::Finish("stop")],
        vec![
            Chunk::ToolCall {
                name: "list_memories",
                args: serde_json::json!({}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("recalled"), Chunk::Finish("stop")],
    ]))
    .await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    // Process A: a fresh `conway` invocation that remembers a distinctive
    // string via the real `remember` tool.
    let out_a = run_conway(
        &[
            "-p",
            "remember the launch code",
            "--allowed-tools",
            "remember,list_memories",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );
    assert!(
        out_a.status.success(),
        "process A (remember) must succeed -- stderr: {}",
        String::from_utf8_lossy(&out_a.stderr)
    );
    let lines_a = jsonl_lines(&out_a.stdout);
    let remember_preview = tool_call_finished_preview(&lines_a, "remember")
        .expect("process A must have actually called the real `remember` tool");
    assert!(
        remember_preview.starts_with("remembered (id:"),
        "the real RememberTool::invoke must have produced this exact reply, got: \
         {remember_preview}"
    );

    // Process B: a WHOLLY SEPARATE `conway` invocation (new process, new
    // in-process `first_party_plugins::install` call, new `Conway`) against
    // the SAME fixture directory. If memory only survived in-process
    // (`InMemoryMemoryStore`, this item's own starting defect), this would
    // list nothing.
    let out_b = run_conway(
        &[
            "-p",
            "what do you remember",
            "--allowed-tools",
            "remember,list_memories",
            "--output-format",
            "jsonl",
        ],
        &fixture,
    );
    assert!(
        out_b.status.success(),
        "process B (list_memories) must succeed -- stderr: {}",
        String::from_utf8_lossy(&out_b.stderr)
    );
    let lines_b = jsonl_lines(&out_b.stdout);
    let list_preview = tool_call_finished_preview(&lines_b, "list_memories")
        .expect("process B must have actually called the real `list_memories` tool");
    assert!(
        list_preview.contains("the launch code is DURABLE-42"),
        "a memory remembered by process A must be recalled by process B, a later, separate \
         `conway` process sharing only the on-disk fixture directory -- got list_memories \
         preview: {list_preview}"
    );
}

/// The failure posture (this item's third acceptance criterion): an
/// operator who names `conway.memory` in `[plugins].install` but whose
/// memory directory cannot be opened gets a LOUD, non-starting failure, not
/// a silent fallback to a non-durable store. Simulated by pre-creating
/// `.conway/memory` as a FILE (not a directory) at the fixture's cwd, so
/// `FsMemoryStore::open`'s own `create_dir_all(root/memories)` fails with a
/// real filesystem error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unopenable_memory_directory_fails_the_process_closed_not_silently() {
    let mock = MockBackend::start(Script(vec![vec![
        Chunk::Text("unreachable"),
        Chunk::Finish("stop"),
    ]]))
    .await;
    let fixture = common::write_fixture(&mock, 10);
    add_plugins_install(&fixture, &["conway.memory"]);

    // `.conway` already exists (write_fixture creates it for models.json);
    // make `.conway/memory` a plain FILE so `create_dir_all("<...>/memory/
    // memories")` cannot succeed -- `NotADirectory` on every real OS this
    // suite runs on.
    std::fs::write(
        fixture.dir.path().join(".conway").join("memory"),
        b"not a dir",
    )
    .expect("create a file where the memory store root would go");

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        !out.status.success(),
        "conway.memory selected with an unopenable memory directory must fail to start, not \
         silently fall back to a non-durable store -- stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("conway.memory") && stderr.contains("memory store"),
        "the failure must be visible and name what failed, got stderr: {stderr}"
    );
}

//! CLI-level acceptance test for board item `01M0XBZNBPXEESX8VNTJDKNG0J`:
//! naming a directory in `[plugins].claude_compat[]` gets its MAPPED
//! `hooks/hooks.json` rules DISPATCHING through the real compiled `conway`
//! binary -- not merely reported. Mirrors `tests/hook_runner_wiring.rs`'s
//! own "break-the-guard" proof shape exactly (a translated `pre_tool_use`
//! rule whose command fails, denying a real `bash` call via
//! `ProcessHookRunner`'s fail-closed behavior on a nonzero exit), applied to
//! a rule that arrived via CLAUDE COMPAT TRANSLATION rather than an
//! operator-authored `[hooks].rules[]` entry -- the exact gap
//! `crates/conway-cli/src/claude_compat_plugins.rs`'s own unit tests (in-
//! process, against a bare `ConwayBuilder`) cannot close on their own: this
//! file is the proof that the SAME compiled binary that used to wire only
//! the MCP half now reaches the hook half too, end to end.
//!
//! No `.mcp.json` in the fixture plugin directory -- this file's own scope
//! is the hook half only; `tests/subprocess_plugins.rs`/`mcp_plugins`-
//! adjacent coverage already proves the MCP half elsewhere, and
//! `claude_compat_plugins.rs`'s own in-process tests already prove an
//! empty `.mcp.json`/absent-`.mcp.json` directory is a true no-op for that
//! half.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway_core::content::ContentBlock;
use conway_core::log::LogRecord;

/// This fixture's own translated hook id -- the exact
/// `"claude_compat:<id>:<claude_event>:<n>"` scheme
/// `ClaudeCompatReport::hook_registrations()` assigns (`crates/
/// conway-plugin-claude/src/lib.rs`), predictable here because the fixture
/// plugin directory's own `.claude-plugin/plugin.json` names `"acme-tools"`
/// explicitly (`PLUGIN_NAME` below) rather than falling back to a random
/// tempdir basename. Asserted against verbatim in the persisted denial
/// text, the same discriminating check `hook_runner_wiring.rs::HOOK_ID`
/// makes for an operator-authored rule -- this test cannot pass against a
/// denial produced by any OTHER mechanism (e.g. the one-shot allow-list
/// gate that would ALSO deny `bash` and ALSO say "denied", just without
/// naming this id).
const PLUGIN_NAME: &str = "acme-tools";
const EXPECTED_HOOK_ID: &str = "claude_compat:acme-tools:PreToolUse:0";

/// Writes a Claude Code plugin directory, INSIDE `fixture.dir` (so it lives
/// exactly as long as the fixture itself, no second `TempDir` to keep
/// alive), declaring one `PreToolUse` rule whose command always fails
/// (`exit 1`) -- a FAILURE, not an explicit deny answer, so the assertion
/// below proves `ProcessHookRunner`'s real fail-closed plumbing actually
/// ran this translated rule's command, mirroring `hook_runner_wiring.rs`'s
/// own `write_fixture_with_denying_hook` rationale exactly.
fn write_claude_compat_plugin_dir(fixture: &Fixture) -> std::path::PathBuf {
    let plugin_dir = fixture.dir.path().join("acme-claude-plugin");
    std::fs::create_dir_all(plugin_dir.join(".claude-plugin")).expect("create .claude-plugin");
    std::fs::write(
        plugin_dir.join(".claude-plugin").join("plugin.json"),
        format!(r#"{{"name":"{PLUGIN_NAME}"}}"#),
    )
    .expect("write plugin.json");
    std::fs::create_dir_all(plugin_dir.join("hooks")).expect("create hooks dir");
    std::fs::write(
        plugin_dir.join("hooks").join("hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"exit 1"}]}]}}"#,
    )
    .expect("write hooks.json");
    plugin_dir
}

/// `write_fixture`'s rendered config, patched with a `[plugins].
/// claude_compat[]` entry naming the directory `write_claude_compat_plugin_dir`
/// wrote -- `config_warnings.rs`/`hook_runner_wiring.rs`'s own "patch the
/// parsed JSON in place" pattern.
fn write_fixture_with_claude_compat_entry(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let plugin_dir = write_claude_compat_plugin_dir(&fixture);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["plugins"] = serde_json::json!({
        "claude_compat": [
            { "id": PLUGIN_NAME, "dir": plugin_dir.display().to_string(), "timeout_ms": 5_000 }
        ]
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");
    fixture
}

fn tool_call_chunk(name: &'static str, command: &str) -> Chunk {
    Chunk::ToolCall {
        name,
        args: serde_json::json!({ "command": command }),
    }
}

/// Identical two-turn shape to `hook_runner_wiring.rs::
/// one_denied_bash_call_script`: one denied `bash` call, then a plain-text
/// finish, so the run completes with exit 0 rather than looping to
/// `BudgetExceeded`.
fn one_denied_bash_call_script() -> Script {
    Script(vec![
        vec![
            tool_call_chunk("bash", "echo hi"),
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ])
}

/// Scans `fixture`'s session store for the single session file a one-shot
/// `-p` run creates -- inlined rather than shared, mirroring
/// `hook_runner_wiring.rs::only_session_records`'s own doc on why (each
/// `tests/*.rs` integration file compiles independently).
fn only_session_records(fixture: &Fixture) -> Vec<LogRecord> {
    let dir = common::session_dir(fixture);
    let mut found: Option<std::path::PathBuf> = None;
    for entry in std::fs::read_dir(&dir).expect("read sessions dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "index" {
            continue;
        }
        assert!(
            found.is_none(),
            "expected exactly one session file in {}, also found {stem}",
            dir.display()
        );
        found = Some(path);
    }
    let path = found.unwrap_or_else(|| panic!("no session file found in {}", dir.display()));
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read session jsonl at {}: {e}", path.display()));
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<LogRecord>(line)
                .unwrap_or_else(|e| panic!("parse LogRecord: {e}; line: {line}"))
        })
        .collect()
}

fn bash_tool_result_text(records: &[LogRecord]) -> String {
    records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "bash" => {
                result.blocks.iter().find_map(|b| match b {
                    ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("expected a `bash` ToolResultRecord with text in the session transcript")
        })
}

/// VERIFICATION ANCHOR: a `PreToolUse` rule in a Claude Code plugin
/// directory named by `[plugins].claude_compat[]`, driven through the
/// CLI's own `build_conway`, actually denies a `bash` call -- and the
/// persisted denial names THIS translated rule's own id, not merely
/// "denied" by whatever mechanism happened to fire first (the one-shot
/// allow-list gate would also produce "denied" text with no id at all,
/// which is exactly the vacuous pass `hook_runner_wiring.rs`'s own
/// identical assertion shape rules out for the operator-authored case).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_compat_pre_tool_use_hook_denies_a_real_tool_call() {
    let mock = MockBackend::start(one_denied_bash_call_script()).await;
    let fixture = write_fixture_with_claude_compat_entry(&mock);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        out.status.success(),
        "run must complete (the denial is fed back into the turn, not terminal); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let records = only_session_records(&fixture);
    let text = bash_tool_result_text(&records);
    assert!(
        text.contains("denied"),
        "expected the bash ToolResultRecord to say denied; got: {text:?}"
    );
    assert!(
        text.contains(EXPECTED_HOOK_ID),
        "LOAD-BEARING: the denial must name the translated hook's own id \
         ('{EXPECTED_HOOK_ID}') -- a denial by any other mechanism (e.g. the default \
         allow-list gate) would also contain 'denied' without naming it; got: {text:?}"
    );
}

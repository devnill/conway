//! Proves `conway-cli`'s `build_conway`
//! actually installs a `HookRunner` -- not merely that the facade method
//! exists, but that a `pre_tool_use` rule written in a real `settings.json`,
//! driven through the real compiled `conway` binary's own build path,
//! denies a tool call end to end (: an observable outcome, not an
//! intermediate).
//!
//! Reuses the harness (`tests/common/mod.rs`, `common::{run_conway,
//! write_fixture}`) unchanged, the same way `config_warnings.rs` and
//! `subcommands.rs` do: `write_fixture` renders the shared template, then
//! this file patches the parsed JSON in place to add a `[hooks]` section
//! (`config_warnings.rs::write_headroom_warning_fixture`'s own pattern).
//!
//! Reads the persisted `LogRecord::ToolResultRecord` straight off disk
//! (`<common::session_dir(fixture)>/<sid>.jsonl`) rather than the CLI's live
//! `jsonl` stream, mirroring `oneshot_ask.rs`'s own rationale: the denial
//! text is real, but it only ever lands in the tool result record, never on
//! an event the live stream itself carries (`oneshot.rs::
//! unlisted_tool_gets_feedback`'s comment states the identical fact for the
//! allow-list denial shape this file's break-the-guard reproduces).
//!
//! No in-process `ConwayBuilder::from_config`/`ConwayBuilder::discover` call
//! anywhere in this file -- both read the operator's real
//! `$HOME`/`$CONWAY_CONFIG_DIR` ('s
//! `crates/conway/tests/` fix does not reach this crate's own test suite).
//! `common::command`/`common::run_conway` already isolate `CONWAY_CONFIG_DIR`
//! to the fixture's own temp dir for the SUBPROCESS this file drives; this
//! file adds no second, unisolated construction path.

#[allow(dead_code)]
mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway_core::content::ContentBlock;
use conway_core::log::LogRecord;

/// This fixture's `[hooks].rules[0].id` -- asserted against verbatim in the
/// persisted denial text below, so the test cannot pass against a denial
/// produced by any OTHER mechanism (the load-bearing point this whole file
/// exists to pin -- see the module doc's "break-the-guard" note).
const HOOK_ID: &str = "deny-every-bash-call";

/// `write_fixture`'s rendered config, patched with one `pre_tool_use` rule
/// whose command always fails (`exit 1`) -- deliberately a FAILURE, not an
/// explicit `HookPermissionVerdict::Deny` answer: `ProcessHookRunner`'s own
/// fail-closed behavior on a nonzero exit (`crates/conway-tools/src/
/// hook_runner.rs`'s `unix::run`) is what actually denies the call, proving
/// the runner is real, spawned, invoked plumbing, not a stub that always
/// says yes/no regardless of what it ran (spec's own instruction: rebuild
/// the verification-anchor test the first worker already wrote and tested).
///
/// No `[permissions]` override: one-shot `-p`'s own default
/// (`presets::default_permissions_for_one_shot`: allow-list mode, empty
/// allow list) is left in place deliberately -- the hook-denial step this
/// item wires sits BEFORE the mode gate in `PermissionBroker::decide`
/// (`crates/conway-runtime/src/permission.rs`'s own ordering comment), so a
/// hook that is actually consulted denies `bash` before the allow-list gate
/// ever gets a turn either way. That ordering is exactly what makes the
/// hook-id assertion below discriminating: remove `build_conway`'s
/// injection and the SAME empty allow list denies the same call instead,
/// but by naming the allow list, not the hook (verified by hand -- see this
/// item's completion report for the literal denial text observed).
fn write_fixture_with_denying_hook(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["hooks"] = serde_json::json!({
        "rules": [
            {
                "id": HOOK_ID,
                "event": "pre_tool_use",
                "command": ["/bin/sh", "-c", "exit 1"],
            }
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

/// A script that proposes exactly one `bash` call, then -- once the denial
/// is fed back into the turn -- finishes with plain text, so the run
/// completes with exit 0 rather than looping to `BudgetExceeded` (mirrors
/// `oneshot.rs::unlisted_tool_gets_feedback`'s identical two-turn shape).
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
/// `-p` run creates, returning its parsed `LogRecord`s
/// (`subcommands.rs::only_session_id`'s identical scan, plus
/// `oneshot_ask.rs::read_session_records`'s identical parse -- inlined here
/// rather than shared because each integration-test binary compiles
/// independently and this file needs both in one place).
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

/// The rendered text of the `bash` `ToolResultRecord` this run's session
/// transcript carries -- panics if none is found, so a caller cannot
/// silently pass on an absent record the way asserting inside a `.find` map
/// closure alone could.
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

/// VERIFICATION ANCHOR (spec's own "end to end from config text to
/// denial"): a `pre_tool_use` rule written in a real `settings.json`,
/// driven through the CLI's own `build_conway`, actually denies a `bash`
/// call -- and the persisted denial names THIS rule's id, not merely
/// "denied" by whatever mechanism happened to fire first.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cli_pre_tool_use_hook_denies_a_real_tool_call() {
    let mock = MockBackend::start(one_denied_bash_call_script()).await;
    let fixture = write_fixture_with_denying_hook(&mock);

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
        text.contains(HOOK_ID),
        "LOAD-BEARING: the denial must name the hook id ('{HOOK_ID}') that actually fired -- a \
         denial by any other mechanism (e.g. the default allow-list gate) would also contain \
         'denied' without naming it, which is the exact vacuous-pass this test exists to rule \
         out; got: {text:?}"
    );
}

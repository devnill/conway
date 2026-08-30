//! **VERIFICATION ANCHOR, acceptance 1/3, board item
//! `01M19NH39AE2D5AMJK0RZRQY86`.** `conway.ui`'s own model-callable tool
//! (`ask_question`), driven through the REAL compiled `conway` binary in
//! one-shot (`-p`) mode -- the two siblings of `ui_form_absent_by_default.rs`/
//! `ui_form_degrades_under_one_shot.rs`, restated for the NEW consumer those
//! two files predate.
//!
//! Two cases:
//! 1. `conway.ui` absent from `[plugins].install` -> the model cannot even
//!    ask; the run fails with a usage/tool error, since `ask_question` is
//!    not among the announced tools at all (mirrors
//!    `ui_form_absent_by_default.rs`'s own "absent -> unreachable" claim,
//!    for a DIRECT tool call rather than an Edge B capability call).
//! 2. `conway.ui` installed, one-shot `-p` -- the exact host with no
//!    drawing surface (`crates/conway-cli/src/first_party_plugins.rs`'s own
//!    doc: only the interactive TUI ever gets `Some(surface)`) -- the tool
//!    degrades and announces rather than failing the run (mirrors
//!    `ui_form_degrades_under_one_shot.rs`'s own acceptance-3 anchor).

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway_core::content::ContentBlock;
use conway_core::log::LogRecord;

fn one_ask_question_call_script() -> Script {
    Script(vec![
        vec![
            Chunk::ToolCall {
                name: "ask_question",
                args: serde_json::json!({ "prompt": "proceed?", "options": ["yes", "no"] }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ])
}

fn write_fixture_with_plugins(
    mock: &common::mock_backend::MockHandle,
    install: &[&str],
) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["plugins"] = serde_json::json!({ "install": install });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");
    fixture
}

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

fn ask_question_result_text(records: &[LogRecord]) -> (bool, String) {
    records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. }
                if result.tool.as_str() == "ask_question" =>
            {
                let text = result
                    .blocks
                    .iter()
                    .find_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                Some((result.is_error, text))
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "expected an `ask_question` ToolResultRecord in the session transcript: {records:?}"
            )
        })
}

// Multi-thread flavour is REQUIRED, not stylistic: `run_conway` blocks on
// `std::process::Command::output()`, so on the single-threaded flavour the
// `MockBackend` task can never be polled while the binary is running and
// every request it makes is refused. `durable_memory.rs` is the precedent --
// it drives the real binary against this same mock and uses this flavour.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_question_degrades_under_a_real_one_shot_run_with_no_drawing_surface() {
    let mock = MockBackend::start(one_ask_question_call_script()).await;
    let fixture = write_fixture_with_plugins(&mock, &["conway.ui"]);

    let output = run_conway(
        &["-p", "please ask", "--allowed-tools", "ask_question"],
        &fixture,
    );

    assert!(
        output.status.success(),
        "a degraded ask_question call must not fail the one-shot run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = only_session_records(&fixture);
    let (is_error, text) = ask_question_result_text(&records);
    assert!(
        !is_error,
        "the tool result itself must not be marked an error; got text: {text}"
    );
    assert_eq!(
        text,
        "no answer available: no interactive surface is available in this host to ask the operator",
        "expected ask_question's own no-surface degrade sentence"
    );
}

/// Mirrors `ui_form_absent_by_default.rs`'s own "absent -> unreachable"
/// claim, for a DIRECT model tool call rather than an Edge B capability
/// call: `conway.ui` not named in `[plugins].install` at all means
/// `ask_question` is not a registered tool at all -- a call the mock
/// backend proposes anyway (the observable this test drives) reaches
/// `conway-runtime`'s own `unknown tool` refusal
/// (`crates/conway-runtime/src/tools/runner.rs`), not `AskQuestionTool::
/// invoke`. The discriminating text (`"unknown tool"`) is what actually
/// proves absence -- a run that instead showed a real or degraded ANSWER
/// would mean `conway.ui` installed despite not being named.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_question_is_not_reachable_when_conway_ui_is_absent_from_plugins_install() {
    let mock = MockBackend::start(one_ask_question_call_script()).await;
    let fixture = write_fixture_with_plugins(&mock, &[]);

    let output = run_conway(&["-p", "please ask"], &fixture);

    assert!(
        output.status.success(),
        "an unknown-tool refusal must not fail the one-shot run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = only_session_records(&fixture);
    let (is_error, text) = ask_question_result_text(&records);
    assert!(
        is_error,
        "an unregistered tool call must be marked an error, got text: {text}"
    );
    assert!(
        text.contains("unknown tool"),
        "expected conway-runtime's own unknown-tool refusal, proving ask_question is not \
         reachable when conway.ui is absent from [plugins].install, got: {text}"
    );
}

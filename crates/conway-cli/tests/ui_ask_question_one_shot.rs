//! **VERIFICATION ANCHOR, acceptance 1/3, board item
//! `01M19NH39AE2D5AMJK0RZRQY86`.** `conway.ui`'s own model-callable tool
//! (`ask_question`), driven through the REAL compiled `conway` binary in
//! one-shot (`-p`) mode -- the two siblings of `ui_form_absent_by_default.rs`/
//! `ui_form_degrades_under_one_shot.rs`, restated for the NEW consumer those
//! two files predate.
//!
//! Two cases:
//! 1. `conway.ui` absent from `[plugins].install` -> `ask_question` is not
//!    among the announced tools at all (mirrors `ui_form_absent_by_default.
//!    rs`'s own "absent -> unreachable" claim, for a DIRECT tool call
//!    rather than an Edge B capability call). A call for it anyway (this
//!    test's own mock proposes one regardless, to prove absence rather
//!    than assume cooperative model behavior) is rejected one layer below
//!    `conway-runtime`'s tool dispatch, at the wire boundary
//!    (`conway_plugin_backends::tool_calls::ToolCallAccumulator::finish`'s
//!    own "unknown tool" validation against the tools actually SENT) --
//!    the run itself still completes, since that is a `BackendError::
//!    ToolParse`, and `AttemptEngine`'s existing one-shot non-streaming
//!    retry (`conway-runtime/src/attempt.rs`) absorbs it. See that test's
//!    own doc for the two measurements this drives instead of a
//!    `ToolResultRecord` (which never exists here -- the call never
//!    reaches that far).
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
/// call: `conway.ui` not named in `[plugins].install` at all means NO
/// plugin loads at all (`first_party_plugins.rs`'s `bundle` is the sole
/// source of every tool this binary registers), so `ask_question` is not
/// merely one tool missing from a populated list -- the announced tool
/// list is EMPTY, full stop.
///
/// **This test used to assert the wrong layer.** It expected
/// `conway-runtime`'s own `unknown tool` tool-dispatch refusal
/// (`crates/conway-runtime/src/tools/runner.rs`), reached by a
/// `ToolResultRecord`. That refusal is unreachable this way: a call for a
/// tool absent from `req.tools` (the schema list actually sent to the
/// backend) never survives long enough to become a `GenerateResponse`
/// `ToolCall` conway-runtime could dispatch -- it is rejected one layer
/// earlier, at the wire boundary
/// (`conway_plugin_backends::tool_calls::ToolCallAccumulator::finish`'s own
/// "unknown tool" validation against the tools actually sent). That
/// `BackendError::ToolParse` is `Fatal`, so `AttemptEngine::execute`'s
/// existing one-shot non-streaming retry (`attempt.rs`) fires, resending
/// the identical (still toolless) request; this file's shared script's
/// second entry carries no tool call, so the retry completes as ordinary
/// text and the run finishes normally, `ask_question` never dispatched.
///
/// Two measurements establish that (per this crate's own mock's `requests`
/// accessor, not a log line), replacing the single wrong assertion above:
/// 1. **Not one request across the whole run ever carries a `tools`
///    key** -- `wire::build_request_body` only inserts `tools` `if
///    !req.tools.is_empty()` -- the direct proof `ask_question` (indeed
///    every tool) was never announced.
/// 2. **Exactly two requests were made**, the first `"stream": true`
///    (the ordinary no-tools streaming path -- `attempt.rs`'s
///    `strategy_for` streams unconditionally whenever `has_tools` is
///    `false`), the second with NO `"stream"` key at all (`wire::
///    build_request_body` inserts `stream` only `if stream`, never a
///    literal `false`) -- the direct proof it was `AttemptEngine`'s own
///    non-streaming `ToolParse` retry, not some other path, that produced
///    the final answer.
///
/// No `ToolResultRecord` for `ask_question` ever exists in this run's
/// session log -- asserted below as the closing proof the call, though the
/// mock proposed it, never reached tool dispatch at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_question_is_not_reachable_when_conway_ui_is_absent_from_plugins_install() {
    let mock = MockBackend::start(one_ask_question_call_script()).await;
    let fixture = write_fixture_with_plugins(&mock, &[]);

    let output = run_conway(&["-p", "please ask"], &fixture);

    assert!(
        output.status.success(),
        "a tool call for a tool absent from every announced tool list must not fail the \
         one-shot run -- AttemptEngine's own one-shot non-streaming ToolParse retry must \
         absorb it; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        2,
        "expected the initial (toolless, streamed) attempt plus AttemptEngine's own \
         one-shot non-streaming ToolParse retry, got: {requests:#?}"
    );
    for request in &requests {
        assert!(
            request.get("tools").is_none(),
            "ask_question (indeed every tool) must not be announced when conway.ui is \
             absent from [plugins].install, got request: {request}"
        );
    }
    assert_eq!(
        requests[0]["stream"], true,
        "the first attempt is the ordinary no-tools streaming path, got: {}",
        requests[0]
    );
    assert!(
        requests[1].get("stream").is_none(),
        "the retry is AttemptEngine's own non-streaming Strategy::Generate attempt -- \
         `stream` is only ever inserted (as a literal `true`) for a streamed request, never \
         as a literal `false` (see wire::build_request_body), got: {}",
        requests[1]
    );

    let records = only_session_records(&fixture);
    assert!(
        !records.iter().any(|record| matches!(
            record,
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "ask_question"
        )),
        "ask_question was proposed by the mock backend anyway but must never reach tool \
         dispatch when it was never announced; session records: {records:?}"
    );
}

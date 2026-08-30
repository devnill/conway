//! **VERIFICATION ANCHOR, acceptance 3, board item
//! `01M0WWPA70E8YAAN981EK10D3D`.** Under a host offering no drawing
//! surface, a plugin calling `conway.ui`'s `ui.form` capability degrades and
//! announces rather than failing the run -- driven through the REAL
//! compiled `conway` binary in one-shot (`-p`) mode, not a mock, per this
//! item's own instruction: "Test drives the one-shot path, not a mock. This
//! is MAIN-LINE, not an edge case."
//!
//! `conway -p` is exactly the host with no drawing surface
//! (`conway_plugin_ui`'s own module doc: no shipped call site wires a live
//! `FormSurface` into `conway.ui` in this pass, so this is also, today, the
//! TUI's own behavior -- there is nothing TUI-specific being skipped here).
//! Both `conway.ui` and `conway.plugin_skeleton` are named in
//! `[plugins].install`; the mock model calls `skeleton_ask`, which calls
//! into `ui.form` via `CapabilityCallHandle::call_versioned` (the first
//! in-tree caller of that method). The discriminating observable: the run
//! completes with exit code 0 (never fails), and the persisted
//! `ToolResultRecord` for `skeleton_ask` carries the exact degrade text
//! `SkeletonAskTool::invoke` produces when no answer could be collected --
//! a session transcript that instead showed a real answer, an error exit,
//! or a hang would all fail this test, so a regression in EITHER the
//! degrade path OR the plumbing that reaches it is caught.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture, Fixture};
use conway_core::content::ContentBlock;
use conway_core::log::LogRecord;

fn one_skeleton_ask_call_script() -> Script {
    Script(vec![
        vec![
            Chunk::ToolCall {
                name: "skeleton_ask",
                args: serde_json::json!({}),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ])
}

/// `write_fixture`'s rendered config, patched with a `[plugins].install`
/// entry naming both `conway.ui` and `conway.plugin_skeleton` -- mirrors
/// `claude_compat_hooks.rs::write_fixture_with_claude_compat_entry`'s own
/// "patch the parsed JSON in place" pattern.
fn write_fixture_with_ui_and_skeleton_installed(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["plugins"] = serde_json::json!({
        "install": ["conway.ui", "conway.plugin_skeleton"]
    });
    std::fs::write(
        &fixture.config_path,
        serde_json::to_vec(&value).expect("serialize fixture config"),
    )
    .expect("rewrite fixture config");
    fixture
}

/// Scans `fixture`'s session store for its single session file -- inlined
/// rather than shared, mirroring `tilde_expansion.rs::only_session_records`'
/// own doc on why (each `tests/*.rs` integration file compiles
/// independently).
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

fn skeleton_ask_result_text(records: &[LogRecord]) -> (bool, String) {
    records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "skeleton_ask" => {
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
            panic!("expected a `skeleton_ask` ToolResultRecord in the session transcript: {records:?}")
        })
}

#[tokio::test]
async fn skeleton_ask_degrades_under_a_real_one_shot_run_with_no_drawing_surface() {
    let mock = MockBackend::start(one_skeleton_ask_call_script()).await;
    let fixture = write_fixture_with_ui_and_skeleton_installed(&mock);

    let output = run_conway(&["-p", "please ask"], &fixture);

    assert!(
        output.status.success(),
        "a degraded ui.form call must not fail the one-shot run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = only_session_records(&fixture);
    let (is_error, text) = skeleton_ask_result_text(&records);
    assert!(
        !is_error,
        "the tool result itself must not be marked an error; got text: {text}"
    );
    assert!(
        text.starts_with("skeleton ask: no answer available"),
        "expected the degrade message conway.ui's no-drawing-surface refusal produces, got: {text}"
    );
    // The discriminating half -- this file's own module doc names the
    // sibling case (`ui_form_absent_by_default.rs`, `conway.ui` absent
    // entirely) that shares this exact "no answer available" prefix via
    // `CapabilityCallError::NotProvided`. Only the `Provider{
    // no_drawing_surface}` wording below proves `conway.ui` WAS reached
    // and refused, rather than never having been reached at all.
    assert!(
        text.contains("provider failed") && text.contains("no drawing surface"),
        "expected the Provider{{no_drawing_surface}}-shaped message proving \
         conway.ui was reached and refused, got: {text}"
    );
}

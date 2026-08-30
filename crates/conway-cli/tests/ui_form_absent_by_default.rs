//! **VERIFICATION ANCHOR, acceptance 1, board item
//! `01M0WWPA70E8YAAN981EK10D3D`.** `conway.ui` is bundled but never
//! enabled by default: naming `conway.plugin_skeleton` alone in
//! `[plugins].install` (the general case a build with no `[plugins]`
//! section at all reduces to -- `PluginsConfig::install`'s own
//! `#[serde(default)]` empty `Vec`) must NOT also reach `conway.ui`'s
//! `ui.form` capability. As of board item `01M19NH39AE2D5AMJK0RZRQY86`,
//! `conway.ui` DOES contribute a tool of its own (`ask_question`, called
//! directly by the model) -- but this test's own observable predates and is
//! unaffected by that: it drives `conway.ui`'s Edge B capability through a
//! DIFFERENT consumer (`skeleton_ask`, `conway-plugin-skeleton`'s own tool)
//! specifically to prove the capability itself is unreachable when
//! `conway.ui` is absent, which is orthogonal to whether `conway.ui` also
//! ships a tool of its own -- `crates/conway-cli/tests/
//! ui_ask_question_one_shot.rs` is this file's own sibling, proving the
//! identical "absent -> unreachable" claim for `ask_question` directly,
//! plus that tool's own no-drawing-surface degrade (this file's own
//! `ui_form_degrades_under_one_shot.rs` sibling's claim, restated for the
//! new consumer). `skeleton_ask` calls into `ui.form` and gets
//! `CapabilityCallError::NotProvided` unless `conway.ui` is ALSO named. The
//! assertion below checks for `NotProvided`'s own `Display` wording
//! specifically (`"no installed plugin provides capability"`), not merely
//! the "no answer available" prefix every degrade shares with the
//! installed-but-no-surface case
//! (`ui_form_degrades_under_one_shot.rs`) -- that shared prefix alone would
//! stay green even if `conway.ui` were made default-on, since a default-on
//! `conway.ui` would still refuse (today, with no shipped call site wiring
//! a `FormSurface`), just via `Provider{no_drawing_surface}` instead of
//! `NotProvided`. Naming the differentiating substring is what lets this
//! test actually fail if `conway.ui` becomes reachable when absent from
//! `[plugins].install`.

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

/// `write_fixture`'s rendered config, patched with `[plugins].install`
/// naming ONLY `conway.plugin_skeleton` -- `conway.ui` deliberately absent,
/// which is exactly what a build with no `[plugins]` section at all also
/// produces (`PluginsConfig::install`'s empty default).
fn write_fixture_with_only_skeleton_installed(mock: &common::mock_backend::MockHandle) -> Fixture {
    let fixture = write_fixture(mock, 5);
    let text = std::fs::read_to_string(&fixture.config_path).expect("read fixture config");
    let mut value: serde_json::Value = serde_json::from_str(&text).expect("parse fixture config");
    value["plugins"] = serde_json::json!({
        "install": ["conway.plugin_skeleton"]
    });
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

fn skeleton_ask_result_text(records: &[LogRecord]) -> (bool, String) {
    records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. }
                if result.tool.as_str() == "skeleton_ask" =>
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
                "expected a `skeleton_ask` ToolResultRecord in the session transcript: {records:?}"
            )
        })
}

// Multi-thread flavour is REQUIRED, not stylistic: `run_conway` blocks on
// `std::process::Command::output()`, so on the single-threaded flavour the
// `MockBackend` task can never be polled while the binary is running and
// every request it makes is refused. `durable_memory.rs` is the precedent --
// it drives the real binary against this same mock and uses this flavour.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conway_ui_is_not_reachable_when_absent_from_plugins_install() {
    let mock = MockBackend::start(one_skeleton_ask_call_script()).await;
    let fixture = write_fixture_with_only_skeleton_installed(&mock);

    let output = run_conway(
        &["-p", "please ask", "--allowed-tools", "skeleton_ask"],
        &fixture,
    );

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = only_session_records(&fixture);
    let (is_error, text) = skeleton_ask_result_text(&records);
    assert!(
        !is_error,
        "skeleton_ask must degrade, not error: is_error={is_error}, text={text}"
    );
    assert!(
        text.starts_with("skeleton ask: no answer available"),
        "conway.ui must not be reachable when it is not named in [plugins].install, got: {text}"
    );
    // The discriminating half -- see this file's module doc: the shared
    // prefix above cannot tell "genuinely absent" apart from "installed
    // but no drawing surface" (`ui_form_degrades_under_one_shot.rs`'s own
    // case). Only `NotProvided`'s own wording proves `conway.ui` was never
    // reached at all.
    assert!(
        text.contains("no installed plugin provides capability"),
        "expected the NotProvided-shaped message proving conway.ui was never \
         reached, got: {text}"
    );
}

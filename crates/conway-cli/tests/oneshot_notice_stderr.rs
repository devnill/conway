//! Acceptance coverage for board item `01M2MGPF52NHFYN1AKBPR9FDK6`:
//! `crate::render::text::TextRenderer::on_event` used to fall into its own
//! wildcard `_ => {}` arm for `Event::AgentProgress`/`Event::BudgetWarning`,
//! silently dropping every notice conway emits through them -- the runway
//! warning, the budget-crossing notice, and the `max_tokens` silent-turn
//! notice (board item `01M23JRDAM480SRXFGM46GBVA6`) all produced ZERO
//! output on plain `conway -p` (the default `text` output format).
//!
//! **P-15.** `text_mode_prints_the_silent_max_tokens_notice_to_stderr`
//! below FAILS against HEAD (before `TextRenderer::on_event` grew its own
//! `Event::AgentProgress` arm): with only the wildcard arm, the notice
//! never reaches stderr at all, so the `stderr.contains(...)` assertion
//! fails outright. It is deliberately paired with
//! `json_mode_stdout_stays_one_parseable_object_for_the_same_notice`, which
//! asserts the OTHER half of this item's own binding constraint -- that
//! `--output-format json` still emits exactly one clean JSON object on
//! stdout for the identical scenario -- so a fix that restores the notice
//! by (incorrectly) writing it to stdout, or that otherwise corrupts the
//! one-document-in/one-document-out contract `docs/scripting.md` promises
//! every script, would be caught by this file's OTHER test even if it
//! accidentally made the first one pass.
//!
//! Reuses the exact scenario shape `crates/conway-runtime/tests/
//! max_tokens_silence.rs` already established at the runtime level (a
//! turn that ends `stop: max_tokens` having produced neither a `Text` nor
//! a `ToolUse` block) rather than inventing a second fixture -- the CLI's
//! own mock backend speaks OpenAI-compat wire JSON, not
//! `conway_testkit::ScriptedBackend`, so the equivalent shape here is a
//! response whose ONLY chunk is `Chunk::Finish("length")`: no
//! `Chunk::Text`, no `Chunk::ToolCall`. `"length"` is the OpenAI-compat
//! `finish_reason` `conway-plugin-backends`' own wire dialect maps to
//! `StopReason::MaxTokens` (`openai_compat/wire.rs::map_finish_reason`),
//! and `AgentLoop`'s own silent-turn check keys off the ABSENCE of a
//! `Text`/`ToolUse` block, never off `Thinking` specifically -- so no
//! `Thinking` chunk is needed to land in the same "said nothing at all"
//! branch that produces the `max_tokens_silent` wording.

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{run_conway, write_fixture};

fn silent_max_tokens_script() -> Script {
    Script(vec![vec![Chunk::Finish("length")]])
}

/// The default `text` output format: a turn that says nothing at all
/// (`stop: max_tokens`, no text, no tool call) now prints the "said
/// nothing" notice to stderr, rather than leaving the operator staring at
/// an exit-0 run with empty stdout and no explanation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn text_mode_prints_the_silent_max_tokens_notice_to_stderr() {
    let mock = MockBackend::start(silent_max_tokens_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.is_empty(),
        "a turn with no Text/ToolUse block produces no reply text at all -- stdout must stay \
         empty, got: {:?}",
        String::from_utf8_lossy(&out.stdout)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("max_tokens") && stderr.to_lowercase().contains("no visible answer"),
        "expected the silent max_tokens notice on stderr, got: {stderr:?}"
    );
}

/// The identical scenario, but `--output-format json`: stdout must still
/// carry exactly one parseable JSON object -- the terminal `AgentResult` --
/// and nothing else, byte-clean. This is what stops a fix for the test
/// above from being implemented by (incorrectly) writing the notice to
/// stdout: `--output-format json` withholds all incremental output from
/// STDOUT until the terminal result by design (`crate::render::json`'s own
/// module doc), so any notice reaching stdout in this mode -- from this
/// event or any other -- would corrupt the one-document contract every
/// script depends on. Its stderr half is the test immediately below.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_mode_stdout_stays_one_parseable_object_for_the_same_notice() {
    let mock = MockBackend::start(silent_max_tokens_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--output-format", "json"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout_text = String::from_utf8(out.stdout).expect("stdout must be valid utf-8");
    assert_eq!(
        stdout_text.matches('\n').count(),
        1,
        "stdout must be exactly one line (one trailing newline), got: {stdout_text:?}"
    );
    let value: serde_json::Value =
        serde_json::from_str(stdout_text.trim_end()).expect("stdout must parse as one JSON object");
    assert!(
        value.is_object(),
        "the one object must be a JSON object: {value:?}"
    );
    assert_eq!(
        value["status"]["status"], "completed",
        "sanity: the scripted silent turn still completes normally: {value:?}"
    );
}

/// The stderr half of the same `--output-format json` run: a `json`-mode
/// caller gets the notice too, as prose on stderr, alongside the untouched
/// JSON on stdout.
///
/// This is the pair that makes the two tests above mean what they claim.
/// Without it, `json` mode could satisfy "nothing on stdout" by rendering
/// nothing anywhere -- which is exactly what it did before board item
/// `01M2MGPF52NHFYN1AKBPR9FDK6`, and exactly the silence that item exists
/// to remove. A `json`-mode caller is the one with NO transcript to read
/// the notice from instead, so it is the surface that can least afford to
/// be the one left silent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn json_mode_still_prints_the_silent_max_tokens_notice_to_stderr() {
    let mock = MockBackend::start(silent_max_tokens_script()).await;
    let fixture = write_fixture(&mock, 10);

    let out = run_conway(&["-p", "hi", "--output-format", "json"], &fixture);

    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("max_tokens") && stderr.to_lowercase().contains("no visible answer"),
        "expected the silent max_tokens notice on stderr in json mode too, got: {stderr:?}"
    );
}

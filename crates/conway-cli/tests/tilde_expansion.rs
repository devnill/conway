//! Compiled-binary regression test for board item
//! `01M10HSENWKTEE4G691XJXBH6T`: `conway_core::containment::
//! resolve_candidate` -- the one implementation every fs/shell tool path
//! argument resolves through (`conway_tools::common::resolve_path`) -- now
//! expands a leading `~`/`~/` against the process's home directory. Before
//! this item, `~` was passed through as a literal path component and the
//! model saw "file not found" for what was actually an unexpanded
//! reference (the dogfooding failure this item is named after).
//!
//! Driven through the REAL compiled `conway` binary, a real one-shot
//! session, and a real `read` tool call -- not merely a unit-level
//! `resolve_candidate`/`resolve_path` call -- because a test of the shared
//! resolver alone proves nothing about whether the tool that a model
//! actually calls still reaches it (the exact discriminating shape
//! `claude_compat_hooks.rs`/`root_containment_seam.rs` already use in this
//! tree).
//!
//! **No real `$HOME`, ever.** `simulated_home` below is an ordinary
//! [`tempfile::TempDir`] this process created and owns, standing in for the
//! operator's real home directory -- `HOME`/`USERPROFILE` are overridden on
//! the SPAWNED CHILD PROCESS only (`Command::env`), never mutated on this
//! test binary's own process, so this cannot race any other test in the
//! same `cargo test` binary the way an in-process `std::env::set_var` would
//! (mirrors `global_instructions_isolation.rs`'s own isolation shape).

mod common;

use common::mock_backend::{Chunk, MockBackend, Script};
use common::{command, write_fixture, Fixture};
use conway_core::content::ContentBlock;
use conway_core::log::LogRecord;

/// Content written into the simulated home's `target.txt` -- must appear in
/// the `read` tool's persisted result once `~/target.txt` resolves to the
/// simulated home rather than being denied as a literal, nonexistent path.
const MARKER: &str = "TILDE_EXPANSION_MARKER_7B1AC0";

fn one_read_call_script(path: &str) -> Script {
    Script(vec![
        vec![
            Chunk::ToolCall {
                name: "read",
                args: serde_json::json!({ "path": path }),
            },
            Chunk::Finish("tool_calls"),
        ],
        vec![Chunk::Text("done"), Chunk::Finish("stop")],
    ])
}

/// Scans `fixture`'s session store for its single session file, exactly as
/// `claude_compat_hooks.rs::only_session_records` does (each `tests/*.rs`
/// integration file compiles independently, so this is a deliberate
/// byte-for-byte sibling, not a shared helper).
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

fn read_tool_result(records: &[LogRecord]) -> (bool, String) {
    records
        .iter()
        .find_map(|r| match r {
            LogRecord::ToolResultRecord { result, .. } if result.tool.as_str() == "read" => {
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
            panic!("expected a `read` ToolResultRecord in the session transcript: {records:?}")
        })
}

/// **VERIFICATION ANCHOR.** A `~/`-prefixed `read` argument, resolved by the
/// real `read` tool inside the real compiled binary, reaches the simulated
/// home directory's file -- not a literal `<cwd>/~/target.txt` (which would
/// not exist, and would surface as the pre-fix "file not found").
///
/// `#[cfg(unix)]` only: this test overrides `HOME`/`USERPROFILE` on the
/// SPAWNED CHILD, but on Windows `directories::BaseDirs::home_dir()` does
/// not read `%USERPROFILE%` at all -- it goes through the Windows Known
/// Folder API (`SHGetKnownFolderPath` / `FOLDERID_Profile`), which the env
/// override has zero effect on (see `home_dir`'s doc comment in
/// `conway-core/src/containment.rs`). On Windows the child would expand
/// `~/target.txt` against the real machine profile directory instead of
/// `simulated_home`, the marker file would not be there, and this test
/// would fail for a reason unrelated to the expansion logic under test.
/// Gated here with a stated reason rather than left to fail unexplained.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_leading_tilde_slash_argument_resolves_against_the_real_home_directory() {
    let simulated_home = tempfile::tempdir().expect("tempdir for simulated $HOME");
    std::fs::write(simulated_home.path().join("target.txt"), MARKER)
        .expect("write target file under simulated $HOME");

    let mock = MockBackend::start(one_read_call_script("~/target.txt")).await;
    let fixture = write_fixture(&mock, 5);

    let out = command(
        &["-p", "read the file", "--allowed-tools", "read"],
        &fixture,
    )
    .env("HOME", simulated_home.path())
    .env("USERPROFILE", simulated_home.path())
    .output()
    .expect("run conway binary");

    assert!(
        out.status.success(),
        "conway -p should succeed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let records = only_session_records(&fixture);
    let (is_error, text) = read_tool_result(&records);
    assert!(
        !is_error,
        "expected `read ~/target.txt` to succeed once `~` is expanded to the simulated home \
         directory, got an error result: {text:?}"
    );
    assert!(
        text.contains(MARKER),
        "expected the read tool's result to contain the simulated home directory's file \
         contents, proving `~/target.txt` resolved there rather than under the fixture's cwd; \
         got: {text:?}"
    );
}

/// **Acceptance: "the failure an operator sees names tilde explicitly."**
/// `~bob/secret.txt` begins with `~` but is a form this item's ruling names
/// explicitly as NOT expanded (only a bare `~` or a leading `~/` are). The
/// pre-fix behavior for ANY `~`-prefixed argument was the generic,
/// diagnosis-free "could not be found" the item's own dogfooding narrative
/// describes; the fix must instead name `~` in the denial text reaching the
/// model, end to end through the real compiled binary and a real `read`
/// call -- not merely as a unit-level `resolve_candidate`/`resolve_path`
/// return value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unsupported_tilde_user_form_names_tilde_in_the_denial_reaching_the_model() {
    let mock = MockBackend::start(one_read_call_script("~bob/secret.txt")).await;
    let fixture = write_fixture(&mock, 5);

    let out = command(
        &["-p", "read the file", "--allowed-tools", "read"],
        &fixture,
    )
    .output()
    .expect("run conway binary");

    assert!(
        out.status.success(),
        "conway -p should succeed (the failure is fed back into the turn, not terminal); \
         stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let records = only_session_records(&fixture);
    let (is_error, text) = read_tool_result(&records);
    assert!(
        is_error,
        "a `~bob/...`-style argument conway does not expand must be refused, not silently \
         treated as a literal path: {text:?}"
    );
    assert!(
        text.contains('~'),
        "LOAD-BEARING: the denial text reaching the model must name tilde explicitly -- the \
         old behavior was a generic \"file not found\" that named nothing about `~` at all; \
         got: {text:?}"
    );
}

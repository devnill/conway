//! Pins `LogRecord`'s wire-compatibility contract (board item
//! `01M0V2KE7PG8BF3FK90BFTSG47`, and `crate::log`'s own module doc, which
//! states the contract this test enforces): every record ever written to a
//! `<session-id>.jsonl` file must still deserialize under the CURRENT
//! build.
//!
//! ## Where `fixtures/log_2026-08-15_73df3c0.jsonl` came from
//!
//! **Hand-authored from the schema as it stood at commit `73df3c0`**
//! (subject: "Confinement survives resume, the default role stops naming a
//! job, and -p reads a pipe", 2026-08-15) — `git show 73df3c0:crates/
//! conway-core/src/log.rs` (plus `agent.rs`, `content.rs`, `provenance.rs`,
//! `ids.rs` at the same commit) was read to determine the exact field set
//! each record
//! carried at that point, and this file's JSON was written to match THAT
//! shape, not today's. `73df3c0` was chosen, not an older prototype commit,
//! because it lands two days after the earliest point this project has any
//! record of `conway` being used to produce a real session (2026-08-13 —
//! see the project's own dogfooding note); commits before that point never
//! had a real session log written against them, so a fixture pinned to one
//! of them would assert compatibility with a shape no operator data ever
//! actually took. This route was chosen over generating a fixture by
//! running an old build (not possible here — no `cargo test`/`cargo run`
//! budget in this lane) and over generating one from the CURRENT schema
//! (which would test today's `serde` derive against itself and catch
//! nothing — the exact failure mode this test exists to close, matching
//! `conway-plugin-trim`'s already-shipped `synthetic_session.jsonl`, which
//! this project's own review flagged as generated-not-recovered).
//!
//! Diffing `73df3c0..HEAD` for `crates/conway-core/src/{log,agent,content,
//! provenance,ids}.rs` at the time this fixture was written turned up only
//! additive, contract-safe changes: `Provenance` gained a `Memory` variant
//! (new variant -- always safe); `ContextReport` gained `curator_failed`
//! and `instruction_fragments`, both `#[serde(default)]`; and
//! `conway_core::log::LogRecord` gained two variants that did not exist
//! at `73df3c0` at all, `ContextPathSet` and `ContextPathNamed`, which are
//! therefore correctly absent from this fixture rather than faked in.
//! Nothing in that diff removes `#[serde(default)]` from an existing field
//! or adds a bare required one -- see "Question 4" in this item's own
//! completion report for the full audit.
//!
//! ## Which variants this fixture covers, and why
//!
//! The ones an ordinary resumed session actually contains: `Header`,
//! `UserTurn`, `Assistant`, `ToolResultRecord`, `ForkDirective`,
//! `ParentSteer`, `SystemNote`, `AgentResultRecord`, `ChildResultRecord`,
//! `ContextReportRecord` (the last is written after nearly every turn --
//! see `conway_runtime::context::report::append_context_report`). Left out
//! deliberately: `ContextMask`, `ContextPathSet`, `ContextPathNamed` --
//! all three are reachable only through an installed plugin's own command
//! (or, for `ContextPathNamed`, not reachable at all -- see that variant's
//! own doc), so they are not part of the shape an ordinary `--resume`
//! replays, and two of the three did not exist yet at the commit this
//! fixture is pinned to regardless.
//!
//! ## Do not regenerate this fixture to make a failing test pass
//!
//! A fixture refreshed to match whatever the schema currently is proves
//! nothing -- it is the pinning-test failure mode `docs/vision/
//! DESIGN-context-path.md`'s golden-file rule already names: "a
//! regenerated golden file is the failure signal, not the fix." If this
//! test starts failing, the correct response is to add `#[serde(default)]`
//! (or otherwise restore compatibility) to whatever field broke it, not to
//! rewrite this file so the assertions match the new shape.
//!
//! ## Falsification (documented, not executed here -- this lane has no
//! `cargo test` budget)
//!
//! Adding a single non-optional field with no `#[serde(default)]` to an
//! existing variant reproduces the exact failure this test guards against.
//! For example: add `pub foo: u32` (no `Option`, no `#[serde(default)]`)
//! to `LogRecord::SystemNote` in `crates/conway-core/src/log.rs`. Running
//! this test afterward fails at `parses_every_record_line` -- specifically
//! the `serde_json::from_str::<LogRecord>(line)` call for the
//! `"kind":"system_note"` line returns
//! `Err(missing field "foo")`, and the `.expect(...)` on that line panics
//! with a message naming the fixture's line number and the underlying
//! `serde_json::Error`.

use conway_core::agent::ResultStatus;
use conway_core::log::LogRecord;

const FIXTURE: &str = include_str!("fixtures/log_2026-08-15_73df3c0.jsonl");

/// Parses every non-empty line of the fixture as a standalone `LogRecord`
/// (the header line included -- `LogRecord::Header` is itself a variant of
/// this enum, tagged `"kind":"header"` by the same `#[serde(tag = "kind")]`
/// that tags every other line, per `crate::log`'s own module doc).
fn parse_fixture() -> Vec<LogRecord> {
    FIXTURE
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str::<LogRecord>(line)
                .unwrap_or_else(|e| panic!("fixture line {} failed to deserialize: {e}", i + 1))
        })
        .collect()
}

#[test]
fn parses_every_record_line() {
    let records = parse_fixture();
    assert_eq!(
        records.len(),
        10,
        "fixture line count changed -- update this test deliberately if the fixture itself \
         was deliberately changed, never to paper over a deserialize failure"
    );
}

#[test]
fn header_decodes_with_every_field_this_build_expects() {
    let records = parse_fixture();
    match &records[0] {
        LogRecord::Header(meta) => {
            assert_eq!(meta.agent_def.as_deref(), Some("reviewer"));
            assert_eq!(
                meta.role.as_ref().map(|r| r.to_string()),
                Some("coder".to_string())
            );
            assert!(!meta.ephemeral);
            assert!(meta.ask_origin.is_none());
            assert!(meta.root.is_none());
            assert!(meta.plugin_config.values.is_empty());
            assert_eq!(meta.labels, vec!["compat-fixture".to_string()]);
        }
        other => panic!("line 1: expected Header, got {other:?}"),
    }
}

#[test]
fn ordinary_turn_records_decode_with_provenance_intact() {
    let records = parse_fixture();

    match &records[1] {
        LogRecord::UserTurn { seq, text, .. } => {
            assert_eq!(seq.0, 1);
            assert_eq!(text, "Track down why the flaky test fails intermittently.");
        }
        other => panic!("line 2: expected UserTurn, got {other:?}"),
    }

    match &records[2] {
        LogRecord::Assistant { seq, content, .. } => {
            assert_eq!(seq.0, 2);
            assert_eq!(content.len(), 2);
        }
        other => panic!("line 3: expected Assistant, got {other:?}"),
    }

    match &records[3] {
        LogRecord::ToolResultRecord { seq, result, .. } => {
            assert_eq!(seq.0, 3);
            assert!(!result.is_error);
            assert!(result.truncated.is_none());
        }
        other => panic!("line 4: expected ToolResultRecord, got {other:?}"),
    }
}

#[test]
fn agent_result_and_child_result_carry_transcript_ref() {
    let records = parse_fixture();

    // `ChildResultRecord` (fixture line 8 -- seq 7). The index is line-1
    // because line 1 is the `header`, which carries no `seq` of its own:
    // `seq` 1 lands on line 2, so seq N is at fixture line N+1 and index N.
    match &records[7] {
        LogRecord::ChildResultRecord { seq, result, .. } => {
            assert_eq!(seq.0, 7);
            assert_eq!(result.status, ResultStatus::Completed);
            assert_eq!(
                result.transcript_ref.to_string(),
                "01M0J07RX738Z7R95BFKQBRPB1"
            );
        }
        other => panic!("fixture line 8: expected ChildResultRecord, got {other:?}"),
    }

    // The session's own terminal `AgentResultRecord` (fixture line 9 -- seq 8).
    match &records[8] {
        LogRecord::AgentResultRecord { seq, result, .. } => {
            assert_eq!(seq.0, 8);
            assert_eq!(result.steps_taken, 9);
            assert_eq!(result.artifacts.len(), 1);
            // The exact field this whole compatibility question started
            // from (`crate::agent`'s `AgentResult::transcript_ref` doc):
            // still present, still required, still round-trips.
            assert_eq!(
                result.transcript_ref.to_string(),
                "01M0J07RX60F7A5NR9TW51JG18"
            );
        }
        other => panic!("fixture line 9: expected AgentResultRecord, got {other:?}"),
    }
}

#[test]
fn context_report_decodes_without_the_field_it_did_not_have_yet() {
    let records = parse_fixture();
    match &records[9] {
        LogRecord::ContextReportRecord { seq, report, .. } => {
            assert_eq!(seq.0, 9);
            // `73df3c0`'s `ContextReport` has neither `curator_failed` nor
            // `instruction_fragments` -- both landed later, both defaulted.
            // Their absence from the fixture line is the point: this line
            // decodes into TODAY's `ContextReport` (which has both fields)
            // with each filled in via `#[serde(default)]`, not rejected as
            // a missing field.
            assert_eq!(report.dropped, Vec::<String>::new());
            assert_eq!(report.total_tokens_est, 548);
            assert_eq!(report.curator_failed, None);
            assert!(report.instruction_fragments.is_empty());
        }
        other => panic!("line 10: expected ContextReportRecord, got {other:?}"),
    }
}

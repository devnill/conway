//! Integration tests for the JSONL line codec (WI-046 criteria): header
//! round-trip against a §5.1-shaped example, record line shape, and a
//! ≥256-case property test asserting
//! `decode_record(encode_record(r, s)) == (s, r)` across every
//! non-`Header` `LogRecord` variant.

use chrono::{DateTime, Utc};
use conway_core::agent::{AgentResult, ResultStatus};
use conway_core::content::{ContentBlock, StopReason, ToolResult, Usage};
use conway_core::ids::{AgentId, LogSeq, ModelRef, SegmentId, SessionId, ToolName};
use conway_core::log::{ForkOrigin, LogRecord, SubagentMode};
use conway_core::provenance::{ContextReport, ContextReportEntry, Provenance};
use conway_session::codec::{
    decode_header, decode_line, decode_record, encode_header, encode_record, CodecError, Line,
};
use conway_session::SessionMeta;
use proptest::prelude::*;
use std::path::PathBuf;

fn ts() -> DateTime<Utc> {
    "2026-07-20T00:00:00Z".parse().unwrap()
}

/// §5.1-shaped example header (valid ULIDs substituted for the doc's
/// illustrative ids).
fn example_header_line() -> (String, SessionId, AgentId, SessionId) {
    let sid = SessionId::new();
    let parent = SessionId::new();
    let agent = AgentId::new();
    let line = format!(
        "{{\"kind\":\"header\",\"session\":\"{sid}\",\"agent\":\"{agent}\",\"created\":\"2026-07-20T00:00:00Z\",\
         \"origin\":{{\"parent\":\"{parent}\",\"at_seq\":142,\"mode\":\"fork\"}},\
         \"agent_def\":\"reviewer\",\"role\":\"coder\",\"cwd\":\"/tmp/p\",\"labels\":[],\"status\":\"active\"}}\n"
    );
    (line, sid, agent, parent)
}

#[test]
fn interior_newline_in_text_stays_escaped_single_line() {
    let rec = LogRecord::UserTurn {
        seq: LogSeq(1),
        ts: ts(),
        text: "line one\nline two".into(),
        prov: Provenance::UserPrompt,
    };
    let line = encode_record(&rec, LogSeq(1));
    assert_eq!(
        line.matches('\n').count(),
        1,
        "newline in text must stay JSON-escaped"
    );
    let (seq, back) = decode_record(&line).unwrap();
    assert_eq!(seq, LogSeq(1));
    match back {
        LogRecord::UserTurn { text, .. } => assert_eq!(text, "line one\nline two"),
        other => panic!("wrong variant: {other:?}"),
    }
}

#[test]
fn header_round_trips_through_the_5_1_shaped_example_verbatim() {
    let (line, sid, agent, parent) = example_header_line();
    let meta = decode_header(&line).unwrap();
    let expected = SessionMeta {
        id: sid,
        agent_id: agent,
        origin: Some(ForkOrigin {
            parent,
            at_seq: LogSeq(142),
            mode: SubagentMode::Fork,
        }),
        agent_def: Some("reviewer".into()),
        role: Some(conway_core::ids::RoleAlias::new("coder")),
        created: ts(),
        cwd: PathBuf::from("/tmp/p"),
        labels: vec![],
        ephemeral: false,
        ask_origin: None,
        root: None,
    };
    assert_eq!(meta, expected);

    // Re-encoding and re-decoding is also a round trip (encode_header ->
    // decode_header), independent of the hand-written example line above.
    let re_encoded = encode_header(&meta);
    let back = decode_header(&re_encoded).unwrap();
    assert_eq!(back, meta);
}

#[test]
fn encode_record_emits_exactly_one_line_with_seq_and_kind_top_level() {
    let rec = LogRecord::UserTurn {
        seq: LogSeq(3),
        ts: ts(),
        text: "hi".into(),
        prov: Provenance::UserPrompt,
    };
    let line = encode_record(&rec, LogSeq(3));
    assert!(line.ends_with('\n'), "must end with \\n: {line:?}");
    assert_eq!(
        line.matches('\n').count(),
        1,
        "must contain no interior \\n: {line:?}"
    );
    let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(value["seq"], 3);
    assert_eq!(value["kind"], "user_turn");
}

#[test]
fn decode_line_of_a_record_missing_seq_is_missing_seq() {
    let line = r#"{"kind":"user_turn","ts":"2026-07-20T00:00:00Z","text":"hi","prov":{"type":"user_prompt"}}"#;
    let err = decode_line(line).unwrap_err();
    assert!(matches!(err, CodecError::MissingSeq));
    let err = decode_record(line).unwrap_err();
    assert!(matches!(err, CodecError::MissingSeq));
}

#[test]
fn decode_line_dispatches_header_vs_record() {
    let (header_line, ..) = example_header_line();
    assert!(matches!(
        decode_line(&header_line).unwrap(),
        Line::Header(_)
    ));

    let rec = LogRecord::UserTurn {
        seq: LogSeq(0),
        ts: ts(),
        text: "hi".into(),
        prov: Provenance::UserPrompt,
    };
    let record_line = encode_record(&rec, LogSeq(0));
    assert!(matches!(
        decode_line(&record_line).unwrap(),
        Line::Record { seq: LogSeq(0), .. }
    ));
}

// ---------------------------------------------------------------------
// Property test: decode_record(encode_record(r, s)) == (s, r), for
// arbitrary LogRecord covering every non-Header variant. `s` is always
// `r.seq()` (the realistic call pattern — see codec.rs's module doc on
// why `encode_record`'s `seq` parameter is authoritative), so the
// equality is a literal round trip.
// ---------------------------------------------------------------------

fn arb_seq() -> impl Strategy<Value = LogSeq> {
    (0u64..1_000_000).prop_map(LogSeq)
}

fn arb_text() -> impl Strategy<Value = String> {
    // Deliberately includes newlines, quotes, backslashes, and multi-byte
    // unicode: the "no interior newline" invariant depends on JSON string
    // escaping, so the strategy must actually generate the threatening
    // input class (incremental review S3, cycle 1).
    proptest::string::string_regex("[a-zA-Z0-9 \n\"\\\\\u{e9}\u{3042}]{0,40}").expect("valid regex")
}

fn arb_user_turn() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text()).prop_map(|(seq, text)| LogRecord::UserTurn {
        seq,
        ts: ts(),
        text,
        prov: Provenance::UserPrompt,
    })
}

fn arb_assistant() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text(), any::<u32>(), any::<u32>()).prop_map(
        |(seq, text, input_tokens, output_tokens)| LogRecord::Assistant {
            seq,
            ts: ts(),
            content: vec![ContentBlock::Text { text }],
            model: ModelRef {
                backend: "anthropic".into(),
                model: "claude-sonnet-4-6".into(),
            },
            route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
            usage: Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            stop: StopReason::EndTurn,
        },
    )
}

fn arb_tool_result_record() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text(), any::<bool>()).prop_map(|(seq, call_id, is_error)| {
        LogRecord::ToolResultRecord {
            seq,
            ts: ts(),
            result: ToolResult {
                call_id,
                tool: ToolName::new("read"),
                blocks: vec![],
                is_error,
                truncated: None,
            },
        }
    })
}

fn arb_fork_directive() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text()).prop_map(|(seq, text)| {
        let by = AgentId::new();
        LogRecord::ForkDirective {
            seq,
            ts: ts(),
            text,
            by,
            prov: Provenance::ForkDirective { by },
        }
    })
}

fn arb_parent_steer() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text(), arb_seq()).prop_map(|(seq, text, parent_seq)| {
        let from = AgentId::new();
        LogRecord::ParentSteer {
            seq,
            ts: ts(),
            text,
            from,
            parent_seq,
            prov: Provenance::ParentSteer { from, parent_seq },
        }
    })
}

fn arb_system_note() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text(), arb_text()).prop_map(|(seq, text, reason)| LogRecord::SystemNote {
        seq,
        ts: ts(),
        text,
        reason: reason.clone(),
        prov: Provenance::SystemNote { reason },
    })
}

fn arb_agent_result_record() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text()).prop_map(|(seq, summary)| LogRecord::AgentResultRecord {
        seq,
        ts: ts(),
        result: AgentResult::new(
            AgentId::new(),
            SessionId::new(),
            ResultStatus::Completed,
            summary,
        ),
    })
}

fn arb_child_result_record() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), arb_text()).prop_map(|(seq, summary)| {
        let from = AgentId::new();
        LogRecord::ChildResultRecord {
            seq,
            ts: ts(),
            result: AgentResult::new(from, SessionId::new(), ResultStatus::Completed, summary),
            prov: Provenance::ChildResult { from },
        }
    })
}

fn arb_context_report_record() -> impl Strategy<Value = LogRecord> {
    (arb_seq(), any::<u32>(), any::<u32>()).prop_map(|(seq, turn, tokens_est)| {
        LogRecord::ContextReportRecord {
            seq,
            ts: ts(),
            report: ContextReport {
                agent_id: AgentId::new(),
                turn,
                tokenizer: "cl100k_base".into(),
                segments: vec![ContextReportEntry {
                    segment: SegmentId::new(),
                    provenance: Provenance::UserPrompt,
                    tokens_est,
                    estimated: true,
                }],
                total_tokens_est: tokens_est,
            },
        }
    })
}

fn arb_log_record() -> impl Strategy<Value = LogRecord> {
    prop_oneof![
        arb_user_turn(),
        arb_assistant(),
        arb_tool_result_record(),
        arb_fork_directive(),
        arb_parent_steer(),
        arb_system_note(),
        arb_agent_result_record(),
        arb_child_result_record(),
        arb_context_report_record(),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn encode_decode_record_round_trips(rec in arb_log_record()) {
        let seq = rec.seq().expect("non-Header LogRecord variants always carry a seq");
        let line = encode_record(&rec, seq);
        prop_assert!(line.ends_with('\n'));
        prop_assert_eq!(line.matches('\n').count(), 1);
        let (decoded_seq, decoded_rec) = decode_record(&line).unwrap();
        prop_assert_eq!(decoded_seq, seq);
        prop_assert_eq!(decoded_rec, rec);
    }
}

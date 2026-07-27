//! The JSONL line codec: header (de)serialization and record line
//! (de)serialization (architecture §5.1).
//!
//! Every other work item in this module depends on this file: `store.rs`
//! (WI-047) reads/writes lines through it, `fork.rs` (WI-048) writes header
//! lines through `encode_header`, and `provenance.rs` (WI-051) appends
//! `context_report` records through `encode_record`.
//!
//! ## Wire forms
//!
//! - Header (line 0 of every session file): `LogRecord::Header(SessionMeta)`
//!   serialized directly. `conway-core`'s `#[serde(tag = "kind")]` on
//!   `LogRecord` supplies `"kind":"header"`; `SessionMeta`'s own renames
//!   supply `session`/`agent` for `id`/`agent_id` (architecture §5.1).
//! - Record (line 1+): every non-`Header` `LogRecord` variant already
//!   carries `seq: LogSeq` as a struct field, so serializing the record
//!   directly already yields a top-level `"seq"` key alongside `"kind"`
//!   (`serde`'s internally-tagged enum representation flattens struct
//!   variant fields into the tagged object). `encode_record`'s `seq`
//!   parameter is authoritative: see the "seq resolution" note below.
//!
//! ## seq resolution (implementation note, not part of the public contract)
//!
//! The WI-046 spec text was written assuming `LogRecord` records do not
//! carry their own `seq` and that `encode_record` must inject it via
//! `serde_json::Value::Object` insertion. `conway-core`'s actual
//! `LogRecord` (authoritative; see `crates/conway-core/src/log.rs`) already
//! has `seq: LogSeq` on every non-`Header` variant, so there is exactly one
//! `"seq"` key in the serialized object either way. `encode_record` still
//! performs the `Value::Object` insertion described in the spec — but as an
//! *overwrite*, not an *addition*: the `seq` parameter always wins over
//! whatever `rec`'s own `seq` field held, guaranteeing the round-trip law
//! `decode_record(encode_record(r, s)) == (s, r)` holds unconditionally,
//! including for a caller-supplied `r`/`s` pair whose `r.seq()` disagrees
//! with `s` (the returned record's `seq` field reflects `s`, not the
//! original `r.seq()`). This is a deliberate resolution of the tension
//! between the spec's literal signature (`encode_record(..) -> String`,
//! infallible) and its prose ("mismatch -> a CodecError variant"): because
//! the signature the acceptance criteria pins down returns a bare `String`
//! and not a `Result`, `encode_record` cannot itself return a `CodecError`,
//! so mismatch cannot be an error — it is instead resolved by treating
//! `seq` as authoritative and overwriting. `CodecError::NotAnObject` is
//! kept in the error enum per the spec (and is what `try_encode_record`,
//! below, would return), but it is unreachable through the public
//! `encode_record` today because every `LogRecord` variant currently
//! serializes to a JSON object.

use conway_core::ids::LogSeq;
use conway_core::log::{LogRecord, SessionMeta};

/// Errors from decoding (or, in principle, encoding) a JSONL session log
/// line.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The line was not valid JSON, or the parsed JSON did not match the
    /// shape `serde` expected for `SessionMeta`/`LogRecord`.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// A record line (`kind != "header"`) had no top-level `"seq"` key.
    #[error("record line is missing the `seq` field")]
    MissingSeq,
    /// The line's top-level JSON object had no `"kind"` key at all.
    #[error("line is missing the `kind` field")]
    MissingKind,
    /// A record failed to serialize to a JSON object (defensive; not
    /// reachable via the public API today — see the module-level note).
    #[error("serialized record is not a JSON object")]
    NotAnObject,
    /// `decode_header` was called on a record line, or `decode_record` was
    /// called on a header line.
    #[error("wrong line kind: expected {expected}, got {got}")]
    WrongLineKind {
        expected: &'static str,
        got: &'static str,
    },
}

/// One decoded line of a session file: either the header (line 0) or a
/// sequenced record (line 1+).
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    Header(SessionMeta),
    Record { seq: LogSeq, rec: LogRecord },
}

/// Encodes a session header as a single `\n`-terminated JSON line
/// (architecture §5.1, line 0 of every session file).
pub fn encode_header(meta: &SessionMeta) -> String {
    let record = LogRecord::Header(meta.clone());
    let mut line = serde_json::to_string(&record).expect("SessionMeta always serializes to JSON");
    line.push('\n');
    line
}

/// Decodes a header line. Returns `CodecError::WrongLineKind` if the line
/// decodes cleanly but is not a header (i.e. `kind != "header"`).
pub fn decode_header(line: &str) -> Result<SessionMeta, CodecError> {
    match decode_line(line)? {
        Line::Header(meta) => Ok(meta),
        Line::Record { .. } => Err(CodecError::WrongLineKind {
            expected: "header",
            got: "record",
        }),
    }
}

/// Encodes one log record as a single `\n`-terminated JSON line whose
/// top-level object carries `"seq"` (authoritative — see the module-level
/// "seq resolution" note) and `"kind"`. Never fails: see the module-level
/// note for why this is infallible despite the spec's prose describing a
/// `CodecError` on mismatch.
///
/// Calling this with `LogRecord::Header` is a caller error (headers have no
/// `seq`; use [`encode_header`] instead) — the `seq` parameter would still
/// be spliced in, producing a header line with a spurious `"seq"` key, so
/// callers must not do this.
pub fn encode_record(rec: &LogRecord, seq: LogSeq) -> String {
    debug_assert!(
        !matches!(rec, LogRecord::Header(_)),
        "encode_record called with Header; use encode_header"
    );
    debug_assert_eq!(
        rec.seq(),
        Some(seq),
        "encode_record: seq param disagrees with the record's own seq — the \
         caller (store append path) has a seq-discipline bug"
    );
    let mut value = serde_json::to_value(rec).expect("LogRecord always serializes to JSON");
    match value.as_object_mut() {
        Some(obj) => {
            obj.insert(
                "seq".to_string(),
                serde_json::to_value(seq).expect("LogSeq always serializes to JSON"),
            );
        }
        None => unreachable!(
            "every LogRecord variant serializes to a JSON object under the current schema"
        ),
    }
    let mut line = serde_json::to_string(&value).expect("Value always serializes to JSON");
    line.push('\n');
    line
}

/// Decodes one record line, returning `(seq, rec)`. Returns
/// `CodecError::WrongLineKind` if the line decodes cleanly but is a header.
pub fn decode_record(line: &str) -> Result<(LogSeq, LogRecord), CodecError> {
    match decode_line(line)? {
        Line::Record { seq, rec } => Ok((seq, rec)),
        Line::Header(_) => Err(CodecError::WrongLineKind {
            expected: "record",
            got: "header",
        }),
    }
}

/// Decodes one JSONL session log line into a [`Line`], dispatching on the
/// top-level `"kind"` key: `"header"` decodes as `SessionMeta`, anything
/// else decodes as a sequenced `LogRecord`.
///
/// A trailing `\n` (if present) is trimmed before parsing; interior
/// newlines are not handled specially (a valid line has none).
///
/// Errors:
/// - `CodecError::Json` — the line is not valid JSON, or does not match the
///   `SessionMeta`/`LogRecord` schema.
/// - `CodecError::MissingKind` — the parsed JSON object has no `"kind"`
///   key.
/// - `CodecError::MissingSeq` — `kind != "header"` and the parsed JSON
///   object has no `"seq"` key.
pub fn decode_line(line: &str) -> Result<Line, CodecError> {
    let trimmed = line.trim_end_matches('\n');
    let value: serde_json::Value = serde_json::from_str(trimmed)?;
    let kind = value
        .get("kind")
        .and_then(|k| k.as_str())
        .ok_or(CodecError::MissingKind)?;

    if kind == "header" {
        let meta: SessionMeta = serde_json::from_value(value)?;
        Ok(Line::Header(meta))
    } else {
        if value.get("seq").is_none() {
            return Err(CodecError::MissingSeq);
        }
        let rec: LogRecord = serde_json::from_value(value)?;
        let seq = rec.seq().ok_or(CodecError::MissingSeq)?;
        Ok(Line::Record { seq, rec })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use conway_core::ids::{AgentId, SessionId};
    use conway_core::log::SubagentMode;
    use conway_core::log::{ForkOrigin, SessionStatus};
    use std::path::PathBuf;

    fn ts() -> DateTime<Utc> {
        "2026-07-20T00:00:00Z".parse().unwrap()
    }

    fn sample_meta() -> SessionMeta {
        SessionMeta {
            id: SessionId::new(),
            agent_id: AgentId::new(),
            origin: Some(ForkOrigin {
                parent: SessionId::new(),
                at_seq: LogSeq(142),
                mode: SubagentMode::Fork,
            }),
            agent_def: Some("reviewer".into()),
            role: Some(conway_core::ids::RoleAlias::new("coder")),
            created: ts(),
            cwd: PathBuf::from("/tmp/project"),
            labels: vec!["x".into()],
            status: SessionStatus::Active,
            ephemeral: false,
            ask_origin: None,
        }
    }

    #[test]
    fn encode_header_round_trips() {
        let meta = sample_meta();
        let line = encode_header(&meta);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let back = decode_header(&line).unwrap();
        assert_eq!(back, meta);
    }

    #[test]
    fn decode_header_on_architecture_5_1_shaped_line() {
        let sid = SessionId::new();
        let parent = SessionId::new();
        let agent = AgentId::new();
        let line = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":{{"parent":"{parent}","at_seq":142,"mode":"fork"}},"agent_def":"reviewer","role":"coder","cwd":"/tmp/p","status":"active"}}
"#
        );
        let meta = decode_header(&line).unwrap();
        assert_eq!(meta.id, sid);
        assert_eq!(meta.agent_id, agent);
        assert_eq!(
            meta.origin,
            Some(ForkOrigin {
                parent,
                at_seq: LogSeq(142),
                mode: SubagentMode::Fork,
            })
        );
    }

    #[test]
    fn decode_header_rejects_record_line() {
        let rec = LogRecord::UserTurn {
            seq: LogSeq(0),
            ts: ts(),
            text: "hi".into(),
            prov: conway_core::provenance::Provenance::UserPrompt,
        };
        let line = encode_record(&rec, LogSeq(0));
        let err = decode_header(&line).unwrap_err();
        assert!(matches!(err, CodecError::WrongLineKind { .. }));
    }

    #[test]
    fn encode_record_emits_exactly_one_trailing_newline_and_seq_kind_keys() {
        let rec = LogRecord::UserTurn {
            seq: LogSeq(7),
            ts: ts(),
            text: "hi".into(),
            prov: conway_core::provenance::Provenance::UserPrompt,
        };
        let line = encode_record(&rec, LogSeq(7));
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(value["seq"], 7);
        assert_eq!(value["kind"], "user_turn");
    }

    /// A seq mismatch is a caller bug: loudly caught in debug builds
    /// (incremental review S1, cycle 1). Release builds keep the
    /// parameter-authoritative overwrite so a production log line is
    /// internally consistent even past a caller bug.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "seq-discipline bug")]
    fn encode_record_seq_mismatch_panics_in_debug() {
        let rec = LogRecord::UserTurn {
            seq: LogSeq(0),
            ts: ts(),
            text: "hi".into(),
            prov: conway_core::provenance::Provenance::UserPrompt,
        };
        let _ = encode_record(&rec, LogSeq(9));
    }

    #[test]
    fn decode_line_of_record_missing_seq_is_missing_seq_error() {
        let line = r#"{"kind":"user_turn","ts":"2026-07-20T00:00:00Z","text":"hi","prov":{"type":"user_prompt"}}"#;
        let err = decode_line(line).unwrap_err();
        assert!(matches!(err, CodecError::MissingSeq));
    }

    #[test]
    fn decode_line_missing_kind_is_missing_kind_error() {
        let line = r#"{"seq":0,"text":"hi"}"#;
        let err = decode_line(line).unwrap_err();
        assert!(matches!(err, CodecError::MissingKind));
    }

    #[test]
    fn decode_record_rejects_header_line() {
        let meta = sample_meta();
        let line = encode_header(&meta);
        let err = decode_record(&line).unwrap_err();
        assert!(matches!(err, CodecError::WrongLineKind { .. }));
    }

    #[test]
    fn decode_line_rejects_malformed_json() {
        let err = decode_line("not json").unwrap_err();
        assert!(matches!(err, CodecError::Json(_)));
    }
}

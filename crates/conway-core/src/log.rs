//! The append-only session log record union and session metadata.
//!
//! `conway-session` persists these as JSONL (one record per line, `kind`
//! internally tagged); every other crate reads them. Wire-format notes:
//! the header's `id`/`agent_id` fields serialize as `session`/`agent`
//! (architecture §5.1), and a `ToolResult` is flattened into its record.
//!
//! `route_reason` stays `serde_json::Value` permanently — the log stores
//! the reason as data; typed access is via `Event::ModelDecision`.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::AgentResult;
use crate::content::{ContentBlock, StopReason, ToolCall, ToolResult, Usage};
use crate::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SessionId};
use crate::provenance::{ContextReport, Provenance};

/// Fork vs spawn: the only two subagent modes (GP-02).
///
/// Defined here because [`ForkOrigin`] persists it; re-exported from
/// `agent` (WI-005) as the canonical public location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentMode {
    Fork,
    Spawn,
}

/// Where a session came from, when it was created by a fork.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForkOrigin {
    pub parent: SessionId,
    pub at_seq: LogSeq,
    pub mode: SubagentMode,
}

/// Session lifecycle status.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Completed,
    Failed,
    Cancelled,
}

/// The session header: the first line of every session file.
///
/// Serde note: `id` and `agent_id` serialize as `session` and `agent` to
/// match the §5.1 wire format.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMeta {
    #[serde(rename = "session")]
    pub id: SessionId,
    #[serde(rename = "agent")]
    pub agent_id: AgentId,
    pub origin: Option<ForkOrigin>,
    pub agent_def: Option<String>,
    pub role: Option<RoleAlias>,
    pub created: DateTime<Utc>,
    pub cwd: PathBuf,
    #[serde(default)]
    pub labels: Vec<String>,
    pub status: SessionStatus,
}

/// Filter for session listing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFilter {
    pub agent_def: Option<String>,
    pub label: Option<String>,
    pub status: Option<SessionStatus>,
    pub parent: Option<SessionId>,
    pub limit: Option<usize>,
}

/// One line of the append-only session log.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LogRecord {
    Header(SessionMeta),
    UserTurn {
        seq: LogSeq,
        ts: DateTime<Utc>,
        text: String,
        prov: Provenance,
    },
    Assistant {
        seq: LogSeq,
        ts: DateTime<Utc>,
        content: Vec<ContentBlock>,
        model: ModelRef,
        route_reason: serde_json::Value,
        usage: Usage,
        stop: StopReason,
    },
    #[serde(rename = "tool_call")]
    ToolCallRecord {
        seq: LogSeq,
        ts: DateTime<Utc>,
        call: ToolCall,
    },
    #[serde(rename = "tool_result")]
    ToolResultRecord {
        seq: LogSeq,
        ts: DateTime<Utc>,
        #[serde(flatten)]
        result: ToolResult,
    },
    ForkDirective {
        seq: LogSeq,
        ts: DateTime<Utc>,
        text: String,
        by: AgentId,
        prov: Provenance,
    },
    ParentSteer {
        seq: LogSeq,
        ts: DateTime<Utc>,
        text: String,
        from: AgentId,
        parent_seq: LogSeq,
        prov: Provenance,
    },
    SystemNote {
        seq: LogSeq,
        ts: DateTime<Utc>,
        text: String,
        reason: String,
        prov: Provenance,
    },
    #[serde(rename = "agent_result")]
    AgentResultRecord {
        seq: LogSeq,
        ts: DateTime<Utc>,
        result: AgentResult,
    },
    #[serde(rename = "context_report")]
    ContextReportRecord {
        seq: LogSeq,
        ts: DateTime<Utc>,
        report: ContextReport,
    },
}

impl LogRecord {
    /// The record's sequence number. Headers have none.
    pub fn seq(&self) -> Option<LogSeq> {
        match self {
            LogRecord::Header(_) => None,
            LogRecord::UserTurn { seq, .. }
            | LogRecord::Assistant { seq, .. }
            | LogRecord::ToolCallRecord { seq, .. }
            | LogRecord::ToolResultRecord { seq, .. }
            | LogRecord::ForkDirective { seq, .. }
            | LogRecord::ParentSteer { seq, .. }
            | LogRecord::SystemNote { seq, .. }
            | LogRecord::AgentResultRecord { seq, .. }
            | LogRecord::ContextReportRecord { seq, .. } => Some(*seq),
        }
    }

    /// The serialized `kind` tag for this record.
    pub fn kind_str(&self) -> &'static str {
        match self {
            LogRecord::Header(_) => "header",
            LogRecord::UserTurn { .. } => "user_turn",
            LogRecord::Assistant { .. } => "assistant",
            LogRecord::ToolCallRecord { .. } => "tool_call",
            LogRecord::ToolResultRecord { .. } => "tool_result",
            LogRecord::ForkDirective { .. } => "fork_directive",
            LogRecord::ParentSteer { .. } => "parent_steer",
            LogRecord::SystemNote { .. } => "system_note",
            LogRecord::AgentResultRecord { .. } => "agent_result",
            LogRecord::ContextReportRecord { .. } => "context_report",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{tool_call, tool_result};
    use super::*;

    fn ts() -> DateTime<Utc> {
        "2026-07-20T00:00:00Z".parse().unwrap()
    }

    #[test]
    fn kind_tags_are_exact() {
        let session = SessionId::new();
        let agent = AgentId::new();
        let meta = SessionMeta {
            id: session,
            agent_id: agent,
            origin: None,
            agent_def: None,
            role: None,
            created: ts(),
            cwd: PathBuf::from("/tmp"),
            labels: vec![],
            status: SessionStatus::Active,
        };
        let records: Vec<(LogRecord, &str)> = vec![
            (LogRecord::Header(meta.clone()), "header"),
            (
                LogRecord::UserTurn {
                    seq: LogSeq(0),
                    ts: ts(),
                    text: "hi".into(),
                    prov: Provenance::UserPrompt,
                },
                "user_turn",
            ),
            (
                LogRecord::Assistant {
                    seq: LogSeq(1),
                    ts: ts(),
                    content: vec![],
                    model: "anthropic/claude-sonnet-4-6".parse().unwrap(),
                    route_reason: serde_json::json!({"AliasPrimary": {"alias": "coder"}}),
                    usage: Usage::default(),
                    stop: StopReason::EndTurn,
                },
                "assistant",
            ),
            (
                LogRecord::ToolCallRecord {
                    seq: LogSeq(2),
                    ts: ts(),
                    call: tool_call("tc_1"),
                },
                "tool_call",
            ),
            (
                LogRecord::ToolResultRecord {
                    seq: LogSeq(3),
                    ts: ts(),
                    result: tool_result("tc_1"),
                },
                "tool_result",
            ),
            (
                LogRecord::ForkDirective {
                    seq: LogSeq(0),
                    ts: ts(),
                    text: "review the diff".into(),
                    by: AgentId::new(),
                    prov: Provenance::ForkDirective { by: AgentId::new() },
                },
                "fork_directive",
            ),
            (
                LogRecord::ParentSteer {
                    seq: LogSeq(4),
                    ts: ts(),
                    text: "skip tests dir".into(),
                    from: AgentId::new(),
                    parent_seq: LogSeq(150),
                    prov: Provenance::ParentSteer {
                        from: AgentId::new(),
                        parent_seq: LogSeq(150),
                    },
                },
                "parent_steer",
            ),
            (
                LogRecord::SystemNote {
                    seq: LogSeq(5),
                    ts: ts(),
                    text: "repeated step".into(),
                    reason: "repeated_step".into(),
                    prov: Provenance::SystemNote {
                        reason: "repeated_step".into(),
                    },
                },
                "system_note",
            ),
            (
                LogRecord::AgentResultRecord {
                    seq: LogSeq(6),
                    ts: ts(),
                    result: crate::agent::AgentResult::new(
                        AgentId::new(),
                        SessionId::new(),
                        crate::agent::ResultStatus::Completed,
                        "done",
                    ),
                },
                "agent_result",
            ),
            (
                LogRecord::ContextReportRecord {
                    seq: LogSeq(7),
                    ts: ts(),
                    report: ContextReport {
                        agent_id: AgentId::new(),
                        turn: 1,
                        tokenizer: "cl100k_base".into(),
                        segments: vec![],
                        total_tokens_est: 0,
                    },
                },
                "context_report",
            ),
        ];
        for (record, expected) in &records {
            let value = serde_json::to_value(record).unwrap();
            assert_eq!(&value["kind"], expected, "tag for {record:?}");
            assert_eq!(record.kind_str(), *expected);
            let back: LogRecord = serde_json::from_value(value).unwrap();
            assert_eq!(&back, record);
        }
    }

    #[test]
    fn header_uses_session_and_agent_keys_and_seq_is_none() {
        let meta = SessionMeta {
            id: SessionId::new(),
            agent_id: AgentId::new(),
            origin: Some(ForkOrigin {
                parent: SessionId::new(),
                at_seq: LogSeq(142),
                mode: SubagentMode::Fork,
            }),
            agent_def: Some("reviewer".into()),
            role: Some(RoleAlias::new("coder")),
            created: ts(),
            cwd: PathBuf::from("/tmp/project"),
            labels: vec!["x".into()],
            status: SessionStatus::Active,
        };
        let record = LogRecord::Header(meta.clone());
        assert_eq!(record.seq(), None);
        let value = serde_json::to_value(&record).unwrap();
        assert!(
            value.get("session").is_some(),
            "header must use `session` key"
        );
        assert!(value.get("agent").is_some(), "header must use `agent` key");
        assert_eq!(value["origin"]["mode"], "fork");
        assert_eq!(value["origin"]["at_seq"], 142);
    }

    /// §5.1-shaped example lines (valid ULIDs substituted for the doc's
    /// illustrative ids, `ts` included; the doc elides both).
    #[test]
    fn architecture_5_1_shaped_lines_deserialize() {
        let sid = SessionId::new();
        let parent = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":{{"parent":"{parent}","at_seq":142,"mode":"fork"}},"agent_def":"reviewer","role":"coder","cwd":"/tmp/p","status":"active"}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        assert_eq!(record.kind_str(), "header");

        let fork = format!(
            r#"{{"kind":"fork_directive","seq":0,"ts":"2026-07-20T00:00:00Z","text":"Now review the diff for races","by":"{agent}","prov":{{"type":"fork_directive","by":"{agent}"}}}}"#
        );
        let record: LogRecord = serde_json::from_str(&fork).unwrap();
        assert_eq!(record.seq(), Some(LogSeq(0)));

        let assistant = r#"{"kind":"assistant","seq":1,"ts":"2026-07-20T00:00:00Z","content":[{"type":"text","text":"ok"}],"model":{"backend":"anthropic","model":"claude-sonnet-4-6"},"route_reason":{"AliasPrimary":{"alias":"coder"}},"usage":{"input_tokens":1,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"reasoning_tokens":0},"stop":"end_turn"}"#;
        let record: LogRecord = serde_json::from_str(assistant).unwrap();
        assert_eq!(record.kind_str(), "assistant");

        let tool_result_line = r#"{"kind":"tool_result","seq":2,"ts":"2026-07-20T00:00:00Z","call_id":"tc_1","tool":"read","blocks":[],"is_error":false,"truncated":{"policy":"head_tail","head_bytes":1000,"tail_bytes":1000,"original_bytes":918233,"kept_bytes":2000}}"#;
        let record: LogRecord = serde_json::from_str(tool_result_line).unwrap();
        assert_eq!(record.kind_str(), "tool_result");

        let steer = format!(
            r#"{{"kind":"parent_steer","seq":3,"ts":"2026-07-20T00:00:00Z","text":"skip the tests dir","from":"{agent}","parent_seq":150,"prov":{{"type":"parent_steer","from":"{agent}","parent_seq":150}}}}"#
        );
        let record: LogRecord = serde_json::from_str(&steer).unwrap();
        assert_eq!(record.kind_str(), "parent_steer");

        let context_report = format!(
            r#"{{"kind":"context_report","seq":4,"ts":"2026-07-20T00:00:00Z","report":{{"agent_id":"{agent}","turn":1,"tokenizer":"cl100k_base","segments":[],"total_tokens_est":0}}}}"#
        );
        let record: LogRecord = serde_json::from_str(&context_report).unwrap();
        assert_eq!(record.kind_str(), "context_report");
    }

    #[test]
    fn session_filter_default_is_empty() {
        let f = SessionFilter::default();
        assert!(f.agent_def.is_none() && f.limit.is_none());
    }

    /// Documents current behavior at the forward-compat boundary: an unknown
    /// `kind` from a future writer is a clear `Err`, not a panic or silent
    /// misparse. `conway-session`'s tolerate-unknown-records design builds on
    /// knowing this is where the error surfaces.
    #[test]
    fn unknown_kind_is_a_clear_error() {
        let line = r#"{"kind":"future_variant","seq":9,"ts":"2026-07-20T00:00:00Z"}"#;
        let err = serde_json::from_str::<LogRecord>(line).unwrap_err();
        assert!(
            err.to_string().contains("future_variant"),
            "error was: {err}"
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::ids::ToolName;

    pub fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            call_id: id.into(),
            name: ToolName::new("read"),
            arguments: serde_json::json!({"path": "a.txt"}),
        }
    }

    pub fn tool_result(id: &str) -> ToolResult {
        ToolResult {
            call_id: id.into(),
            tool: ToolName::new("read"),
            blocks: vec![],
            is_error: false,
            truncated: None,
        }
    }
}

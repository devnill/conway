//! The typed provenance enum that makes context composition inspectable:
//! every [`PromptSegment`](crate::segment::PromptSegment)
//! carries one of these variants so the IDE's provenance tree and
//! `ContextBuilder`'s fixed segment ordering (architecture §5.3) can both
//! render and reason about *why* a byte of context is present.
//!
//! Internally tagged on field `type`, snake_case tag values. This is the
//! wire format for the `prov` field on several [`LogRecord`](crate::log::LogRecord)
//! variants and for [`ContextReportEntry::provenance`].

use serde::{Deserialize, Serialize};

use crate::ids::{AgentId, LogSeq, SegmentId, SeqRange, SessionId, ToolName};

/// Why a segment of assembled context exists.
///
/// Eleven variants: the original nine of architecture §5.3, plus
/// [`Provenance::MergedAsk`] (B4), plus
/// [`Provenance::ChildResult`].
/// Adding another is a breaking wire-format change and must be treated as
/// such.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Provenance {
    /// The user's own turn text.
    UserPrompt,
    /// The system prompt sourced from an `AgentDef`.
    AgentDef { name: String },
    /// A skill fragment injected into context.
    Skill { name: String },
    /// The tool schema set, identified by the blake3 hex hash of the sorted
    /// `ToolSpec` set.
    ToolRegistry { hash: String },
    /// A verbatim prefix inherited from a parent session at fork time.
    Inherited {
        from: SessionId,
        seq_range: SeqRange,
    },
    /// The directive text a forking parent attaches after the inherited
    /// prefix.
    ForkDirective { by: AgentId },
    /// A steer message drained from the mailbox at a turn boundary.
    ParentSteer { from: AgentId, parent_seq: LogSeq },
    /// The output of a tool invocation.
    ToolResult { call_id: String, tool: ToolName },
    /// A runtime-authored note (e.g. repeated-step detection).
    SystemNote { reason: String },
    /// A question merged into the parent's log by `Conway::pull_in` (B4)
    /// when an ephemeral `/ask` child is folded back into its asker. The
    /// child's `ForkDirective` head record (and any genuine `UserTurn`s in
    /// the child) lands in the parent as a `UserTurn` re-stamped with this
    /// variant, so the merge origin — the purged child's `SessionId` —
    /// stays explicit and inspectable even after the child's
    /// own session file is gone.
    MergedAsk { from: SessionId },
    /// A child's terminal `AgentResult`, recorded into the PARENT's own log
    /// by `mailbox::classify` (`conway-runtime`) when a drained
    /// `AgentMessage::Result` produces `DrainEffect::Persist` --
    /// `LogRecord::ChildResultRecord` is the record. `from` is the
    /// finishing child's `AgentId`. Exists so a child's own output is never
    /// misattributed as parent-authored: a `ChildResult` segment is
    /// unambiguously marked as having come from elsewhere in the tree, the
    /// same way `ParentSteer` marks a steer as not self-authored.
    ChildResult { from: AgentId },
}

/// Where a segment sits in the fixed §5.3 ordering: `Static` segments are
/// identical across siblings, `Inherited` is the fork-boundary prefix, and
/// `Volatile` is everything appended after it. `ContextBuilder` sorts
/// segments by this ordering.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentTier {
    Static,
    Inherited,
    Volatile,
}

impl Provenance {
    /// `true` for `AgentDef`, `Skill`, and `ToolRegistry` — the segments that
    /// are byte-identical across sibling agents (architecture §5.3).
    pub fn is_static(&self) -> bool {
        matches!(
            self,
            Provenance::AgentDef { .. }
                | Provenance::Skill { .. }
                | Provenance::ToolRegistry { .. }
        )
    }

    /// The §5.3 tier this provenance belongs to: `Static` < `Inherited` <
    /// `Volatile`.
    pub fn tier(&self) -> SegmentTier {
        match self {
            Provenance::AgentDef { .. }
            | Provenance::Skill { .. }
            | Provenance::ToolRegistry { .. } => SegmentTier::Static,
            Provenance::Inherited { .. } => SegmentTier::Inherited,
            Provenance::UserPrompt
            | Provenance::ForkDirective { .. }
            | Provenance::ParentSteer { .. }
            | Provenance::ToolResult { .. }
            | Provenance::SystemNote { .. }
            | Provenance::MergedAsk { .. }
            | Provenance::ChildResult { .. } => SegmentTier::Volatile,
        }
    }
}

/// A persisted snapshot of what context was assembled for one turn: every
/// segment's provenance, its estimated token count, and whether that count
/// is exact or estimated. Persisted alongside the turn so provenance
/// survives restart (architecture, Internal Design Notes).
///
/// Per T-9: `estimated` and `tokenizer` are mandatory — the report must
/// never present a token count as exact without naming the tokenizer that
/// produced it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextReport {
    pub agent_id: AgentId,
    pub turn: u32,
    /// The tokenizer or estimation heuristic that produced every
    /// `tokens_est` in this report — e.g. `"heuristic-chars4"` (the
    /// runtime's chars/4 estimate) or a real tokenizer name like
    /// `"cl100k_base"`. Despite the field name, this may be an
    /// estimator, not a true tokenizer (T-9): counts are estimates
    /// unless a per-entry `estimated` flag says otherwise.
    /// asserts this field (there is no separate `estimator` field).
    pub tokenizer: String,
    pub segments: Vec<ContextReportEntry>,
    pub total_tokens_est: u32,
    /// `call_id`s of tool calls the assembler removed from this turn's
    /// request because no answering result was present anywhere in it.
    ///
    /// A tool call with no result is a request every provider rejects
    /// outright, so dropping it is not optional -- but dropping it silently
    /// would make this report describe a context the model never saw. The
    /// harness does not curate context on its own initiative; where it must
    /// intervene to produce a sendable request at all, the intervention is
    /// part of the record rather than behind it.
    ///
    /// Empty for the overwhelmingly common case of a settled transcript. A
    /// non-empty list means the model no longer sees that it made those
    /// calls and may re-issue them.
    ///
    /// `#[serde(default)]`: every session log written before this field
    /// existed still decodes, with no dropped calls recorded.
    #[serde(default)]
    pub dropped: Vec<String>,
}

/// One segment's entry in a [`ContextReport`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextReportEntry {
    pub segment: SegmentId,
    pub provenance: Provenance,
    pub tokens_est: u32,
    pub estimated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_tagged() -> Vec<(Provenance, &'static str)> {
        vec![
            (Provenance::UserPrompt, "user_prompt"),
            (
                Provenance::AgentDef {
                    name: "reviewer".into(),
                },
                "agent_def",
            ),
            (
                Provenance::Skill {
                    name: "review".into(),
                },
                "skill",
            ),
            (
                Provenance::ToolRegistry {
                    hash: "deadbeef".into(),
                },
                "tool_registry",
            ),
            (
                Provenance::Inherited {
                    from: SessionId::new(),
                    seq_range: SeqRange::new(LogSeq::ZERO, Some(LogSeq(142))),
                },
                "inherited",
            ),
            (
                Provenance::ForkDirective { by: AgentId::new() },
                "fork_directive",
            ),
            (
                Provenance::ParentSteer {
                    from: AgentId::new(),
                    parent_seq: LogSeq(150),
                },
                "parent_steer",
            ),
            (
                Provenance::ToolResult {
                    call_id: "tc_1".into(),
                    tool: ToolName::new("read"),
                },
                "tool_result",
            ),
            (
                Provenance::SystemNote {
                    reason: "repeated_step".into(),
                },
                "system_note",
            ),
            (
                Provenance::MergedAsk {
                    from: SessionId::new(),
                },
                "merged_ask",
            ),
            (
                Provenance::ChildResult {
                    from: AgentId::new(),
                },
                "child_result",
            ),
        ]
    }

    #[test]
    fn tags_are_exact_for_every_variant() {
        for (prov, expected) in all_tagged() {
            let value = serde_json::to_value(&prov).unwrap();
            assert_eq!(value["type"], expected, "tag for {prov:?}");
            let back: Provenance = serde_json::from_value(value).unwrap();
            assert_eq!(back, prov);
        }
        // Eleven variants, no more, no fewer.
        assert_eq!(all_tagged().len(), 11);
    }

    #[test]
    fn deserializes_inherited_example() {
        let session = SessionId::new();
        let json = format!(
            r#"{{"type":"inherited","from":"{session}","seq_range":{{"start":0,"end":142}}}}"#
        );
        let prov: Provenance = serde_json::from_str(&json).unwrap();
        match prov {
            Provenance::Inherited { from, seq_range } => {
                assert_eq!(from, session);
                assert_eq!(seq_range, SeqRange::new(LogSeq::ZERO, Some(LogSeq(142))));
            }
            other => panic!("expected Inherited, got {other:?}"),
        }
    }

    #[test]
    fn deserializes_tool_result_example() {
        let json = r#"{"type":"tool_result","call_id":"tc_1","tool":"read"}"#;
        let prov: Provenance = serde_json::from_str(json).unwrap();
        match prov {
            Provenance::ToolResult { call_id, tool } => {
                assert_eq!(call_id, "tc_1");
                assert_eq!(tool, ToolName::new("read"));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn is_static_and_tier() {
        assert!(Provenance::AgentDef { name: "x".into() }.is_static());
        assert!(Provenance::Skill { name: "x".into() }.is_static());
        assert!(Provenance::ToolRegistry { hash: "x".into() }.is_static());
        assert!(!Provenance::UserPrompt.is_static());
        assert!(!Provenance::Inherited {
            from: SessionId::new(),
            seq_range: SeqRange::full(),
        }
        .is_static());
        assert!(!Provenance::ChildResult {
            from: AgentId::new()
        }
        .is_static());
        assert_eq!(
            Provenance::ChildResult {
                from: AgentId::new()
            }
            .tier(),
            SegmentTier::Volatile
        );

        assert_eq!(
            Provenance::AgentDef { name: "x".into() }.tier(),
            SegmentTier::Static
        );
        assert_eq!(
            Provenance::Inherited {
                from: SessionId::new(),
                seq_range: SeqRange::full(),
            }
            .tier(),
            SegmentTier::Inherited
        );
        assert_eq!(Provenance::UserPrompt.tier(), SegmentTier::Volatile);

        assert!(SegmentTier::Static < SegmentTier::Inherited);
        assert!(SegmentTier::Inherited < SegmentTier::Volatile);
    }

    #[test]
    fn context_report_round_trips() {
        let report = ContextReport {
            agent_id: AgentId::new(),
            turn: 1,
            tokenizer: "cl100k_base".into(),
            segments: vec![ContextReportEntry {
                segment: SegmentId::new(),
                provenance: Provenance::UserPrompt,
                tokens_est: 42,
                estimated: true,
            }],
            total_tokens_est: 42,
            dropped: vec!["call_9".into()],
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: ContextReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report, back);
    }
}

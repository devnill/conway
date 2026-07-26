//! The flat, `agent`-tagged event stream: the IDE's render surface and the
//! CLI's `--output-format jsonl` output format.
//!
//! Transcribed from architecture §6.5, plus three additions the architecture
//! prose names but the §6.5 listing omits: `SteerQueued { queued_since }`
//! (§6.3), `SteerDropped` (§6.2, mailbox overflow), and `Lagged { skipped }`
//! (§8's broadcast-channel guarantee: a slow consumer receives `Lagged`
//! rather than stalling the runtime).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agent::{AgentResult, MessageKind, PermissionDecisionKind};
use crate::content::{StopReason, Usage};
use crate::error::ConwayError;
use crate::ids::{
    AgentId, EndpointId, LogSeq, ModelRef, RoleAlias, SegmentId, SessionId, ToolName,
};
use crate::log::SubagentMode;
use crate::provenance::Provenance;
use crate::routing::{BreakerKind, RoutingReason};

/// One envelope on the event stream: sequencing and identity wrapped around
/// one [`Event`]. `#[serde(flatten)]` on `event` combined with
/// `#[serde(tag = "event")]` on [`Event`] produces exactly one flat JSON
/// object per line — the event is never nested under an `"event"` object
/// key.
///
/// Restates the three architecture §8 delivery guarantees so downstream
/// implementers see them at the definition site:
/// - `seq` is monotonic per session across ALL agents in that session's tree.
/// - an agent's [`Event::AgentSpawned`] precedes every other event bearing
///   that agent id.
/// - every [`Event::AgentSpawned`] is eventually followed by exactly one
///   [`Event::AgentFinished`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub session: SessionId,
    pub agent: AgentId,
    #[serde(flatten)]
    pub event: Event,
}

/// The flat, `agent`-tagged event enum: the IDE's render surface and the
/// CLI's `jsonl` output format (architecture §6.5).
///
/// A future ACP shim filters `agent == root` for one ACP session and maps
/// individual variants to ACP updates; nothing in this enum precludes that.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    AgentSpawned {
        kind: SubagentMode,
        parent: Option<AgentId>,
        agent_def: Option<String>,
        inherited_upto: Option<LogSeq>,
        /// Whether this child is an ephemeral `/ask`-style aside (decision
        /// 01KYD1TWXMZD4BT842CMJT1AED): stamped from the child's
        /// `SessionMeta::ephemeral` at `attach` time. `#[serde(default)]`
        /// keeps old JSON logs readable (C-04): a missing key deserializes to
        /// `false`, matching the pre-ephemeral semantics every non-ask fork/
        /// spawn/root already had.
        #[serde(default)]
        ephemeral: bool,
    },
    AgentProgress {
        note: String,
    },
    AgentFinished {
        result: AgentResult,
        /// See [`Event::AgentSpawned::ephemeral`]: stamped from the child
        /// node's `ephemeral` field at every emission site (the live
        /// `AgentLoop` finish and the supervisor's synthesized finish).
        #[serde(default)]
        ephemeral: bool,
    },

    TurnStarted {
        turn: u32,
    },
    ModelDecision {
        role: RoleAlias,
        chosen: ModelRef,
        reason: RoutingReason,
        attempt: u8,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    TurnFinished {
        usage: Usage,
        stop: StopReason,
    },

    ToolCallProposed {
        call_id: String,
        tool: ToolName,
        args: serde_json::Value,
    },
    PermissionRequested {
        call_id: String,
        rendered: String,
    },
    PermissionResolved {
        call_id: String,
        decision: PermissionDecisionKind,
    },
    ToolCallStarted {
        call_id: String,
    },
    ToolProgress {
        call_id: String,
        note: String,
    },
    ToolCallFinished {
        call_id: String,
        is_error: bool,
        preview: String,
    },

    ContextSegmentAdded {
        segment: SegmentId,
        provenance: Provenance,
        tokens_est: u32,
    },
    MessageSent {
        to: AgentId,
        kind: MessageKind,
    },
    /// A steer message was accepted into a mailbox but not yet drained
    /// (architecture §6.3). Not in the §6.5 listing; justified by the §6.3
    /// prose.
    SteerQueued {
        target: AgentId,
        queued_since: DateTime<Utc>,
    },
    /// A steer message was dropped because the target's mailbox was full
    /// (architecture §6.2). Not in the §6.5 listing; justified by the §6.2
    /// prose.
    SteerDropped {
        target: AgentId,
        reason: String,
    },
    RepeatedStep {
        tool: ToolName,
        prior_seq: LogSeq,
    },
    BackendDegraded {
        endpoint: EndpointId,
        breaker: BreakerKind,
        until: DateTime<Utc>,
    },
    /// A slow event-stream consumer missed events; the broadcast channel
    /// dropped `skipped` of them rather than stalling the runtime (§8). Full
    /// history is always recoverable from the session log.
    Lagged {
        skipped: u64,
    },
    Error {
        error: ConwayError,
        fatal: bool,
    },
}

/// The two boundary points in an agent's lifecycle, as projected by
/// [`Event::agent_lifecycle_kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    Start,
    End,
}

impl Event {
    /// `true` only for `Error { fatal: true, .. }`.
    pub fn is_fatal(&self) -> bool {
        matches!(self, Event::Error { fatal: true, .. })
    }

    /// Maps `AgentSpawned -> Start`, `AgentFinished -> End`, everything else
    /// to `None`.
    pub fn agent_lifecycle_kind(&self) -> Option<LifecyclePhase> {
        match self {
            Event::AgentSpawned { .. } => Some(LifecyclePhase::Start),
            Event::AgentFinished { .. } => Some(LifecyclePhase::End),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ResultStatus;
    use crate::ids::{BackendId, ModelId};

    fn ts() -> DateTime<Utc> {
        "2026-07-20T00:00:00Z".parse().unwrap()
    }

    fn model_ref() -> ModelRef {
        ModelRef {
            backend: BackendId::new("anthropic"),
            model: ModelId::new("claude-sonnet-4-6"),
        }
    }

    /// One constructed value of every `Event` variant, tagged with its
    /// expected `event` string.
    fn all_variants() -> Vec<(Event, &'static str)> {
        vec![
            (
                Event::AgentSpawned {
                    kind: SubagentMode::Fork,
                    parent: Some(AgentId::new()),
                    agent_def: Some("reviewer".into()),
                    inherited_upto: Some(LogSeq(10)),
                    ephemeral: false,
                },
                "agent_spawned",
            ),
            (
                Event::AgentProgress {
                    note: "working".into(),
                },
                "agent_progress",
            ),
            (
                Event::AgentFinished {
                    result: AgentResult::new(
                        AgentId::new(),
                        SessionId::new(),
                        ResultStatus::Completed,
                        "done",
                    ),
                    ephemeral: false,
                },
                "agent_finished",
            ),
            (Event::TurnStarted { turn: 1 }, "turn_started"),
            (
                Event::ModelDecision {
                    role: RoleAlias::new("planner"),
                    chosen: model_ref(),
                    reason: RoutingReason::PinnedByApi,
                    attempt: 0,
                },
                "model_decision",
            ),
            (Event::TextDelta { text: "hi".into() }, "text_delta"),
            (
                Event::ThinkingDelta { text: "hmm".into() },
                "thinking_delta",
            ),
            (
                Event::TurnFinished {
                    usage: Usage::default(),
                    stop: StopReason::EndTurn,
                },
                "turn_finished",
            ),
            (
                Event::ToolCallProposed {
                    call_id: "tc_1".into(),
                    tool: ToolName::new("read"),
                    args: serde_json::json!({"path": "a.txt"}),
                },
                "tool_call_proposed",
            ),
            (
                Event::PermissionRequested {
                    call_id: "tc_1".into(),
                    rendered: "read a.txt".into(),
                },
                "permission_requested",
            ),
            (
                Event::PermissionResolved {
                    call_id: "tc_1".into(),
                    decision: PermissionDecisionKind::AllowOnce,
                },
                "permission_resolved",
            ),
            (
                Event::ToolCallStarted {
                    call_id: "tc_1".into(),
                },
                "tool_call_started",
            ),
            (
                Event::ToolProgress {
                    call_id: "tc_1".into(),
                    note: "50%".into(),
                },
                "tool_progress",
            ),
            (
                Event::ToolCallFinished {
                    call_id: "tc_1".into(),
                    is_error: false,
                    preview: "ok".into(),
                },
                "tool_call_finished",
            ),
            (
                Event::ContextSegmentAdded {
                    segment: SegmentId::new(),
                    provenance: Provenance::UserPrompt,
                    tokens_est: 10,
                },
                "context_segment_added",
            ),
            (
                Event::MessageSent {
                    to: AgentId::new(),
                    kind: MessageKind::Progress,
                },
                "message_sent",
            ),
            (
                Event::SteerQueued {
                    target: AgentId::new(),
                    queued_since: ts(),
                },
                "steer_queued",
            ),
            (
                Event::SteerDropped {
                    target: AgentId::new(),
                    reason: "mailbox full".into(),
                },
                "steer_dropped",
            ),
            (
                Event::RepeatedStep {
                    tool: ToolName::new("read"),
                    prior_seq: LogSeq(3),
                },
                "repeated_step",
            ),
            (
                Event::BackendDegraded {
                    endpoint: EndpointId::new("anthropic-1"),
                    breaker: BreakerKind::Transport,
                    until: ts(),
                },
                "backend_degraded",
            ),
            (Event::Lagged { skipped: 5 }, "lagged"),
            (
                Event::Error {
                    error: ConwayError::Config { detail: "x".into() },
                    fatal: true,
                },
                "error",
            ),
        ]
    }

    #[test]
    fn every_variant_constructs_and_round_trips_with_exact_tag() {
        let variants = all_variants();
        // Twenty-two variants: the twenty from architecture §6.5 (Envelope's
        // inline `event` field dropped from the count) plus `Lagged`,
        // `SteerDropped`, and the extra `SteerQueued` field, minus the
        // §6.5 listing's un-additioned `SteerQueued` — i.e. every variant
        // currently defined on `Event`.
        assert_eq!(variants.len(), 22);
        for (event, expected_tag) in variants {
            let value = serde_json::to_value(&event).unwrap();
            assert_eq!(value["event"], expected_tag, "tag for {event:?}");
            let back: Event = serde_json::from_value(value).unwrap();
            assert_eq!(back, event);
        }
    }

    #[test]
    fn envelope_serializes_to_one_flat_line_not_nested() {
        let envelope = Envelope {
            seq: 1,
            ts: ts(),
            session: SessionId::new(),
            agent: AgentId::new(),
            event: Event::TurnStarted { turn: 1 },
        };
        let json = serde_json::to_string(&envelope).unwrap();
        assert!(!json.contains('\n'), "envelope JSON must be single-line");

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("seq"));
        assert!(obj.contains_key("ts"));
        assert!(obj.contains_key("session"));
        assert!(obj.contains_key("agent"));
        assert!(obj.contains_key("event"));
        // Flattened, not nested: `event` is the tag string, and the
        // variant's own fields sit directly on the top-level object.
        assert_eq!(obj["event"], "turn_started");
        assert!(!obj["event"].is_object());
        assert_eq!(obj["turn"], 1);

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, envelope);
    }

    #[test]
    fn is_fatal_only_for_error_with_fatal_true() {
        assert!(Event::Error {
            error: ConwayError::Config { detail: "x".into() },
            fatal: true,
        }
        .is_fatal());
        assert!(!Event::Error {
            error: ConwayError::Config { detail: "x".into() },
            fatal: false,
        }
        .is_fatal());
        for (event, _) in all_variants() {
            if matches!(event, Event::Error { fatal: true, .. }) {
                assert!(event.is_fatal());
            } else {
                assert!(!event.is_fatal(), "unexpected fatal for {event:?}");
            }
        }
    }

    #[test]
    fn agent_lifecycle_kind_maps_spawned_and_finished_only() {
        let spawned = Event::AgentSpawned {
            kind: SubagentMode::Spawn,
            parent: None,
            agent_def: None,
            inherited_upto: None,
            ephemeral: false,
        };
        assert_eq!(spawned.agent_lifecycle_kind(), Some(LifecyclePhase::Start));

        let finished = Event::AgentFinished {
            result: AgentResult::new(
                AgentId::new(),
                SessionId::new(),
                ResultStatus::Completed,
                "done",
            ),
            ephemeral: false,
        };
        assert_eq!(finished.agent_lifecycle_kind(), Some(LifecyclePhase::End));

        for (event, tag) in all_variants() {
            if tag == "agent_spawned" || tag == "agent_finished" {
                continue;
            }
            assert_eq!(
                event.agent_lifecycle_kind(),
                None,
                "unexpected lifecycle kind for {event:?}"
            );
        }
    }

    /// C-04 backward-compat: an old JSON log line for `agent_spawned` (and,
    /// symmetrically, `agent_finished`) written before `ephemeral` existed
    /// deserializes with `ephemeral: false`, and a round trip of an
    /// ephemeral-flagged value preserves it.
    #[test]
    fn agent_spawned_and_finished_ephemeral_field_round_trips_and_defaults_false_when_absent() {
        // Spawned: absent key -> false.
        let spawned_json = serde_json::json!({
            "event": "agent_spawned",
            "kind": "fork",
            "parent": null,
            "agent_def": null,
            "inherited_upto": null,
        });
        let back: Event = serde_json::from_value(spawned_json).unwrap();
        match back {
            Event::AgentSpawned { ephemeral, .. } => assert!(!ephemeral),
            other => panic!("expected AgentSpawned, got {other:?}"),
        }

        // Spawned: explicit true round-trips.
        let spawned_true = Event::AgentSpawned {
            kind: SubagentMode::Fork,
            parent: Some(AgentId::new()),
            agent_def: None,
            inherited_upto: None,
            ephemeral: true,
        };
        let value = serde_json::to_value(&spawned_true).unwrap();
        assert_eq!(value["ephemeral"], true);
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(back, spawned_true);

        // Finished: build from a real value, then strip the `ephemeral` key
        // to simulate an old log line written before the field existed.
        let finished = Event::AgentFinished {
            result: AgentResult::new(
                AgentId::new(),
                SessionId::new(),
                ResultStatus::Completed,
                "done",
            ),
            ephemeral: false,
        };
        let mut value = serde_json::to_value(&finished).unwrap();
        assert!(value
            .as_object_mut()
            .unwrap()
            .remove("ephemeral")
            .is_some());
        let back: Event = serde_json::from_value(value).unwrap();
        match back {
            Event::AgentFinished { ephemeral, .. } => assert!(!ephemeral),
            other => panic!("expected AgentFinished, got {other:?}"),
        }

        // Finished: explicit true round-trips.
        let finished_true = Event::AgentFinished {
            result: AgentResult::new(
                AgentId::new(),
                SessionId::new(),
                ResultStatus::Completed,
                "done",
            ),
            ephemeral: true,
        };
        let value = serde_json::to_value(&finished_true).unwrap();
        assert_eq!(value["ephemeral"], true);
        let back: Event = serde_json::from_value(value).unwrap();
        assert_eq!(back, finished_true);
    }
}

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
/// - `seq` is monotonic per session across ALL agents in that session's
///   tree, for as long as the emitting process's in-memory counter for that
///   session stays live. The counter is reclaimed (and, if ever reused,
///   restarts at 0) once a spawned/forked child's own terminal
///   `Event::AgentFinished` is observed, or once an `/ask`-style ephemeral
///   child finishes without being promoted -- see `conway-runtime`'s
///   `EventBus`'s own doc (`events.rs`) for exactly which sessions this
///   applies to and why nothing in this workspace depends on `seq` staying
///   gap-free or non-repeating beyond that. This guarantee makes no claim
///   across a resume in a fresh process either: the counter is never
///   persisted or reseeded from stored history.
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
        /// Whether this child is an ephemeral `/ask`-style aside -- stamped from the child's
        /// `SessionMeta::ephemeral` at `attach` time. `#[serde(default)]`
        /// keeps old JSON logs readable: a missing key deserializes to
        /// `false`, matching the pre-ephemeral semantics every non-ask fork/
        /// spawn/root already had.
        #[serde(default)]
        ephemeral: bool,
    },
    AgentProgress {
        note: String,
    },
    /// A user's own turn text -- the typed counterpart the doc comment on
    /// `conway`'s `record_to_event` used to say did not exist for
    /// `LogRecord::UserTurn` (that gap is what this variant closes). Carries
    /// `prov` (mirroring `LogRecord::UserTurn::prov` byte-for-byte) so a
    /// consumer can tell a genuine typed-in prompt (`Provenance::UserPrompt`)
    /// apart from a merged `/ask` question folded back in by `Conway::
    /// pull_in` (`Provenance::MergedAsk`) without any string-matching
    /// -- the envelope's own `agent`/`seq`/`ts` already say
    /// which agent and which position in that agent's log this turn is, so
    /// no separate turn-number field is needed here.
    ///
    /// Emitted live by `conway-runtime`'s `Runtime::prompt` (every
    /// `SessionHandle::prompt`/`prompt_agent` call), `Runtime::start_root`
    /// (a root created with an initial prompt), and `subagent.rs`'s `start`
    /// for a `Spawn` whose `SubagentSpec::prompt` is non-empty -- always
    /// AFTER the owning agent's own `Event::AgentSpawned` (see this item's
    /// completion notes on why the `Spawn`-with-prompt case needs its
    /// emission moved past `attach`, unlike `Runtime::prompt`'s already-
    /// attached target). Replayed faithfully (no more `AgentProgress`
    /// fallback) by `record_to_event`'s `LogRecord::UserTurn` arm.
    UserTurn {
        text: String,
        prov: Provenance,
    },
    AgentFinished {
        result: AgentResult,
        /// See [`Event::AgentSpawned::ephemeral`]: stamped from the child
        /// node's `ephemeral` field at every emission site (the live
        /// `AgentLoop` finish and the supervisor's synthesized finish).
        #[serde(default)]
        ephemeral: bool,
    },
    /// An ephemeral `/ask`-style agent was promoted to persistent (the
    /// facade `Conway::promote` — the `/ask` modal's "keep" fate). Emitted
    /// exactly once per promotion, only AFTER both the durable session-header
    /// rewrite (`SessionStore::set_ephemeral`) and the live-tree flag flip
    /// (`AgentTree::set_ephemeral`) have succeeded: this event is the "all
    /// three flips are done" signal, so a UI may flip its own cached copy of
    /// the flag unconditionally on receipt (no optimistic pre-flip). Carries
    /// no fields of its own — the envelope's `agent` names the promoted
    /// agent and its `session` that agent's own session. Fieldless, so old
    /// and new logs agree on its shape; additive under `#[non_exhaustive]`
    /// (a new variant never disturbs existing ones' deserialization).
    AgentPromoted {},

    TurnStarted {
        turn: u32,
    },
    ModelDecision {
        role: RoleAlias,
        chosen: ModelRef,
        reason: RoutingReason,
        attempt: u8,
    },
    /// A stringified tool-call argument was coerced to its parsed JSON
    /// value -- board item `01M23SDCE6T85Z48CRQ8NBY6PV` step 1
    /// (`conway_plugin_backends::tool_calls::validate::SchemaValidator::
    /// validate`'s own doc has the narrow rule this fires under: the value
    /// is a string, that string parses as JSON, and the parsed value THEN
    /// validates against the exact schema branch that rejected the
    /// string). The operator's own ruling requires every firing to be
    /// "recorded... countable... a coercion nobody can count is the one
    /// this decision would have rejected" -- this event is what makes that
    /// durable and visible in a default run, mirroring
    /// [`Event::StreamRestarted`] (a recovery a subscriber would otherwise
    /// never know about) and [`Event::ModelDecision`] (the same shape for
    /// routing) exactly. `tool`/`call_id` name the call the argument
    /// belonged to; `argument_path` is the RFC 6901 JSON Pointer that was
    /// rejected before coercion (e.g. `/budget`). **Streaming-backend
    /// paths only** today: `StreamChunk::ToolArgumentCoerced`'s own doc
    /// discloses the non-streaming `generate()` gap this does not yet
    /// close.
    ToolArgumentCoerced {
        tool: ToolName,
        call_id: String,
        argument_path: String,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    /// A mid-stream `Transport`/`ServerError` (or a stream that ended
    /// without a `Done` chunk) discarded a partial reply and is about to
    /// retry the SAME candidate (board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`;
    /// `conway_runtime::attempt::AttemptEngine::execute`'s per-candidate
    /// loop, before it ever advances the fallback chain — a pre-stream
    /// failure, `RateLimit`, or a `Fatal`/`RequestIncompatible` class never
    /// produces this event; those keep advancing the chain as before).
    /// Emitted BEFORE the retry's own `Event::ModelDecision`, so a
    /// subscriber sees this first. `attempt` is the 1-based ordinal of the
    /// upcoming retry (`2` for the first retry, `3` for the second and
    /// last), matching `Event::ModelDecision::attempt`'s own counting
    /// convention one step later in the same sequence.
    /// `discarded_text_chars`/`discarded_thinking_chars` are this failed
    /// attempt's own running totals (already emitted to the bus as
    /// `TextDelta`/`ThinkingDelta`, never persisted — the assistant record
    /// is only written after a `Done`) so a renderer can truncate its
    /// in-progress view back to before this attempt's own deltas: the TUI
    /// (`conway-cli`'s `AppState::apply`) truncates the in-progress
    /// assistant/reasoning entry and appends a discard notice; the `text`
    /// one-shot renderer prints a newline plus a stderr diagnostic (partial
    /// stdout text cannot be unprinted — `docs/scripting.md`'s "Streaming"
    /// section says so, and points a script that needs clean stdout at
    /// `--output-format json` instead); `jsonl` needs no dedicated arm, it
    /// forwards every envelope uniformly already.
    StreamRestarted {
        agent_id: AgentId,
        attempt: u32,
        discarded_text_chars: usize,
        discarded_thinking_chars: usize,
    },
    TurnFinished {
        usage: Usage,
        stop: StopReason,
    },
    /// A `keep_alive` agent's current user turn was ended BY THE HARNESS,
    /// not by the model producing a final reply: a turn-scoped budget
    /// dimension (`max_steps`/`max_tool_calls`) reached its ceiling
    /// mid-turn. Unlike `Event::AgentFinished { result: ResultStatus::
    /// BudgetExceeded, .. }`, this does NOT end the session -- the harness
    /// performs the exact same turn-boundary reset a natural completion
    /// would (`conway_runtime::agent_loop::AgentLoop`'s shared
    /// `end_keep_alive_turn`) and returns to idling for the operator's next
    /// prompt. `agent_id` names the (root) agent whose turn was aborted --
    /// carried explicitly rather than relied on from the envelope alone,
    /// mirroring `Event::StreamRestarted::agent_id`'s own precedent, since a
    /// consumer filtering this specific event by agent should not have to
    /// reach into the envelope for it. `limit` is the bare `"<key>=<n>"`
    /// that tripped (e.g. `"max_steps=40"` -- no `"(this turn)"` scope
    /// suffix: unlike `ResultStatus::BudgetExceeded`'s `limit`, this event
    /// only ever fires for a turn-scoped dimension, so the scope is never
    /// ambiguous). `steps_this_turn` is `LoopState::turn_steps` at the
    /// moment of the trip, before the reset zeroes it -- the same count a
    /// companion `LogRecord::SystemNote { reason: "budget_turn_aborted",
    /// .. }` (persisted just before this is emitted) already tells the
    /// model in its own text. Board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`.
    TurnAborted {
        agent_id: AgentId,
        limit: String,
        steps_this_turn: u32,
    },
    /// Board item A5.6: `agent_id` crossed `conway_runtime::runway::
    /// BUDGET_WARN_FRACTION` (80%) of one of its own budget dimensions
    /// (`max_steps`/`max_tool_calls`/`max_tokens`/`deadline`). Emitted
    /// live, once per newly-crossed dimension, from the SAME site
    /// `LogRecord::SystemNote { reason: "runway", .. }` is already
    /// persisted -- this is the "somebody watching the live stream instead
    /// of the log" counterpart, not a second computation (`conway_runtime::
    /// runway`'s own doc: one threshold implementation, reused). Unlike
    /// `Event::TurnAborted`, this is never itself terminal and never ends
    /// anything -- `AgentLoop::check_budget` is untouched by this event's
    /// existence. `agent_id` is carried explicitly (not left to the
    /// envelope alone) so a consumer filtering by agent -- notably
    /// `conway-cli`'s `/agents` panel, which tracks a whole tree from ONE
    /// subscription and needs to mark the crossing CHILD's own row, not
    /// necessarily the row currently in view -- never has to reach into the
    /// envelope for it, mirroring `Event::TurnAborted`/`Event::
    /// StreamRestarted`'s own precedent. `limit` is the bare `"<key>=<n>"`
    /// that crossed (e.g. `"max_steps=5"`), matching `Event::TurnAborted::
    /// limit`'s own bare-key convention -- never a full sentence, which
    /// `text` (below) already carries for a renderer that wants the whole
    /// message. `text` is the exact model-facing sentence the companion
    /// `SystemNote` also carries, so a consumer needs no second formatting
    /// pass to show the operator what the model itself was told.
    BudgetWarning {
        agent_id: AgentId,
        limit: String,
        text: String,
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
    /// The FULL detail behind one `conway_runtime::permission::
    /// PermissionBroker::decide` resolution -- emitted alongside
    /// [`Event::PermissionResolved`] above (never in place of it:
    /// `PermissionResolved`'s own shape is untouched, so no existing
    /// consumer that pattern-matches it needs to change), for EVERY call
    /// the broker resolves, not just the ones that reach the operator's
    /// gate. `source` says HOW this occurrence was resolved
    /// (`operator` reached the gate just now; `rule`/`hook`/`mode` did not)
    /// -- see [`crate::log::PermissionDecisionSource`]'s own doc.
    ///
    /// The live counterpart of [`crate::log::LogRecord::
    /// PermissionDecisionRecord`] (the durable, persisted twin
    /// `PermissionBroker::decide` also writes, when a `SessionStore` is
    /// attached) -- this is what lets `--output-format jsonl` (a generic,
    /// unconditional envelope passthrough; `conway-cli`'s `JsonlRenderer`)
    /// show a denied call's reason, since `PermissionResolved` alone never
    /// carried one.
    PermissionDecision {
        call_id: String,
        tool: ToolName,
        decision: crate::log::PermissionDecisionRecordKind,
        source: crate::log::PermissionDecisionSource,
        waited_ms: Option<u64>,
        feedback: Option<String>,
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
                Event::UserTurn {
                    text: "hi".into(),
                    prov: Provenance::UserPrompt,
                },
                "user_turn",
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
            (Event::AgentPromoted {}, "agent_promoted"),
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
                Event::StreamRestarted {
                    agent_id: AgentId::new(),
                    attempt: 2,
                    discarded_text_chars: 11,
                    discarded_thinking_chars: 0,
                },
                "stream_restarted",
            ),
            (
                Event::TurnFinished {
                    usage: Usage::default(),
                    stop: StopReason::EndTurn,
                },
                "turn_finished",
            ),
            (
                Event::TurnAborted {
                    agent_id: AgentId::new(),
                    limit: "max_steps=40".into(),
                    steps_this_turn: 40,
                },
                "turn_aborted",
            ),
            (
                Event::BudgetWarning {
                    agent_id: AgentId::new(),
                    limit: "max_steps=5".into(),
                    text: "runway: 4 of 5 max_steps used this session (max_steps=5). Wrap up \
                           or report now."
                        .into(),
                },
                "budget_warning",
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
                Event::PermissionDecision {
                    call_id: "tc_1".into(),
                    tool: ToolName::new("bash"),
                    decision: crate::log::PermissionDecisionRecordKind::DenyWithFeedback,
                    source: crate::log::PermissionDecisionSource::Operator,
                    waited_ms: Some(4200),
                    feedback: Some("too risky right now".into()),
                },
                "permission_decision",
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
                    kind: MessageKind::Result,
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
        // Every variant currently defined on `Event`: the twenty from
        // architecture §6.5 (`Envelope`'s inline `event` field dropped from
        // the count), plus `Lagged`, `SteerDropped`, `AgentPromoted` and
        // `UserTurn`, minus `RepeatedStep` -- retired when repeated-step
        // detection moved to `conway-plugin-stepguard`, since the core event
        // vocabulary keeps no variant the core cannot produce -- plus
        // `StreamRestarted` (board item `01M1FSJ4E2S5M9KBSBJAAPJQ48`).
        //
        // This assertion exists precisely so nobody adds or removes a variant
        // without saying so here (see this file's module doc). `TurnAborted`
        // (board item `01M1FSP1QJFCHA7H8QPYZ9GG1P`) is the 25th; `BudgetWarning`
        // (board item A5.6) is the 26th; `PermissionDecision` (board item
        // `01M1YS2ACS0TKJYKF8TBPESTTC`) is the 27th.
        assert_eq!(variants.len(), 27);
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

    /// Backward-compat: an old JSON log line for `agent_spawned` (and,
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
        assert!(value.as_object_mut().unwrap().remove("ephemeral").is_some());
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

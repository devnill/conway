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
use crate::content::{ContentBlock, StopReason, ToolResult, Usage};
use crate::ids::{AgentId, LogSeq, ModelRef, RoleAlias, SessionId};
use crate::path::SelectionKey;
use crate::ports::PluginConfig;
use crate::provenance::{ContextReport, Provenance};

/// Fork vs spawn: the only two subagent modes, never blurred into one.
///
/// Defined here because [`ForkOrigin`] persists it; re-exported from
/// `agent` as the canonical public location.
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

/// Which of the two `/ask`-style ephemeral-fork paths created an ephemeral
/// session (B5). This distinction is LOAD-BEARING for exactly one consumer:
/// the TUI's crash-residue sweep (`Conway::sweep_stale_modal_asks`), which
/// purges leftover ephemeral sessions created by the MODAL `/ask` path
/// (the TUI's own `/ask <prompt>` command, via `conway`'s
/// `SessionHandle::ask`) after a crash left them behind — but must NEVER
/// purge a `conway_ask` TOOL child, whose `EphemeralSessionRef` artifact in
/// the calling agent's persisted `ToolOutput` would be left dangling
/// (that artifact is the only durable provenance the tool call ever
/// had). Both paths build an identical ephemeral fork through
/// `SubagentHost::start`; this tag is the only thing that tells them apart
/// after the fact, which is why it lives on the durable header rather than
/// in any live-only structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskOrigin {
    /// The interactive TUI `/ask <prompt>` modal (`SessionHandle::ask`).
    /// Crash residue of these IS sweep-eligible: the modal that would have
    /// shown their answer died with the process, so no user will ever make
    /// the fork/pull-in/discard choice for them.
    ModalAsk,
    /// The model-invoked `conway_ask` tool (`conway-tools`' `AskTool`).
    /// NEVER sweep-eligible: the caller's `ToolOutput` artifact references
    /// the child's transcript.
    ToolAsk,
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
    /// Marks a session as a disposable, catalog-hidden scratchpad (the
    /// `/ask` fork-ask flow: fork the current agent, drive one throwaway
    /// question, discard).
    ///
    /// Set once, at fork time, in the child's own `SessionMeta`. The single
    /// sanctioned later mutation is the one-way promote (true→false) via
    /// `SessionStore::set_ephemeral` (B3 — the `/ask` modal's "keep" fate);
    /// the false→true direction is refused everywhere, so a persistent
    /// record can never silently become purge-eligible scratchpad.
    /// `#[serde(default)]` so a header written before this field existed
    /// still decodes, as `false` (visible, matching pre-existing behavior).
    #[serde(default)]
    pub ephemeral: bool,
    /// Which `/ask`-style path created this ephemeral session (B5) — see
    /// [`AskOrigin`]'s own doc for why this distinction is load-bearing (the
    /// TUI's crash-residue sweep purges `ModalAsk` residue but must never touch
    /// a `ToolAsk` child). `None` for every non-`/ask` session (roots, plain
    /// forks/spawns) and — via `#[serde(default)]`, so already-persisted
    /// headers stay readable — for every header written before this field
    /// existed, which the sweep correctly treats as "not modal-ask residue"
    /// (never purged). Set at creation only, from `SubagentSpec::ask_origin` in
    /// `conway-runtime`'s `SubagentHost::start`; like `ephemeral` itself there
    /// is no sanctioned later mutation.
    #[serde(default)]
    pub ask_origin: Option<AskOrigin>,
    /// (S3) This session's confinement root, canonicalized once by
    /// `conway_runtime`'s `SubagentHost::start` and persisted here verbatim
    /// so a resumed session comes back with the SAME boundary it was
    /// spawned under -- persisting only in `AgentLoop` (an in-memory-only
    /// value) would make a resumed session silently unconfined, the exact
    /// fail-open this field exists to prevent. `None` for a root agent
    /// (`Runtime::start_root` never sets this -- out of this item's scope)
    /// and for any fork/spawn child whose effective root resolved to
    /// unconfined (inherited `None` all the way from an unconfined
    /// ancestor).
    ///
    /// Represented as a plain `PathBuf` on the wire (matching `cwd` above):
    /// `conway_core::containment::CanonicalRoot` is deliberately an
    /// in-memory-only type (its constructor performs I/O, which this
    /// crate's own no-I/O contract forbids reconstructing implicitly on
    /// every deserialize) -- callers that need containment checks against a
    /// persisted `root` reconstruct a `CanonicalRoot` from it explicitly
    /// (`CanonicalRoot::new`), the same one-canonicalization-per-use
    /// discipline `SubagentHost::start` itself follows. The `PathBuf` here
    /// is always already the *canonical* form at the moment it was written
    /// (never a raw, unresolved caller-supplied path) -- `SubagentHost::
    /// start` canonicalizes before persisting, precisely so a later
    /// reconstruction cannot silently drift from what was actually
    /// enforced/intended at spawn time.
    ///
    /// This field is **not itself enforcement** -- nothing yet checks a tool
    /// call against it (a later slice adds that); it is carried and validated
    /// end-to-end so that slice does not have to touch this plumbing.
    /// `#[serde(default)]` keeps old data readable: a header written before
    /// this field existed still decodes, as `None` -- the pre-existing
    /// unconfined behavior for every such session.
    #[serde(default)]
    pub root: Option<PathBuf>,
    /// (S1.5 resume gap) This agent's own EFFECTIVE per-agent plugin config
    /// at the moment it was created -- the full, already-merged
    /// `conway_runtime::subagent`'s `child_plugin_config` (every key any
    /// ancestor narrowed, overlaid with this agent's own narrowing, if any),
    /// persisted so a resumed session comes back with the SAME per-agent
    /// narrowing it was spawned under -- the exact same rationale
    /// [`Self::root`]'s own doc gives for persisting a fully-resolved value
    /// rather than the caller's raw request: keeping this only in
    /// `conway_runtime`'s in-memory `AgentHandle` map (as
    /// `01KZDC0269171BZDB3HH00179B` shipped it) makes a resumed session
    /// silently revert to the unconfined global default, the exact fail-open
    /// [`Self::root`] was already written to prevent for the OTHER
    /// confinement mechanism -- this field closes the same gap for the
    /// plugin-declared one.
    ///
    /// **Wire compatibility, the question [`Self::root`] never had to
    /// answer.** `root`/`cwd` are two fixed fields; a per-agent plugin
    /// config is a MAP any installed plugin may extend with its own keys,
    /// prefixed `"{plugin_id}.{bare_key}"` (`conway_core::ports::Plugin::
    /// narrowable_keys`'s own doc). [`PluginConfig`] wraps exactly that
    /// shape in its one field, `values: serde_json::Map<String,
    /// serde_json::Value>` (so on the wire this field is
    /// `{"values": {...}}`, not a bare map -- `PluginConfig` has no
    /// `#[serde(transparent)]`) -- so no NEW schema is needed for a
    /// plugin's own key to round-trip: an OLDER `conway` reading a NEWER
    /// log simply carries forward any key it does not itself declare
    /// narrowable, inside that same `values` object (untouched JSON, never
    /// rejected, never dropped -- `serde_json::Map` has no
    /// `deny_unknown_fields` to trip); a NEWER `conway` reading an OLDER log
    /// (written before this field existed) decodes it as the empty map via
    /// `#[serde(default)]`, i.e. "no per-agent narrowing recorded" -- the
    /// same pre-existing behavior every such session already has today. Both
    /// directions are therefore forward- AND backward-compatible by
    /// construction of the type this field reuses, not by anything new this
    /// field adds.
    ///
    /// **Not itself trusted at resume time.** Unlike [`Self::root`] (a
    /// single value this crate's own containment check can re-verify
    /// against nothing but itself), whether a persisted key here is still
    /// meaningful depends on EXTERNAL, mutable state -- which plugins are
    /// currently installed and which keys they currently declare narrowable
    /// (`Plugin::narrowable_keys` can shrink or vanish entirely between the
    /// process that wrote this header and the process that resumes it).
    /// `conway_runtime::runtime::root::Runtime::resume_root` re-applies
    /// [`PluginConfig::narrow`] -- the SAME function `conway_runtime::
    /// subagent`'s `SubagentHost::start` validated this value with
    /// originally -- against the CURRENT plugin set's narrowing rules before
    /// trusting it, and refuses to resume outright (a typed
    /// `RuntimeError::InvalidSpec`, never a silent drop and never a silent
    /// unconfined fallback) when a key can no longer be validated. See that
    /// method's own doc for the full re-validation contract and the
    /// disclosed reasoning for "refuse to resume" over the other two
    /// candidate outcomes.
    ///
    /// `#[serde(default)]` keeps old data readable, mirroring [`Self::root`]
    /// exactly: a header written before this field existed decodes as the
    /// empty map -- "no per-agent plugin narrowing" -- the correct reading
    /// of pre-existing data, which never had this mechanism at all.
    #[serde(default)]
    pub plugin_config: PluginConfig,
}

/// Filter for session listing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionFilter {
    pub agent_def: Option<String>,
    pub label: Option<String>,
    pub parent: Option<SessionId>,
    pub limit: Option<usize>,
    /// Whether ephemeral sessions (`SessionMeta::ephemeral`) are included in
    /// the result. Defaults to `false` (exclude) via `#[derive(Default)]` --
    /// a catalog listing should not surface `/ask` scratchpads unless a
    /// caller opts in explicitly.
    #[serde(default)]
    pub include_ephemeral: bool,
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
    /// A CHILD's terminal `AgentResult`, recorded into the PARENT's own
    /// log -- the durable half of the notification gap: a child already
    /// delivers `AgentMessage::Result` to its parent's mailbox on finish
    /// (`AgentLoop::finish`), but before this variant existed a parent that
    /// never blocked on `AgentTree::await_result` for that specific child
    /// had no way to learn of it. `mailbox::classify` produces this via
    /// `DrainEffect::Persist` -- the same path `AgentMessage::Steer`
    /// already takes to become `LogRecord::ParentSteer` -- so the parent
    /// observes it through the ordinary next-turn re-read, with no new
    /// primitive and `AgentTree::await_result`'s blocking path untouched.
    ///
    /// `prov` is always `Provenance::ChildResult { from }` (never
    /// `Provenance::SystemNote` or anything parent-authored) so the
    /// child's output is never misattributed as the parent's own.
    #[serde(rename = "child_result")]
    ChildResultRecord {
        seq: LogSeq,
        ts: DateTime<Utc>,
        result: AgentResult,
        prov: Provenance,
    },
    #[serde(rename = "context_report")]
    ContextReportRecord {
        seq: LogSeq,
        ts: DateTime<Utc>,
        report: ContextReport,
    },
    /// Marks another record in this SAME session's log as excluded from (or
    /// re-included in) the assembled outgoing LLM payload, without deleting
    /// or mutating it. `target_seq` is a local seq in this
    /// session's own numbering -- the same units `resolve_prefix` already
    /// uses everywhere else in this module's ancestry walk.
    ///
    /// This is itself an ordinary append-only log record: masking (and
    /// un-masking, by appending a second `ContextMask` for the same
    /// `target_seq` with `excluded: false`) is reversible and carries its
    /// own seq/ts/provenance, so "who masked what, and when" is always
    /// reconstructable from the log alone -- no separate side-table. A
    /// per-record flag on the target would mutate a record already written,
    /// which the append-only log never does elsewhere; an overlay record
    /// keeps that invariant intact.
    ///
    /// **Reachable, as of board item 01KZY8QRAVVVKCRBZ6HAEGW3GG
    /// (`/checkout` and a reachable `ContextMask`), through a first-party
    /// PLUGIN, not a built-in surface.** `conway_plugin_history`'s
    /// `/conway.history.mask <seq> [unmask]` command returns
    /// `conway_core::ports::CommandOutcome::MaskRecord`, which the host
    /// resolves via `conway::Conway::mask_record` -- an ordinary
    /// `SessionStore::append` call, exactly like every other record in this
    /// enum. With the plugin uninstalled, nothing in the core or the CLI
    /// can append one; ARCHITECTURE.md §3.5's own "not currently reachable
    /// through any built-in surface" sentence stays literally true for
    /// that reason (a plugin is not a built-in), and is corrected
    /// elsewhere to say so precisely, per that item's own acceptance
    /// criteria.
    ///
    /// **The scope decision that item settled: this still affects ONLY
    /// fork-prefix resolution, never a session's own future turns.**
    /// Widening it to filter a session's OWN live assembly (`ContextBuilder`)
    /// was considered and rejected for that item -- see its own completion
    /// report for the full reasoning (in short: the per-request,
    /// append-only script-hook edit path landed in `0f32bd8` already covers
    /// "exclude a segment from THIS session's own next request," through
    /// `ContextHook`/`request_assembled`, without touching the
    /// `TranscriptResolver`/`ContextBuilder` hot path a persisted-mask
    /// widening would have to; building a second mechanism for the same
    /// effect was rejected as duplication, not attempted and abandoned).
    ContextMask {
        seq: LogSeq,
        ts: DateTime<Utc>,
        target_seq: LogSeq,
        excluded: bool,
    },
    /// The owning session named a head/selection at a point in time (DESIGN §2.5).
    /// A head points at an immutable `selection` and covers the mutable tail up
    /// to `covers_upto` (LOCAL units, same as `resolver.rs`). Absence of any
    /// `ContextPathSet` means the default path (DESIGN §6).
    ///
    /// Not yet implemented in core: the variant, its serde shape, and its
    /// `seq`/`kind_tags_are_exact` handling are in place (D1-3c), but no
    /// production code appends one yet — the call site that writes a head
    /// lands with the runtime wiring (D1-3d). The `resolve_default_path`
    /// orchestrator (`conway-runtime/src/context/path.rs`) READS this variant
    /// to find the HEAD, but reading is not constructing.
    ContextPathSet {
        seq: LogSeq,
        ts: DateTime<Utc>,
        selection: SelectionKey,
        covers_upto: LogSeq,
    },
    /// A session-scoped name→selection binding (DESIGN §2.5). Sibling record to
    /// `ContextPathSet` in the owning session's own log. Names are
    /// session-scoped: two sessions may use the same name for different
    /// selections without collision.
    ///
    /// Not yet implemented in core: the variant and its serde shape are in
    /// place (D1-3c), but no production code appends one yet — the call site
    /// that writes a name→selection binding lands with the path-naming
    /// surface (D1-3d or later).
    ///
    /// **Reconciliation note — why this carries `seq`/`ts` when the §2.5
    /// sketch wrote only `{ name, selection }`.** The sketch was informal, not
    /// a constraint. Every non-Header `LogRecord` variant carries seq/ts; a
    /// naming is a timeline event (it happens at a point in time in a
    /// session); and last-write-wins-by-name resolution needs `seq` to
    /// determine which binding is "latest" when a name is rebound. So the
    /// binding gets seq/ts like every other timeline event.
    ContextPathNamed {
        seq: LogSeq,
        ts: DateTime<Utc>,
        name: String,
        selection: SelectionKey,
    },
}

impl LogRecord {
    /// The record's sequence number. Headers have none.
    pub fn seq(&self) -> Option<LogSeq> {
        match self {
            LogRecord::Header(_) => None,
            LogRecord::UserTurn { seq, .. }
            | LogRecord::Assistant { seq, .. }
            | LogRecord::ToolResultRecord { seq, .. }
            | LogRecord::ForkDirective { seq, .. }
            | LogRecord::ParentSteer { seq, .. }
            | LogRecord::SystemNote { seq, .. }
            | LogRecord::AgentResultRecord { seq, .. }
            | LogRecord::ChildResultRecord { seq, .. }
            | LogRecord::ContextReportRecord { seq, .. }
            | LogRecord::ContextMask { seq, .. }
            | LogRecord::ContextPathSet { seq, .. }
            | LogRecord::ContextPathNamed { seq, .. } => Some(*seq),
        }
    }

    /// The serialized `kind` tag for this record.
    pub fn kind_str(&self) -> &'static str {
        match self {
            LogRecord::Header(_) => "header",
            LogRecord::UserTurn { .. } => "user_turn",
            LogRecord::Assistant { .. } => "assistant",
            LogRecord::ToolResultRecord { .. } => "tool_result",
            LogRecord::ForkDirective { .. } => "fork_directive",
            LogRecord::ParentSteer { .. } => "parent_steer",
            LogRecord::SystemNote { .. } => "system_note",
            LogRecord::AgentResultRecord { .. } => "agent_result",
            LogRecord::ChildResultRecord { .. } => "child_result",
            LogRecord::ContextReportRecord { .. } => "context_report",
            LogRecord::ContextMask { .. } => "context_mask",
            LogRecord::ContextPathSet { .. } => "context_path_set",
            LogRecord::ContextPathNamed { .. } => "context_path_named",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::tool_result;
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
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
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
                LogRecord::ChildResultRecord {
                    seq: LogSeq(9),
                    ts: ts(),
                    result: crate::agent::AgentResult::new(
                        AgentId::new(),
                        SessionId::new(),
                        crate::agent::ResultStatus::Completed,
                        "child done",
                    ),
                    prov: Provenance::ChildResult {
                        from: AgentId::new(),
                    },
                },
                "child_result",
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
                        dropped: vec![],
                    },
                },
                "context_report",
            ),
            (
                LogRecord::ContextMask {
                    seq: LogSeq(8),
                    ts: ts(),
                    target_seq: LogSeq(2),
                    excluded: true,
                },
                "context_mask",
            ),
            (
                LogRecord::ContextPathSet {
                    seq: LogSeq(11),
                    ts: ts(),
                    selection: crate::path::SelectionKey::from_nodes(&[]),
                    covers_upto: LogSeq(9),
                },
                "context_path_set",
            ),
            (
                LogRecord::ContextPathNamed {
                    seq: LogSeq(12),
                    ts: ts(),
                    name: "stable".into(),
                    selection: crate::path::SelectionKey::from_nodes(&[]),
                },
                "context_path_named",
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
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
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
        assert!(
            !f.include_ephemeral,
            "SessionFilter must default to excluding ephemeral sessions"
        );
    }

    /// `SessionMeta::ephemeral` round-trips through the header line -- the
    /// property the `/ask` fork-ask flow depends on for catalog hiding to
    /// survive a store round-trip, not just an in-memory `Conway::sessions`
    /// call.
    #[test]
    fn session_meta_ephemeral_round_trips() {
        let meta = SessionMeta {
            id: SessionId::new(),
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: ts(),
            cwd: PathBuf::from("/tmp/ask"),
            labels: vec![],
            ephemeral: true,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig::default(),
        };
        let value = serde_json::to_value(LogRecord::Header(meta.clone())).unwrap();
        assert_eq!(value["ephemeral"], true);
        let back: LogRecord = serde_json::from_value(value).unwrap();
        match back {
            LogRecord::Header(decoded) => assert_eq!(decoded, meta),
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// A header line written before this field existed (no `ephemeral` key
    /// at all) must still decode -- as `false`, matching pre-existing
    /// (always-visible) behavior -- not fail.
    #[test]
    fn session_meta_ephemeral_defaults_false_when_absent_from_wire() {
        let sid = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":null,"agent_def":null,"role":null,"cwd":"/tmp/p","status":"active"}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        match record {
            LogRecord::Header(meta) => assert!(!meta.ephemeral),
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// `SessionMeta::ask_origin` round-trips through the header line (B5) --
    /// the sweep reads it back from a PERSISTED header, not from memory, so
    /// the tag must survive the store round-trip to be of any use at all.
    #[test]
    fn session_meta_ask_origin_round_trips() {
        for origin in [AskOrigin::ModalAsk, AskOrigin::ToolAsk] {
            let meta = SessionMeta {
                id: SessionId::new(),
                agent_id: AgentId::new(),
                origin: None,
                agent_def: None,
                role: None,
                created: ts(),
                cwd: PathBuf::from("/tmp/ask"),
                labels: vec![],
                ephemeral: true,
                ask_origin: Some(origin),
                root: None,
                plugin_config: PluginConfig::default(),
            };
            let value = serde_json::to_value(LogRecord::Header(meta.clone())).unwrap();
            let back: LogRecord = serde_json::from_value(value).unwrap();
            match back {
                LogRecord::Header(decoded) => assert_eq!(decoded, meta),
                other => panic!("expected Header, got {other:?}"),
            }
        }
    }

    /// A header written before `ask_origin` existed still reads (the same
    /// pre-field wire shape the ephemeral default test above uses) must
    /// decode with `ask_origin: None` -- which the crash-residue sweep
    /// correctly reads as "not modal-ask residue" (never purged).
    #[test]
    fn session_meta_ask_origin_defaults_none_when_absent_from_wire() {
        let sid = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":null,"agent_def":null,"role":null,"cwd":"/tmp/p","status":"active","ephemeral":true}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        match record {
            LogRecord::Header(meta) => {
                assert_eq!(meta.ask_origin, None);
                assert!(meta.ephemeral);
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// (S3) `SessionMeta::root` round-trips through the header line -- the
    /// property a resumed session's confinement depends on: persisting the
    /// resolved root only in memory (never on the header) would make a
    /// resumed session come back unconfined, mirrors `session_meta_
    /// ephemeral_round_trips`.
    #[test]
    fn session_meta_root_round_trips() {
        let meta = SessionMeta {
            id: SessionId::new(),
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: ts(),
            cwd: PathBuf::from("/tmp/scoped"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: Some(PathBuf::from("/tmp/scoped")),
            plugin_config: PluginConfig::default(),
        };
        let value = serde_json::to_value(LogRecord::Header(meta.clone())).unwrap();
        assert_eq!(value["root"], "/tmp/scoped");
        let back: LogRecord = serde_json::from_value(value).unwrap();
        match back {
            LogRecord::Header(decoded) => assert_eq!(decoded, meta),
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// (S3) A header written before `root` existed still reads (no `root` key at
    /// all) must still decode -- as `None`, matching pre-existing (unconfined)
    /// behavior -- not fail. Mirrors `session_meta_ephemeral_defaults_false_
    /// when_absent_from_wire`.
    #[test]
    fn session_meta_root_defaults_none_when_absent_from_wire() {
        let sid = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":null,"agent_def":null,"role":null,"cwd":"/tmp/p","status":"active"}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        match record {
            LogRecord::Header(meta) => assert_eq!(meta.root, None),
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// (S1.5 resume gap) `SessionMeta::plugin_config` round-trips through the
    /// header line -- the property a resumed session's per-agent narrowing
    /// depends on, mirrors `session_meta_root_round_trips` exactly.
    #[test]
    fn session_meta_plugin_config_round_trips() {
        let mut values = serde_json::Map::new();
        values.insert(
            "conway.fs.root".to_string(),
            serde_json::json!("/tmp/scoped"),
        );
        let meta = SessionMeta {
            id: SessionId::new(),
            agent_id: AgentId::new(),
            origin: None,
            agent_def: None,
            role: None,
            created: ts(),
            cwd: PathBuf::from("/tmp/scoped"),
            labels: vec![],
            ephemeral: false,
            ask_origin: None,
            root: None,
            plugin_config: PluginConfig {
                values: values.clone(),
            },
        };
        let value = serde_json::to_value(LogRecord::Header(meta.clone())).unwrap();
        // `PluginConfig` has no `#[serde(transparent)]` -- it round-trips as
        // a one-field struct, `{"values": {...}}`, not a bare map. Asserted
        // explicitly here since it is easy to assume otherwise (the type IS
        // conceptually an open map -- see `SessionMeta::plugin_config`'s own
        // doc -- but that openness is about `values`' own KEYS being
        // unconstrained, not about `PluginConfig` itself lacking a wrapper).
        assert_eq!(
            value["plugin_config"]["values"]["conway.fs.root"],
            "/tmp/scoped"
        );
        let back: LogRecord = serde_json::from_value(value).unwrap();
        match back {
            LogRecord::Header(decoded) => assert_eq!(decoded, meta),
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// (S1.5 resume gap) A header written before `plugin_config` existed
    /// still reads (no `plugin_config` key at all) -- decodes as the empty
    /// map, "no per-agent narrowing recorded" -- the same pre-existing
    /// behavior every such session already has today. Mirrors
    /// `session_meta_root_defaults_none_when_absent_from_wire`.
    #[test]
    fn session_meta_plugin_config_defaults_empty_when_absent_from_wire() {
        let sid = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":null,"agent_def":null,"role":null,"cwd":"/tmp/p","status":"active"}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        match record {
            LogRecord::Header(meta) => {
                assert!(meta.plugin_config.values.is_empty())
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// A NEWER `conway` reading an OLDER log with an unfamiliar plugin key
    /// in `plugin_config` (written by a plugin this reader does not itself
    /// have installed) must carry it through unmodified rather than reject
    /// the line -- see [`SessionMeta::plugin_config`]'s own doc for why this
    /// holds by construction of the type ([`PluginConfig`] is an open
    /// `serde_json::Map`, not a fixed-field struct).
    #[test]
    fn session_meta_plugin_config_carries_forward_an_unfamiliar_key_unmodified() {
        let sid = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":null,"agent_def":null,"role":null,"cwd":"/tmp/p","status":"active","plugin_config":{{"values":{{"acme.unknown.key":{{"nested":[1,2,3]}}}}}}}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        match record {
            LogRecord::Header(meta) => {
                assert_eq!(
                    meta.plugin_config.values.get("acme.unknown.key"),
                    Some(&serde_json::json!({"nested": [1, 2, 3]}))
                );
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// Back-compat in reverse of the usual direction: `SessionMeta` used to carry
    /// a `status` field (`SessionStatus::Active`/`Completed`/`Failed`/
    /// `Cancelled`), removed because only `Active` was ever written anywhere
    /// in the workspace and nothing ever read it back meaningfully. A header
    /// line written by that old code still has a `"status":"active"` key on
    /// the wire (every other test literal above still carries it
    /// unmodified, on purpose); today's status-less `SessionMeta` must still
    /// decode such a line -- serde's default "ignore unknown fields"
    /// behavior, not a special case -- rather than fail on an old session
    /// file.
    #[test]
    fn session_meta_legacy_header_with_status_key_still_deserializes() {
        let sid = SessionId::new();
        let agent = AgentId::new();
        let header = format!(
            r#"{{"kind":"header","session":"{sid}","agent":"{agent}","created":"2026-07-20T00:00:00Z","origin":null,"agent_def":"reviewer","role":"coder","cwd":"/tmp/p","status":"active","labels":["x"]}}"#
        );
        let record: LogRecord = serde_json::from_str(&header).unwrap();
        match record {
            LogRecord::Header(meta) => {
                assert_eq!(meta.id, sid);
                assert_eq!(meta.agent_id, agent);
                assert_eq!(meta.agent_def.as_deref(), Some("reviewer"));
                assert_eq!(meta.labels, vec!["x".to_string()]);
                assert!(!meta.ephemeral);
                assert_eq!(meta.ask_origin, None);
                assert_eq!(meta.root, None);
            }
            other => panic!("expected Header, got {other:?}"),
        }
    }

    /// A mask and its reversal are two distinct, independently-valid
    /// `ContextMask` records targeting the same `target_seq` -- masking
    /// never mutates the masked record or an earlier mask record in place
    /// (the caller's "no silent loss" guiding principle: exclusion is explicit
    /// and reversible by appending the opposite, not by editing history).
    #[test]
    fn context_mask_and_its_reversal_round_trip_as_independent_records() {
        let mask = LogRecord::ContextMask {
            seq: LogSeq(10),
            ts: ts(),
            target_seq: LogSeq(3),
            excluded: true,
        };
        let unmask = LogRecord::ContextMask {
            seq: LogSeq(11),
            ts: ts(),
            target_seq: LogSeq(3),
            excluded: false,
        };
        assert_ne!(mask, unmask);
        assert_eq!(mask.seq(), Some(LogSeq(10)));
        assert_eq!(unmask.seq(), Some(LogSeq(11)));

        for record in [&mask, &unmask] {
            let value = serde_json::to_value(record).unwrap();
            assert_eq!(value["kind"], "context_mask");
            assert_eq!(value["target_seq"], 3);
            let back: LogRecord = serde_json::from_value(value).unwrap();
            assert_eq!(&back, record);
        }
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
